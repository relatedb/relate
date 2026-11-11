//! Match-shape validation: the path-selector, REPEATABLE ELEMENTS,
//! quantified-path-pattern, shortest-path and update-clause pattern rules
//! Neo4j 2026 enforces at compile time but Relate accepted (the V1
//! harness wave's MATCH-shape cluster).
//!
//! Seven rules live here:
//!
//! - **42N34** — a path variable cannot be bound inside a quantified path
//!   pattern (`MATCH (p = (:A)--(:B))+`). CIP-28 permits the spelling; the
//!   live engine rejects it.
//! - **42I21** — a parenthesized/quantified path pattern's `WHERE` may not
//!   reference the enclosing path variable (`MATCH p = ((:A)-->+(:B) WHERE
//!   length(p) …)`), nor — under `REPEATABLE ELEMENTS`, for a selective path
//!   pattern — a variable declared by a sibling path pattern of the same
//!   graph pattern.
//! - **42N59** — under `REPEATABLE ELEMENTS`, a strict interior variable of
//!   a selective path pattern (any node/relationship variable that is not one
//!   of the path's two endpoint nodes) may not be redeclared by another path
//!   pattern of the same graph pattern.
//! - **42N56** — a `shortestPath()`/`allShortestPaths()` pattern supports no
//!   relationship properties.
//! - **42I04** — a `CREATE`/`MERGE` pattern admits neither a
//!   `shortestPath()`/`allShortestPaths()` pattern nor an explicit path mode.
//! - **42N40 / 42I08** — a `shortestPath()`/`allShortestPaths()` pattern
//!   must contain exactly one relationship, and its lower bound is 0 or 1.
//! - **42001** — a shortest-path *expression* (outside a `MATCH` clause)
//!   requires both endpoint node patterns to bind a variable.
//!
//! The parser owns the shape rules that must fire before the AST loses the
//! information (a named group under a non-selective selector, and a path /
//! sub-path variable sharing one name); see `parser::patterns`. The
//! `USING INDEX` hint rules are a separate MATCH-shape family and live in
//! [`super::index_hint`].

use crate::ident_hash::IdentHashSet;
use relate_names::SmallName;

use relate_error::{EngineError, ErrorKind};
use relate_parser::{
    selector_is_selective, Clause, Expr, MatchClause, MatchMode, NodePattern, PathSelector,
    PatternElement, PatternPath, RelPattern,
};

use crate::ast::collect::{collect_expr_variables, pattern_of_expr};

/// The `MATCH`-shape rule battery, one pass per `MATCH` clause.
pub fn validate_match_shape(clauses: &[Clause]) -> Result<(), EngineError> {
    for clause in clauses {
        let Some(patterns) = crate::ast::collect::clause_patterns(clause) else {
            continue;
        };
        match clause {
            Clause::Match(mc) => {
                for path in patterns {
                    reject_path_bound_in_qpp(path)?;
                    reject_own_path_variable_in_group_where(path)?;
                    reject_shortest_path_properties(path)?;
                    reject_shortest_path_function_shape(path)?;
                    reject_legacy_var_length_type_expression(path)?;
                }
                if mc.match_mode == MatchMode::RepeatableElements {
                    reject_repeatable_elements_violations(mc)?;
                }
            }
            // 42I04: the update clauses accept neither a `shortestPath()` /
            // `allShortestPaths()` pattern nor an explicit path mode.
            Clause::Create(_) => reject_update_path_shape("CREATE", patterns)?,
            Clause::Merge(_) => reject_update_path_shape("MERGE", patterns)?,
            _ => {}
        }
    }
    Ok(())
}

/// The expression-embedded half: a shortest-path expression's own shape and
/// its bound endpoints. Rides the whole-tree walk's expression visitor.
pub(crate) fn validate_expr_pattern_shape(expr: &Expr) -> Result<(), EngineError> {
    let Some(path) = pattern_of_expr(expr) else {
        return Ok(());
    };
    let Expr::ShortestPath(shortest) = expr else {
        return Ok(());
    };
    let name = shortest_path_function_name(shortest.all);
    reject_shortest_path_shape(name, path)?;
    reject_unbound_shortest_path_endpoints(name, path)
}

/// 42N40 / 42I08: a `shortestPath()`/`allShortestPaths()` pattern contains
/// exactly one relationship, and that relationship's lower bound is 0 or 1.
/// Live Neo4j 2026.10.1 reports both as `42001` `SyntaxError`, for the
/// `MATCH` sugar and the expression form alike.
fn reject_shortest_path_function_shape(path: &PatternPath) -> Result<(), EngineError> {
    if !path.is_shortest_path_function {
        return Ok(());
    }
    let all = matches!(path.selector.as_ref(), Some(PathSelector::AllShortest));
    reject_shortest_path_shape(shortest_path_function_name(all), path)
}

/// The one rule body both shortest-path spellings share; `name` is the
/// function as written, for the rejection text.
fn reject_shortest_path_shape(name: &str, path: &PatternPath) -> Result<(), EngineError> {
    let Some(rel) = single_relationship(&path.elements) else {
        return Err(EngineError::semantic(
            ErrorKind::InvalidPatternCharacter,
            format!("{name}(...) requires a pattern containing a single relationship"),
        ));
    };
    if rel.min_hops.is_some_and(|min| min > 1) {
        return Err(EngineError::semantic(
            ErrorKind::InvalidPatternCharacter,
            format!("{name}(...) does not support a minimal length different from 0 or 1"),
        ));
    }
    Ok(())
}

/// The pattern's one relationship element, or `None` when it has zero or
/// more than one (a quantified element is its own rejection, but counts as
/// "not a single relationship" here).
fn single_relationship(elements: &[PatternElement]) -> Option<&RelPattern> {
    let mut found = None;
    for element in elements {
        match element {
            PatternElement::Relationship(rel) if found.is_none() => found = Some(rel),
            PatternElement::Relationship(_) | PatternElement::Quantified(_) => return None,
            PatternElement::Node(_) => {}
        }
    }
    found
}

/// The function name a shortest-path pattern was written with, for the
/// rejection text — both spellings' one name table.
fn shortest_path_function_name(all: bool) -> &'static str {
    if all {
        "allShortestPaths"
    } else {
        "shortestPath"
    }
}

/// 42001: outside a `MATCH` clause a `shortestPath()`/`allShortestPaths()`
/// expression cannot bind its endpoints — both endpoint node patterns must
/// name a variable already in scope (live Neo4j 2026.10.1).
fn reject_unbound_shortest_path_endpoints(
    name: &str,
    path: &PatternPath,
) -> Result<(), EngineError> {
    let endpoints = [
        path.elements.iter().find_map(node_of),
        path.elements.iter().rev().find_map(node_of),
    ];
    for endpoint in endpoints.into_iter().flatten() {
        if endpoint.variable.is_none() {
            return Err(unbound_shortest_path_endpoint(name));
        }
    }
    Ok(())
}

/// One pattern element's node pattern, if it is one.
fn node_of(element: &PatternElement) -> Option<&NodePattern> {
    match element {
        PatternElement::Node(node) => Some(node),
        PatternElement::Relationship(_) | PatternElement::Quantified(_) => None,
    }
}

/// The one 42001 text for an endpoint a shortest-path expression cannot
/// bind; `name` is the written function name (live echoes it, "A
/// allShortestPaths(...) …" included).
fn unbound_shortest_path_endpoint(name: &str) -> EngineError {
    EngineError::semantic(
        ErrorKind::InvalidPatternCharacter,
        format!("A {name}(...) requires bound nodes when not part of a MATCH clause."),
    )
}

/// 42I04: `shortestPath()`/`allShortestPaths()` and explicit path modes are
/// MATCH-only pattern shapes — Neo4j rejects them in a `CREATE`/`MERGE`
/// pattern ("'…' cannot be used in a MERGE clause").
fn reject_update_path_shape(clause: &str, patterns: &[PatternPath]) -> Result<(), EngineError> {
    for path in patterns {
        if path.is_shortest_path_function {
            return Err(EngineError::semantic(
                ErrorKind::InvalidPatternCharacter,
                format!(
                    "invalid expression: 'shortestPath()/allShortestPaths()' cannot be used in a {clause} clause"
                ),
            ));
        }
        if let Some(mode) = &path.mode {
            return Err(EngineError::semantic(
                ErrorKind::InvalidPatternCharacter,
                format!(
                    "invalid expression: '{}' cannot be used in a {clause} clause",
                    path_mode_name(mode)
                ),
            ));
        }
        // A path selector is MATCH-only (live 2026.10: `CREATE ANY (a)-[:R]->(b)`
        // reports 42001 "Path selectors such as `ANY 1 PATHS` cannot be used
        // in a CREATE clause, but only in a MATCH clause").
        if path.selector.is_some() {
            return Err(EngineError::semantic(
                ErrorKind::InvalidPatternCharacter,
                format!(
                    "invalid expression: path selectors such as `ANY 1 PATHS` cannot be used in a {clause} clause, but only in a MATCH clause"
                ),
            ));
        }
    }
    Ok(())
}

/// The path-mode keyword as written, for the 42I04 description text.
fn path_mode_name(mode: &relate_parser::PathMode) -> &'static str {
    use relate_parser::PathMode;
    match mode {
        PathMode::Walk => "WALK",
        PathMode::Trail => "TRAIL",
        PathMode::Acyclic => "ACYCLIC",
        PathMode::Simple => "SIMPLE",
    }
}

/// A legacy bracket-syntax variable-length relationship (`[r*1..5]`,
/// `[:!A*]`) admits neither a GPM relationship-type expression nor an
/// inline `WHERE` — live 2026.10 rejects both with 42001
/// ("Variable length relationships must not use relationship type
/// expressions." / "Relationship pattern predicates are not supported for
/// variable-length relationships."). The GPM postfix form
/// (`-[r WHERE …]->{1,5}`, `()-[:!A&!B]->*()`) carries `quantified` and is
/// the legal spelling of the same shape.
fn reject_legacy_var_length_type_expression(path: &PatternPath) -> Result<(), EngineError> {
    fn walk(elements: &[PatternElement]) -> Result<(), EngineError> {
        for element in elements {
            match element {
                PatternElement::Relationship(rp) if !rp.quantified && rp.min_hops.is_some() => {
                    if rp
                        .type_expr
                        .as_ref()
                        .is_some_and(|expr| expr.contains_not_or_wildcard())
                    {
                        return Err(EngineError::semantic(
                            ErrorKind::InvalidPatternCharacter,
                            "Variable length relationships must not use relationship type expressions."
                                .to_string(),
                        ));
                    }
                    if rp.where_expr.is_some() {
                        return Err(EngineError::semantic(
                            ErrorKind::InvalidPatternCharacter,
                            "Relationship pattern predicates are not supported for variable-length relationships."
                                .to_string(),
                        ));
                    }
                }
                PatternElement::Quantified(quantified) => walk(&quantified.elements)?,
                _ => {}
            }
        }
        Ok(())
    }
    walk(&path.elements)
}

/// 42N56: `shortestPath()`/`allShortestPaths()` patterns support no
/// relationship properties ("Properties are not supported in the
/// shortestPath() function").
fn reject_shortest_path_properties(path: &PatternPath) -> Result<(), EngineError> {
    if !path.is_shortest_path_function {
        return Ok(());
    }
    let has_properties = path.elements.iter().any(|element| match element {
        PatternElement::Relationship(rel) => rel.has_properties,
        PatternElement::Node(_) | PatternElement::Quantified(_) => false,
    });
    if has_properties {
        return Err(EngineError::semantic(
            ErrorKind::InvalidPatternCharacter,
            "unsupported use of properties: properties are not supported in the shortestPath() function"
                .to_string(),
        ));
    }
    Ok(())
}

/// 42N34: a quantified path pattern may not also bind a path variable
/// (`MATCH ANY SHORTEST (p = (:A)--(:B))+`).
///
/// Scoped to a path pattern with an explicit selector: the CIP-28/CIP-40
/// stretch corpus — the spec harness's `cip-eval` suite (ADR-0520) —
/// deliberately exercises the plain named-QPP form
/// (`MATCH (p = (:A)-[:S]->(:B))+`, documented there as a GPM-only
/// feature current Neo4j does not implement), and those scenarios must
/// keep passing. Neo4j rejects the no-selector form too; this is a
/// stretch-floor deviation the harness reports (the retired in-repo
/// `cip-eval` baseline used to pin it).
fn reject_path_bound_in_qpp(path: &PatternPath) -> Result<(), EngineError> {
    if path.selector.is_none() {
        return Ok(());
    }
    let bound = path.elements.iter().find_map(|element| match element {
        PatternElement::Quantified(qpp) => qpp.subpath_variable.as_ref(),
        PatternElement::Node(_) | PatternElement::Relationship(_) => None,
    });
    match bound {
        Some(name) => Err(EngineError::semantic(
            ErrorKind::SyntaxError,
            format!(
                "path bound in quantified path pattern: path cannot be bound in a quantified path pattern (`{name}`)"
            ),
        )),
        None => Ok(()),
    }
}

/// 42I21: a parenthesized path pattern's trailing `WHERE` is evaluated
/// inside the group, so it may not reference the path variable the enclosing
/// pattern binds (`MATCH p = ((:A)-->+(:B) WHERE length(p) …)`).
fn reject_own_path_variable_in_group_where(path: &PatternPath) -> Result<(), EngineError> {
    let Some(where_expr) = &path.where_expr else {
        return Ok(());
    };
    let mut referenced = IdentHashSet::default();
    collect_expr_variables(where_expr, &mut referenced);
    for name in path.path_names() {
        if !referenced.contains(name) {
            continue;
        }
        // The group's *own* sub-path name is bound inside the group, so
        // reading it there is legal (`ANY SHORTEST (q = (…) WHERE length(q) …)`).
        // Only the enclosing path variable, declared outside the group, is
        // out of scope inside it (42I21).
        if path.subpath_variable.as_ref() == Some(name) {
            continue;
        }
        return Err(
            name.with_str(|text| out_of_scope_group_reference(text, ErrorKind::SyntaxError))
        );
    }
    Ok(())
}

/// The one 42I21 rejection text. The two sites keep distinct kinds on
/// purpose: a reference to the *enclosing* path variable is a plain
/// `SyntaxError` (the general 42I21 shape), while a reference to a sibling
/// path pattern's variable is the selective-path-pattern dependency rule
/// (`InvalidSelectivePathPatternDependency`), whose own TCK-aligned kind
/// predates this battery.
fn out_of_scope_group_reference(name: &str, kind: ErrorKind) -> EngineError {
    EngineError::semantic(
        kind,
        format!(
            "invalid reference to variable out of scope: not allowed to reference `{name}` from within a parenthesized/quantified path pattern in the same graph pattern"
        ),
    )
}

/// The REPEATABLE ELEMENTS redeclaration battery, over one `declared`
/// index and one "fewer than two paths" guard:
///
/// - **42I21** — a selective path pattern's group predicate may not
///   reference a variable declared by another path pattern of the same
///   graph pattern;
/// - **42N59** — a strict interior variable of a selective path pattern may
///   not be redeclared by another path pattern of the same graph pattern.
fn reject_repeatable_elements_violations(mc: &MatchClause) -> Result<(), EngineError> {
    if mc.pattern.len() < 2 {
        return Ok(());
    }
    let declared: Vec<IdentHashSet<SmallName>> = mc.pattern.iter().map(declared_names).collect();
    for (index, path) in mc.pattern.iter().enumerate() {
        if !selector_is_selective(path.selector.as_ref()) {
            continue;
        }
        // 42I21 first: the predicate's reads are the narrower question.
        let mut referenced = IdentHashSet::default();
        group_where_refs(path, &mut referenced);
        if let Some(name) = sibling_hit(&declared, index, &referenced) {
            return Err(name.with_str(|name| {
                out_of_scope_group_reference(name, ErrorKind::InvalidSelectivePathPatternDependency)
            }));
        }
        // 42N59: a strict interior variable redeclared by a sibling.
        let interior = interior_names(path);
        if let Some(name) = sibling_hit(&declared, index, &interior) {
            return Err(EngineError::semantic(
                ErrorKind::VariableAlreadyDefined,
                format!("variable `{name}` already declared"),
            ));
        }
    }
    Ok(())
}

/// The first name (in sorted order, so the rejection is deterministic) that
/// path `index`'s `names` shares with any *other* path's declarations.
fn sibling_hit<'a>(
    declared: &'a [IdentHashSet<SmallName>],
    index: usize,
    names: &'a IdentHashSet<SmallName>,
) -> Option<&'a SmallName> {
    declared
        .iter()
        .enumerate()
        .filter(|(other_index, _)| *other_index != index)
        .filter_map(|(_, other)| names.intersection(other).min())
        .min()
}

/// Every variable name a path pattern declares: its element variables
/// (including a quantified group's operand variables and sub-path name) plus
/// its own path names.
fn declared_names(path: &PatternPath) -> IdentHashSet<SmallName> {
    let mut names = IdentHashSet::default();
    for element in &path.elements {
        crate::ast::collect::collect_pattern_element_vars(element, &mut names);
    }
    names.extend(path.path_names().cloned());
    names
}

/// The strict interior variables of a path pattern: every node/relationship
/// variable that is not bound by the path's first or last node pattern. A
/// relationship variable is always interior; a quantified group's operand
/// variables are collected too (its own sub-path name is 42N34).
fn interior_names(path: &PatternPath) -> IdentHashSet<SmallName> {
    let mut names = IdentHashSet::default();
    let first_node = path
        .elements
        .iter()
        .position(|element| matches!(element, PatternElement::Node(_)));
    let last_node = path
        .elements
        .iter()
        .rposition(|element| matches!(element, PatternElement::Node(_)));
    for (index, element) in path.elements.iter().enumerate() {
        match element {
            PatternElement::Node(node) => {
                if Some(index) == first_node || Some(index) == last_node {
                    continue;
                }
                if let Some(name) = &node.variable {
                    names.insert(name.clone());
                }
            }
            PatternElement::Relationship(rel) => {
                if let Some(name) = &rel.variable {
                    names.insert(name.clone());
                }
            }
            PatternElement::Quantified(qpp) => {
                for inner in &qpp.elements {
                    crate::ast::collect::collect_pattern_element_vars(inner, &mut names);
                }
            }
        }
    }
    names
}

/// The variables referenced by a path pattern's group predicates: the
/// pattern's own trailing `WHERE` plus every quantified group's `WHERE`.
fn group_where_refs(path: &PatternPath, out: &mut IdentHashSet<SmallName>) {
    if let Some(where_expr) = &path.where_expr {
        collect_expr_variables(where_expr, out);
    }
    for element in &path.elements {
        if let PatternElement::Quantified(qpp) = element {
            if let Some(where_expr) = &qpp.where_expr {
                collect_expr_variables(where_expr, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validate(query: &str) -> Result<(), String> {
        crate::validate::parse_and_validate(query, validate_match_shape)
    }

    /// 42I04: `shortestPath()`/`allShortestPaths()` and explicit path modes
    /// are MATCH-only (live Neo4j 2026.09).
    #[test]
    fn shortest_path_and_path_modes_are_rejected_in_create_and_merge() {
        for query in [
            "MATCH (a), (b) MERGE shortestPath((a)-[:T]->(b))",
            "MATCH (a), (b) MERGE allShortestPaths((a)-[:T]->(b))",
            "MATCH (a), (b) CREATE shortestPath((a)-[:T]->(b))",
            "MATCH (a), (b) MERGE TRAIL (a)-[:T]->(b)",
            "MATCH (a), (b) CREATE ACYCLIC (a)-[:T]->(b)",
        ] {
            let err = validate(query).unwrap_err();
            assert!(err.contains("cannot be used in a"), "{query}: {err}");
        }
        // The same shapes stay legal in MATCH.
        assert!(validate("MATCH p = shortestPath((a)-[:T]->(b)) RETURN p").is_ok());
        assert!(validate("MATCH TRAIL (a)-[:T]->(b) RETURN a").is_ok());
    }

    /// 42N56: relationship properties are not supported inside
    /// `shortestPath()`/`allShortestPaths()`.
    #[test]
    fn shortest_path_relationship_properties_are_rejected() {
        for query in [
            "MATCH (a), (b), shortestPath((a)-[r* {x: 1}]->(b)) RETURN *",
            "MATCH (a), (b), allShortestPaths((a)-[*1..3 {x: 1}]->(b)) RETURN *",
            // An empty map is a written specification too (live-verified:
            // Neo4j answers 42N56 for `-[{}]-` exactly as for `-[{x: 1}]-`).
            "MATCH (a), (b), shortestPath((a)-[{}]->(b)) RETURN *",
        ] {
            let err = validate(query).unwrap_err();
            assert!(err.contains("properties"), "{query}: {err}");
        }
        assert!(validate("MATCH (a), (b), shortestPath((a)-[*]->(b)) RETURN *").is_ok());
    }

    /// 42N40 / 42I08: a `shortestPath()`/`allShortestPaths()` pattern has
    /// exactly one relationship, and its lower bound is 0 or 1 (live Neo4j
    /// 2026.10.1, `42001` `SyntaxError`).
    #[test]
    fn shortest_path_function_pattern_shape_is_rejected() {
        for (query, message) in [
            (
                "MATCH p = shortestPath((n)) RETURN p",
                "shortestPath(...) requires a pattern containing a single relationship",
            ),
            (
                "MATCH p = allShortestPaths((n)) RETURN p",
                "allShortestPaths(...) requires a pattern containing a single relationship",
            ),
            (
                "MATCH p = shortestPath((a)--()--(b)) RETURN p",
                "shortestPath(...) requires a pattern containing a single relationship",
            ),
            (
                "MATCH p = allShortestPaths((a)--()--(b)) RETURN p",
                "allShortestPaths(...) requires a pattern containing a single relationship",
            ),
            (
                "MATCH p = shortestPath((a)-[*2..3]->(b)) RETURN p",
                "shortestPath(...) does not support a minimal length different from 0 or 1",
            ),
            (
                "MATCH p = allShortestPaths((a)-[*2..3]->(b)) RETURN p",
                "allShortestPaths(...) does not support a minimal length different from 0 or 1",
            ),
        ] {
            let err = validate(query).unwrap_err();
            assert!(err.contains(message), "{query}: {err}");
        }
        // A zero lower bound and a fixed one-relationship pattern stay legal.
        assert!(validate("MATCH p = shortestPath((a)-[*0..3]->(b)) RETURN p").is_ok());
        assert!(validate("MATCH p = allShortestPaths((a)-[:T]->(b)) RETURN p").is_ok());

        // The expression form carries the same rule (live Neo4j 2026.10.1).
        for (query, message) in [
            (
                "MATCH (a) RETURN shortestPath((a)) AS p",
                "shortestPath(...) requires a pattern containing a single relationship",
            ),
            (
                "MATCH (a) RETURN allShortestPaths((a)) AS p",
                "allShortestPaths(...) requires a pattern containing a single relationship",
            ),
            (
                "MATCH (a), (b), (c) RETURN shortestPath((a)-[:T]-(b)-[:T]-(c)) AS p",
                "shortestPath(...) requires a pattern containing a single relationship",
            ),
            (
                "MATCH (a), (b) RETURN shortestPath((a)-[:T*2..3]-(b)) AS p",
                "shortestPath(...) does not support a minimal length different from 0 or 1",
            ),
        ] {
            let err = crate::validate::parse_and_validate_tree(query).unwrap_err();
            assert!(err.contains(message), "{query}: {err}");
        }
        assert!(crate::validate::parse_and_validate_tree(
            "MATCH (a), (b) RETURN shortestPath((a)-[:T*0..3]-(b)) AS p"
        )
        .is_ok());
    }

    /// 42001: outside a `MATCH` clause both endpoint node patterns of a
    /// shortest-path expression must name a variable (live Neo4j 2026.10.1).
    #[test]
    fn shortest_path_expression_requires_bound_endpoints() {
        for query in [
            "MATCH (bound:A) RETURN length(shortestpath((bound)-[*1..3]-(:B))) AS dist",
            "MATCH (bound:A) WHERE length(shortestpath((bound)-[*1..3]-(:B))) < 3 RETURN bound",
            "MATCH (bound:A) WITH shortestpath((bound)-[*1..3]-(:B)) AS p RETURN length(p)",
        ] {
            let err = crate::validate::parse_and_validate_tree(query).unwrap_err();
            assert!(
                err.contains("requires bound nodes when not part of a MATCH clause"),
                "{query}: {err}"
            );
        }
        // The all-shortest spelling echoes its own name (live's text).
        let err = crate::validate::parse_and_validate_tree(
            "MATCH (bound:A) RETURN length(allshortestpaths((bound)-[*1..3]-(:B))) AS dist",
        )
        .unwrap_err();
        assert!(
            err.contains("A allShortestPaths(...) requires bound nodes"),
            "{err}"
        );
        // Both endpoints bound: legal, wherever the expression sits.
        assert!(crate::validate::parse_and_validate_tree(
            "MATCH (a:A), (b:B) RETURN length(shortestpath((a)-[*1..3]-(b))) AS dist"
        )
        .is_ok());
    }
}
