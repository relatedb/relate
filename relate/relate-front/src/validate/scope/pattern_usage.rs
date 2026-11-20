//! The MATCH pattern-usage phase of the variable-type check: the variable
//! usages one MATCH introduces, collected before any type is committed, then
//! checked against earlier clauses and earlier usages of the same pattern
//! list. The type vocabulary ([`VarType`], [`reuse_error_kind`]) is the
//! parent's; this module owns the pattern walk and its conflict rules —
//! and, as the one home of that walk, the shared surface every other
//! consumer reads it through: [`bind_clause_var_kinds`], the per-clause
//! kind fold the constraints walk's kind-tagged scope binds with (the
//! walk without the conflict rules, which stay this module's own).

use crate::ident_hash::{IdentHashMap, IdentHashSet};
use relate_error::{EngineError, ErrorKind};
use relate_names::SmallName;
use relate_parser::{Clause, PatternElement, PatternPath};

use super::{reuse_error_kind, VarType};
use crate::ast::collect::{clause_patterns, pattern_vars};

/// The clause-level hook into the canonical kind classification: every
/// variable a pattern-bearing clause binds — through
/// [`clause_patterns`]'s one clause→pattern-list mapping — with the kind
/// its pattern position gives it (a node element's a Node, a
/// relationship element's a Relationship — var-length and
/// quantified-group alike — a path's own name a Path), folded into a
/// name set and a kind map *together*: kinds ⊆ names by construction,
/// the names through the canonical pattern-name enumeration
/// ([`pattern_vars`], which also visits a quantified group's subpath
/// variable — a name with no kind: it binds a list of the sub-paths
/// its group matched, a value the planner binds as `Binding::List`,
/// the SEARCH score variable's shape). [`validate_match_patterns`]'s
/// conflict rules stay this module's own; the fold is the same walk,
/// pure, for the passes that consume the classification per clause. A
/// clause with no pattern binds nothing.
pub(crate) fn bind_clause_var_kinds(
    clause: &Clause,
    names: &mut IdentHashSet<SmallName>,
    kinds: &mut IdentHashMap<SmallName, VarType>,
) -> Result<(), EngineError> {
    let Some(patterns) = clause_patterns(clause) else {
        return Ok(());
    };
    for name in pattern_vars(patterns) {
        names.insert(name.clone());
    }
    for usage in collect_pattern_usages(patterns)? {
        kinds.insert(usage.name, usage.var_type);
    }
    Ok(())
}

/// Validate match patterns for variable type conflicts.
///
/// Checks each pattern path in a MATCH clause. For each variable:
/// - Node positions => VarType::Node
/// - Relationship positions => VarType::Relationship
/// - Named path variables => VarType::Path
///
/// If a variable already has a different type, it's a conflict.
pub(super) fn validate_match_patterns(
    patterns: &[PatternPath],
    var_types: &mut IdentHashMap<SmallName, VarType>,
) -> Result<(), EngineError> {
    let usages = collect_pattern_usages(patterns)?;
    check_usage_conflicts(&usages, var_types)?;
    // Commit all new types.
    for usage in usages {
        var_types.insert(usage.name, usage.var_type);
    }
    Ok(())
}

/// One variable usage a MATCH pattern introduces: the name, the type its
/// pattern position gives it, and whether a variable-length relationship
/// bound it (the scalar→varlen rebinding exception
/// [`check_usage_conflicts`] honors).
struct PatternUsage {
    name: SmallName,
    var_type: VarType,
    is_varlen: bool,
}

/// Phase one: every variable usage this MATCH introduces, collected before
/// any type is committed — in binding order, a path's element variables
/// (nodes/relationships) before the path variable that names the whole
/// pattern, and a quantified path's inner elements as group variables
/// (CIP-28; the group-specific reuse rules are enforced on the lowered IR
/// by the planner's quantified-path binding, `GroupVariableReuse` in
/// `relate-planner`). A nested quantified path is the typed
/// `NestedQuantification` error here (the parser rejects it — CIP-28 Rule
/// 4 — but a hand-built AST can still reach this validator, LABS-70).
fn collect_pattern_usages(patterns: &[PatternPath]) -> Result<Vec<PatternUsage>, EngineError> {
    let mut usages = Vec::new();
    for path in patterns {
        // Order matters for the reuse-keyed error kind in
        // `check_usage_conflicts`: a path's element variables are
        // considered bound BEFORE the path variable that names the whole
        // pattern. So in `p = (p)-[]-()` the node `p` is the original
        // binding and the path variable `p` is the reuse
        // (VariableAlreadyBound), while in `p = ()-[]-(), (p)` the path
        // variable is the original and the later node `(p)` is the reuse
        // (VariableTypeConflict) — matching clauses/match/Match1 scenario
        // 10 and Match6 scenario 23. Hence the path-variable push is
        // deferred to after this path's elements.
        for elem in &path.elements {
            collect_element_usages(elem, false, &mut usages)?;
        }

        // Path variable binds after its elements (see the ordering note above).
        // A named parenthesized subpath binds the same path value, so it is a
        // path usage too.
        for name in path.path_names() {
            usages.push(PatternUsage {
                name: name.clone(),
                var_type: VarType::Path,
                is_varlen: false,
            });
        }
    }
    Ok(usages)
}

/// The usages one pattern element contributes — the
/// element-to-`(name, type, varlen)` mapping written once for the
/// top-level walk and a quantified path's inner walk alike: a `qpp_inner`
/// relationship is a group variable (CIP-28), never varlen, and a
/// quantified path nested inside another is the typed rejection above.
fn collect_element_usages(
    elem: &PatternElement,
    qpp_inner: bool,
    usages: &mut Vec<PatternUsage>,
) -> Result<(), EngineError> {
    match elem {
        PatternElement::Node(np) => {
            if let Some(var) = &np.variable {
                usages.push(PatternUsage {
                    name: var.clone(),
                    var_type: VarType::Node,
                    is_varlen: false,
                });
            }
            Ok(())
        }
        PatternElement::Relationship(rp) => {
            if let Some(var) = &rp.variable {
                let is_varlen = !qpp_inner && rp.is_variable_length();
                usages.push(PatternUsage {
                    name: var.clone(),
                    var_type: VarType::Relationship,
                    is_varlen,
                });
            }
            Ok(())
        }
        PatternElement::Quantified(_) if qpp_inner => {
            // The parser rejects nested QPPs, but a
            // hand-built AST can still reach this validator — return a
            // typed error, never abort (LABS-70).
            Err(EngineError::semantic(
                ErrorKind::SyntaxError,
                "NestedQuantification: quantified path patterns cannot be nested".to_string(),
            ))
        }
        PatternElement::Quantified(qpp) => {
            for inner in &qpp.elements {
                collect_element_usages(inner, true, usages)?;
            }
            Ok(())
        }
    }
}

/// Phases two and three over the collected usages: the cross-clause check
/// against types earlier clauses established, then the within-MATCH check
/// against earlier usages of the same list — the current usage is the
/// later one (the reuse), so its type keys the error kind in both
/// ([`reuse_error_kind`]).
fn check_usage_conflicts(
    usages: &[PatternUsage],
    var_types: &IdentHashMap<SmallName, VarType>,
) -> Result<(), EngineError> {
    for (i, usage) in usages.iter().enumerate() {
        let PatternUsage {
            name: var,
            var_type: new_type,
            is_varlen,
        } = usage;
        // Check against already-established types from previous clauses
        if let Some(existing_type) = var_types.get(var) {
            if *existing_type != *new_type {
                // A Scalar variable from WITH can be rebound as a Relationship
                // in a variable-length pattern (e.g., WITH [r1, r2] AS rs ... [rs*])
                // because variable-length rel patterns accept list variables.
                let allowed = *existing_type == VarType::Scalar
                    && *new_type == VarType::Relationship
                    && *is_varlen;
                if !allowed {
                    // Cross-clause reuse (Match6 21/22 vs Match1 7/8, Match2 9/10).
                    let kind = reuse_error_kind(*new_type);
                    return Err(EngineError::semantic(
                        kind,
                        format!(
                            "variable '{}' is already used as a {} but is used as a {} here",
                            var,
                            existing_type.name(),
                            new_type.name(),
                        ),
                    ));
                }
            }
        }

        // Check against EARLIER usages within the same MATCH clause: the
        // current usage is the later one (the reuse), so its type keys the
        // error kind — reused as a path is VariableAlreadyBound (Match6 23/24),
        // reused as a node/relationship is VariableTypeConflict (Match1 10).
        for other in &usages[..i] {
            if other.name == *var && other.var_type != *new_type {
                let kind = reuse_error_kind(*new_type);
                return Err(EngineError::semantic(
                    kind,
                    format!(
                        "variable '{}' is used as both a {} and a {} in the same pattern",
                        var,
                        new_type.name(),
                        other.var_type.name(),
                    ),
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::validate_variable_types;

    fn parse_and_validate(query: &str) -> Result<(), String> {
        let stmt = relate_parser::parse(query).map_err(|e| e.to_string())?;
        validate_variable_types(&stmt).map_err(|e| e.to_string())
    }

    #[test]
    fn test_rel_var_used_as_node_same_pattern() {
        // node↔relationship clash (no path) is VariableTypeConflict (Match1[7]).
        let result = parse_and_validate("MATCH ()-[r]-() MATCH (r) RETURN r");
        assert!(result.is_err(), "Expected VariableTypeConflict");
        assert!(result.unwrap_err().contains("VariableTypeConflict"));
    }

    #[test]
    fn test_rel_var_used_as_node_in_same_match() {
        let result = parse_and_validate("MATCH ()-[r]-(r) RETURN r");
        assert!(result.is_err(), "Expected VariableTypeConflict");
        assert!(result.unwrap_err().contains("VariableTypeConflict"));
    }

    #[test]
    fn test_path_var_used_as_node() {
        // Cross-clause: reusing a path variable AS A NODE keys on the new
        // (node) usage -> VariableTypeConflict (Match1[8]).
        let result = parse_and_validate("MATCH r = ()-[]-() MATCH (r) RETURN r");
        assert!(result.is_err(), "Expected VariableTypeConflict");
        assert!(result.unwrap_err().contains("VariableTypeConflict"));
    }

    #[test]
    fn test_path_var_used_as_node_same_pattern() {
        // The path variable `r` binds first, then the later `(r)` node reuses it
        // -> the reuse is a node -> VariableTypeConflict (Match1[10]).
        let result = parse_and_validate("MATCH r = ()-[]-(), (r) RETURN r");
        assert!(result.is_err(), "Expected VariableTypeConflict");
        assert!(result.unwrap_err().contains("VariableTypeConflict"));
    }

    #[test]
    fn test_node_then_path_same_pattern_is_already_bound() {
        // Node `p` binds first (element), then the path variable `p` reuses it
        // -> the reuse is a path -> VariableAlreadyBound (Match6[23]).
        let result = parse_and_validate("MATCH p = (p)-[]-() RETURN p");
        assert!(result.is_err(), "Expected VariableAlreadyBound");
        assert!(result.unwrap_err().contains("VariableAlreadyBound"));
    }

    #[test]
    fn test_scalar_used_as_node() {
        // scalar↔node clash (no path) is VariableTypeConflict.
        let result = parse_and_validate("WITH true AS n MATCH (n) RETURN n");
        assert!(result.is_err(), "Expected VariableTypeConflict");
        assert!(result.unwrap_err().contains("VariableTypeConflict"));
    }

    #[test]
    fn test_valid_node_reuse() {
        // Same variable used as node in multiple patterns — valid
        let result = parse_and_validate("MATCH (n) MATCH (n)-[]-() RETURN n");
        assert!(result.is_ok(), "Expected valid query: {result:?}");
    }
}
