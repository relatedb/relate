//! The `<search clause>` rule family: the SEARCH-clause
//! binding-variable, SCORE-conflict, and scope rules of
//! [`validate_search_clauses`].

use crate::ident_hash::IdentHashSet;
use relate_names::SmallName;

use relate_error::{EngineError, ErrorKind};
use relate_parser::{Clause, PatternElement, PatternPath, SearchClause, SearchKind};

use crate::ast::collect::{
    bind_pattern_vars, clause_introduced_names, collect_expr_variables, projected_names,
    search_score_var,
};
use crate::ast::search_spec::{walk_search_spec, SearchSlot};
use crate::validate::ValidateOptions;

/// `<search clause>` rules that don't fit any existing generic
/// scope tracker:
/// - The search query's self-reference ban (the search query can't
///   reference the binding variable it's about to produce).
/// - The binding variable reference must be declared by this same
///   MATCH statement's own pattern (its NODE/RELATIONSHIP value-type
///   half is checked at runtime, see `match_clause::execute_search_for_row`).
/// - The SCORE AS variable must not conflict with anything already
///   in scope, including the binding variable reference itself.
///
/// The plain pattern-shape restriction applies to every source kind
/// (see [`validate_search_pattern_shape`]): a SEARCH MATCH statement binds
/// one variable, in a single node or single-hop relationship pattern, with
/// predicates only on that variable. The `complex-search-patterns` preview
/// gate (mirroring
/// Neo4j's own mode) is the
/// one gate that lifts it; [`ValidateOptions`] carries the store's preview
/// set, and the gate reads its own feature from it.
///
/// The SCORE-conflict scope check builds `scope` by walking the body's clauses from
/// empty, deriving each clause's introduced names from
/// [`clause_introduced_names`]: a SEARCH clause sees the names earlier
/// clauses of the same linear body declared. (A CIP-213 NEXT stage starts a
/// fresh body -- its names are bound per stage on the lowered IR, not by
/// this pass.)
pub(crate) fn validate_search_clauses(
    clauses: &[Clause],
    options: ValidateOptions,
) -> Result<(), EngineError> {
    let mut scope: IdentHashSet<SmallName> = IdentHashSet::default();

    for clause in clauses {
        match clause {
            // A searching MATCH is the one arm this fold spells itself: the
            // CIP-224 rules need the clause's own pattern variables *before*
            // they enter the scope (the binding-reference and
            // SCORE-conflict checks both test against them), while
            // [`clause_introduced_names`] folds the pattern variables and the
            // SCORE variable together.
            Clause::Match(mc) => match mc.search.as_ref() {
                Some(search) => {
                    let mut this_match_vars: IdentHashSet<SmallName> = IdentHashSet::default();
                    bind_pattern_vars(&mc.pattern, &mut this_match_vars);
                    validate_search_clause_rules(
                        &mc.pattern,
                        search,
                        &this_match_vars,
                        &scope,
                        options,
                    )?;
                    scope.extend(this_match_vars);
                    // The SCORE conflict, checked where the scope lifecycle lives: after
                    // this MATCH's own variables have entered the scope, one
                    // `insert` decides the conflict (and binds on win).
                    if let Some(sv) = search_score_var(mc) {
                        if !scope.insert(sv.clone()) {
                            return Err(EngineError::semantic(
                                ErrorKind::VariableAlreadyDefined,
                                format!("variable '{sv}' is already defined"),
                            ));
                        }
                    }
                }
                None => scope.extend(clause_introduced_names(clause)),
            },
            // A WITH replaces the scope (a projection), it does not extend
            // it -- the canonical project-items-to-names skeleton answers
            // exactly that (`*` passes the incoming scope through).
            Clause::With(wc) => {
                scope = projected_names(&wc.items, &scope);
            }
            // A `CALL { }` body's exports depend on the enclosing scope.
            Clause::CallSubquery(csq) => {
                let exports = crate::call_scope::call_subquery_exports(csq, &scope);
                scope.extend(exports);
            }
            clause => scope.extend(clause_introduced_names(clause)),
        }
    }
    Ok(())
}

/// The CIP-224 SEARCH rules proper, extracted from the MATCH arm's scope
/// bookkeeping: the binding-variable reference, and the
/// source-specification rules -- the per-index
/// CIP-225/240 kind×option compatibility (on the index) and the
/// self-reference rules (the `FOR` query and the
/// WEIGHT/CONSTANT controls, by slot) -- as one slot-tagged pass over the
/// (possibly nested, CIP-278) source tree, the first violation in walk
/// order stopping it. The SCORE-conflict check stays with the
/// caller, which owns the scope lifecycle.
fn validate_search_clause_rules(
    pattern: &[PatternPath],
    search: &SearchClause,
    this_match_vars: &IdentHashSet<SmallName>,
    scope: &IdentHashSet<SmallName>,
    options: ValidateOptions,
) -> Result<(), EngineError> {
    // Plain shape: one bound variable, one node or single-hop
    // relationship pattern, predicates only on the bound element — for a
    // FULLTEXT source exactly as for a VECTOR one (FulltextSearchAcceptance
    // [11]-[13]). The `complex-search-patterns` opt-in is the one gate that
    // lifts the shape family (the acceptance suites' multi-component,
    // multi-hop and selected-path rows); everything else this function
    // checks — the binding-variable reference and the per-index
    // kind×option/self-reference rules — holds either way.
    let shape = search_pattern_shape(pattern);
    if !options
        .preview
        .enabled(relate_core::PreviewFeature::ComplexSearchPatterns)
    {
        if let SearchPatternShape::Complex(reason) = shape {
            return Err(EngineError::semantic(
                ErrorKind::SyntaxError,
                reason.to_string(),
            ));
        }
    }
    if !this_match_vars.contains(&search.binding_var) {
        return Err(EngineError::semantic(
            ErrorKind::InvalidSearchVariableReference,
            format!(
                "'{}' must be a variable bound by this SEARCH clause's own MATCH statement",
                search.binding_var
            ),
        ));
    }

    walk_search_spec(
        &search.spec,
        // The kind×option compatibility policy, one spelling per kind and
        // one statement of its polarity: the inner WHERE knob is
        // vector-search-only (CIP-240), while `WITH ANALYZER` and
        // SKIP/OFFSET are fulltext-only (CIP-225 grammar -- accepting them
        // on a VECTOR INDEX silently deviates from the surface, round-4
        // review); CIP-225 on live Neo4j 2026.09.0 additionally rejects
        // WHERE together with FULLTEXT INDEX (42N14). All three rejections
        // raise the typed `InvalidUseOfCommand` detail name.
        &mut |index| {
            match index.kind {
                SearchKind::Fulltext => {
                    if index.inner_where.is_some() {
                        return Err(EngineError::semantic(
                            ErrorKind::InvalidUseOfCommand,
                            "WHERE cannot be used together with 'FULLTEXT INDEX'".to_string(),
                        ));
                    }
                }
                SearchKind::Vector => {
                    if index.analyzer.is_some() {
                        return Err(EngineError::semantic(
                            ErrorKind::InvalidUseOfCommand,
                            "WITH ANALYZER cannot be used together with VECTOR INDEX".to_string(),
                        ));
                    }
                    if index.offset_expr.is_some() {
                        return Err(EngineError::semantic(
                            ErrorKind::InvalidUseOfCommand,
                            "SKIP cannot be used together with VECTOR INDEX".to_string(),
                        ));
                    }
                }
            }
            Ok(())
        },
        // The self-reference rules, dispatched by slot: the search
        // query (`FOR`), the WEIGHT/CONSTANT controls and the
        // per-row `WITH ANALYZER` expression may reference any incoming
        // variable but not the search clause's own binding variable
        // (FulltextSearchAcceptance [59]/[60]). The inner WHERE is
        // deliberately excluded -- CIP-240's predicate shape is `binding
        // variable . property`, so it names the binding variable by design.
        &mut |slot, expr| {
            let message = match slot {
                SearchSlot::For => "the search query cannot reference the binding variable",
                SearchSlot::Weight | SearchSlot::Constant => {
                    "the WEIGHT or CONSTANT expression may not depend on the search clause \
                     binding variable"
                }
                SearchSlot::Analyzer => {
                    "the analyzer expression may not depend on the search clause binding variable"
                }
                // The LIMIT expression reads the input row, never a
                // pattern binding: under a complex pattern the executor
                // states one LIMIT for a whole deferred round, so a
                // pattern-binding read (a per-row LIMIT) is rejected
                // rather than silently applied to every row. InnerWhere
                // and Offset stay exempt (the inner WHERE names the
                // binding variable by design); the match is exhaustive
                // so a new slot must state its policy here before it
                // compiles.
                SearchSlot::InnerWhere | SearchSlot::Offset => return Ok(()),
                SearchSlot::Limit => {
                    // The LIMIT expression reads the input row, never a
                    // variable this MATCH's pattern newly binds: the
                    // executor states one LIMIT for a whole round (the
                    // deferred shape evaluates it per matched row), so a
                    // per-row LIMIT is rejected under every shape, not
                    // silently collapsed to the first row's. The reported
                    // name is the deterministic minimum of the reads
                    // (IdentHashSet iteration is process-random,
                    // ADR-0505 §4).
                    let mut vars = IdentHashSet::default();
                    collect_expr_variables(expr, &mut vars);
                    let newly_bound =
                        |var: &SmallName| this_match_vars.contains(var) && !scope.contains(var);
                    if let Some(var) = vars.iter().filter(|v| newly_bound(v)).min() {
                        return Err(EngineError::semantic(
                            ErrorKind::SyntaxError,
                            format!(
                                "SearchClauseLimitReadsPattern: the LIMIT expression \
                                 may not depend on the pattern variable '{var}'"
                            ),
                        ));
                    }
                    return Ok(());
                }
            };
            let mut vars = IdentHashSet::default();
            collect_expr_variables(expr, &mut vars);
            if vars.contains(&search.binding_var) {
                return Err(EngineError::semantic(
                    ErrorKind::SelfReferencingSearch,
                    format!("{message} '{}'", search.binding_var),
                ));
            }
            Ok(())
        },
    )
}

/// The typed shape of a SEARCH clause's MATCH statement — the single
/// authority every consumer keys on: the `complex-search-patterns` gate
/// maps [`SearchPatternShape::Complex`] to its per-reason rejection, and
/// the executor's top-K rule reads the classification (a plain pattern's
/// LIMIT is a hard candidate window; a complex pattern's counts the
/// candidates the whole MATCH keeps — live Neo4j 2026.10.1 splits the
/// two exactly here). Classification is never the negation of a
/// validation verdict: a new shape rule lands here once, and both the
/// gate and the top-K rule follow it.
pub(crate) enum SearchPatternShape {
    /// The plain shape.
    Plain,
    /// Everything beyond it — the `complex-search-patterns` family, with
    /// the gate's rejection detail.
    Complex(&'static str),
}

/// Classify a SEARCH clause's MATCH statement: [`SearchPatternShape`].
pub(crate) fn search_pattern_shape(pattern: &[PatternPath]) -> SearchPatternShape {
    if pattern.len() != 1 {
        return SearchPatternShape::Complex(
            "SearchClauseTooComplexPattern: a SEARCH clause's MATCH statement must be a single path pattern",
        );
    }
    let path = &pattern[0];
    if path.selector.is_some() || path.is_shortest_path_function {
        return SearchPatternShape::Complex(
            "SearchClauseTooComplexPattern: a SEARCH clause's MATCH statement cannot use a path selector",
        );
    }
    match path.elements.as_slice() {
        [PatternElement::Node(_)] => SearchPatternShape::Plain,
        [PatternElement::Node(n0), PatternElement::Relationship(rp), PatternElement::Node(n1)] => {
            if rp.is_variable_length() || rp.quantified {
                return SearchPatternShape::Complex(
                    "SearchClauseTooComplexPattern: a SEARCH clause's MATCH statement cannot use a variable-length relationship",
                );
            }
            let bound_count = [n0.variable.is_some(), rp.variable.is_some(), n1.variable.is_some()]
                .iter()
                .filter(|b| **b)
                .count();
            if bound_count > 1 {
                return SearchPatternShape::Complex(
                    "SearchClauseMultipleBoundVariables: a SEARCH clause's MATCH statement may bind only one variable",
                );
            }
            // The bound variable is the only element that may carry
            // predicates -- one spelling of the rejection,
            // checked over every element position.
            let unbound_with_predicates = [
                (n0.variable.is_none(), has_node_pattern_predicates(n0)),
                (n1.variable.is_none(), has_node_pattern_predicates(n1)),
                (rp.variable.is_none(), has_rel_pattern_predicates(rp)),
            ]
            .into_iter()
            .any(|(unbound, has_predicates)| unbound && has_predicates);
            if unbound_with_predicates {
                return SearchPatternShape::Complex(
                    "SearchClauseInvalidPredicates: a SEARCH clause's MATCH statement cannot have predicates on elements other than the bound variable",
                );
            }
            SearchPatternShape::Plain
        }
        _ => SearchPatternShape::Complex(
            "SearchClauseTooComplexPattern: a SEARCH clause's MATCH statement must be a single node or single-hop relationship pattern",
        ),
    }
}

/// Whether a SEARCH clause's MATCH statement goes beyond the
/// plain shape -- the `complex-search-patterns` family ([`SearchPatternShape`]).
pub(crate) fn search_pattern_is_complex(pattern: &[PatternPath]) -> bool {
    matches!(
        search_pattern_shape(pattern),
        SearchPatternShape::Complex(_)
    )
}

fn has_node_pattern_predicates(np: &relate_parser::NodePattern) -> bool {
    np.label_expr.is_some()
        || np.has_properties
        || np.properties_param.is_some()
        || np.where_expr.is_some()
}

fn has_rel_pattern_predicates(rp: &relate_parser::RelPattern) -> bool {
    rp.type_expr.is_some() || rp.has_properties || rp.where_expr.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The plain shape holds for a FULLTEXT source exactly as for a VECTOR
    /// one: multi-component, variable-length and QPP patterns are rejected
    /// (FulltextSearchAcceptance [11]-[13]).
    #[test]
    fn complex_search_match_patterns_are_rejected() {
        for (query, detail) in [
            (
                "MATCH (seed:Person {name: 'Alice'}), (p:Person) \
                 SEARCH p IN (FULLTEXT INDEX ft FOR 'brave' LIMIT 5) RETURN p",
                "SearchClauseTooComplexPattern",
            ),
            (
                "MATCH (p:Person)-[:KNOWS*1..2]->(target:Person) \
                 SEARCH p IN (FULLTEXT INDEX ft FOR 'brave' LIMIT 5) RETURN p",
                "SearchClauseTooComplexPattern",
            ),
            (
                "MATCH (m:Movie)-[r]->() \
                 SEARCH m IN (FULLTEXT INDEX ft FOR 'brave' LIMIT 5) RETURN m",
                "SearchClauseMultipleBoundVariables",
            ),
            (
                "MATCH (m:Movie)-[]->(:Book) \
                 SEARCH m IN (FULLTEXT INDEX ft FOR 'brave' LIMIT 5) RETURN m",
                "SearchClauseInvalidPredicates",
            ),
        ] {
            let error =
                crate::validate::validate_statement_tree(&relate_parser::parse(query).unwrap())
                    .unwrap_err();
            assert!(
                error.to_string().contains(detail),
                "{query}: expected {detail}, got {error}"
            );
        }
    }

    /// An *empty* property map is a written specification, not an absent
    /// one: `-[] {}]-` counts exactly like `(a {})`
    /// (live-verified — Neo4j rejects both with 42I71).
    #[test]
    fn a_limit_reading_a_pattern_variable_is_rejected() {
        // Live Neo4j 2026.10.1 rejects the same reads with 42N28 ("only
        // statically inferrable patterns and variables are allowed in
        // 'LIMIT'"): the executor states one LIMIT per round, so a
        // per-row LIMIT is refused at validation under every shape.
        for query in [
            "MATCH (m:Movie) \
             SEARCH m IN (FULLTEXT INDEX ft FOR 'x' LIMIT toInteger(m.keep)) \
             RETURN m",
            "MATCH (m:Movie)-[]->() \
             SEARCH m IN (FULLTEXT INDEX ft FOR 'x' LIMIT toInteger(m.keep)) \
             RETURN m",
        ] {
            let error =
                crate::validate::validate_statement_tree(&relate_parser::parse(query).unwrap())
                    .unwrap_err();
            assert!(
                error.to_string().contains("SearchClauseLimitReadsPattern"),
                "{query}: {error}"
            );
        }
    }

    #[test]
    fn an_empty_property_map_counts_as_a_predicate() {
        for query in [
            "MATCH (a:Person)-[{}]->() \
             SEARCH a IN (FULLTEXT INDEX ft FOR 'x' LIMIT 5) RETURN a",
            "MATCH (a:Person)-[]->({}) \
             SEARCH a IN (FULLTEXT INDEX ft FOR 'x' LIMIT 5) RETURN a",
        ] {
            let error =
                crate::validate::validate_statement_tree(&relate_parser::parse(query).unwrap())
                    .unwrap_err();
            assert!(
                error.to_string().contains("SearchClauseInvalidPredicates"),
                "{query}: {error}"
            );
        }
    }

    #[test]
    fn the_complex_pattern_classifier_matches_the_shape_check() {
        // The executor's top-K rule keys on this classifier: a plain shape
        // keeps the LIMIT as a hard candidate window (its outer WHERE
        // post-filters), every shape the gate rejects counts the candidates
        // the whole MATCH keeps (live Neo4j 2026.10.1 splits the two exactly
        // here).
        for (query, complex) in [
            (
                "MATCH (p:Person) SEARCH p IN (FULLTEXT INDEX ft FOR 'x' LIMIT 5) RETURN p",
                false,
            ),
            (
                "MATCH ()-[r:KNOWS]->() SEARCH r IN (FULLTEXT INDEX ft FOR 'x' LIMIT 5) \
                 RETURN r",
                false,
            ),
            (
                "MATCH (m:Movie)-[r]->() SEARCH m IN (FULLTEXT INDEX ft FOR 'x' LIMIT 5) \
                 RETURN m",
                true,
            ),
            (
                "MATCH (m:Movie)-[]->(:Book) SEARCH m IN (FULLTEXT INDEX ft FOR 'x' LIMIT 5) \
                 RETURN m",
                true,
            ),
            (
                "MATCH (seed:Person {name: 'Alice'}), (p:Person) \
                 SEARCH p IN (FULLTEXT INDEX ft FOR 'x' LIMIT 5) RETURN p",
                true,
            ),
            (
                "MATCH (p:Person)-[:KNOWS*1..2]->(target:Person) \
                 SEARCH p IN (FULLTEXT INDEX ft FOR 'x' LIMIT 5) RETURN p",
                true,
            ),
        ] {
            let statement = relate_parser::parse(query).unwrap();
            let pattern = statement
                .clauses
                .iter()
                .find_map(|clause| match clause {
                    relate_parser::Clause::Match(mc) if mc.search.is_some() => Some(&mc.pattern),
                    _ => None,
                })
                .expect("a searching MATCH");
            assert_eq!(
                search_pattern_is_complex(pattern),
                complex,
                "{query}: the classifier must agree with the shape check"
            );
        }
    }

    /// The CIP-224 rules that raise typed [`ErrorKind`]s render with their
    /// own detail name (the payload unchanged after it) -- pinned here
    /// because the rendered text is the error contract, raised by the
    /// validate battery before lowering. The remaining SEARCH rejections
    /// (the pattern-shape family) keep the `SyntaxError:` outer kind with
    /// their detail in the message.
    #[test]
    fn search_rules_raise_their_typed_kinds() {
        for (query, kind, text) in [
            (
                "MATCH (n) SEARCH m IN (FULLTEXT INDEX ft FOR 'x' LIMIT 1) RETURN n",
                ErrorKind::InvalidSearchVariableReference,
                "InvalidSearchVariableReference: 'm' must be a variable bound by this \
                 SEARCH clause's own MATCH statement",
            ),
            (
                "MATCH (n) SEARCH n IN (FULLTEXT INDEX ft FOR n.title LIMIT 1) RETURN n",
                ErrorKind::SelfReferencingSearch,
                "SelfReferencingSearch: the search query cannot reference the binding \
                 variable 'n'",
            ),
            (
                "MATCH (n) SEARCH n IN (FULLTEXT INDEX ft FOR 'x' LIMIT 1) SCORE AS n \
                 RETURN n",
                ErrorKind::VariableAlreadyDefined,
                "VariableAlreadyDefined: variable 'n' is already defined",
            ),
            (
                "MATCH (n) SEARCH n IN (FULLTEXT INDEX ft FOR 'x' WHERE n.p > 1 LIMIT 1) \
                 RETURN n",
                ErrorKind::InvalidUseOfCommand,
                "InvalidUseOfCommand: WHERE cannot be used together with 'FULLTEXT INDEX'",
            ),
            (
                "MATCH (n) SEARCH n IN (VECTOR INDEX v FOR [1.0] WITH ANALYZER 'a' LIMIT 1) \
                 RETURN n",
                ErrorKind::InvalidUseOfCommand,
                "InvalidUseOfCommand: WITH ANALYZER cannot be used together with VECTOR INDEX",
            ),
            (
                "MATCH (n) SEARCH n IN (VECTOR INDEX v FOR [1.0] SKIP 1 LIMIT 1) RETURN n",
                ErrorKind::InvalidUseOfCommand,
                "InvalidUseOfCommand: SKIP cannot be used together with VECTOR INDEX",
            ),
        ] {
            let error =
                crate::validate::validate_statement_tree(&relate_parser::parse(query).unwrap())
                    .unwrap_err();
            assert_eq!(error.kind(), kind, "wrong kind for: {query}");
            assert_eq!(error.to_string(), text, "wrong text for: {query}");
        }
    }
}
