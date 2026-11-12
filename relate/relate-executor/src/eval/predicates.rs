//! Match predicate evaluation: the one candidate filter policy (inline
//! predicates + WHERE) and the horizontal-aggregate rewrite, shared by the
//! walk's sink, the reduce tail, and the ordinary MATCH projection. Lives
//! below every consumer so no driver module owns another's filter policy.

use std::collections::BTreeMap;
use std::sync::Arc;

use relate_core::{Result, Value};
use relate_ir::{Expr, PathPattern, PatternElement};

use crate::eval::{eval, rewrite_horizontal_aggregates};
use crate::overlay::ExecGraph;
use crate::row::Row;

pub(crate) fn eval_match_predicate(
    graph: &ExecGraph<'_>,
    row: &Row,
    expr: &Expr,
    params: &BTreeMap<String, Value>,
) -> Result<Value> {
    // The horizontal-aggregate rewrite borrows every tree it does not
    // transform (the aggregate-free gate is the transform's own first
    // decision, so the hot paths — the per-candidate residual conjuncts,
    // evaluated once per scanned node — pay nothing), so the call is
    // unconditional: an aggregate-bearing predicate owns its rebuilt tree,
    // the clone the transformed tree costs.
    let rewritten = rewrite_horizontal_aggregates(graph, row, expr, params)?;
    eval(graph, row, &rewritten, params)
}

/// Whether a MATCH predicate holds on `row` — the one spelling of the
/// truthiness decision every inline-predicate site reads: the runtime
/// list/path conversion applies, a null does not hold, and any other
/// non-boolean is a type error (the walk's predicate filter).
pub(crate) fn predicate_holds(
    graph: &ExecGraph<'_>,
    row: &Row,
    predicate: &Expr,
    params: &BTreeMap<String, Value>,
) -> Result<bool> {
    super::predicate_value_holds(&eval_match_predicate(graph, row, predicate, params)?)
}

/// The optional-WHERE gate — the one spelling of "a WHERE clause that is
/// absent imposes no constraint": `None` holds, `Some` holds iff the
/// predicate holds. Every optional-WHERE site reads this, never a
/// hand-spelled `if let Some(...)` — the MATCH candidate filter, the
/// inline pattern predicates, and the `CALL` composer's YIELD WHERE.
pub(crate) fn where_clause_holds(
    graph: &ExecGraph<'_>,
    row: &Row,
    where_clause: Option<&Expr>,
    params: &BTreeMap<String, Value>,
) -> Result<bool> {
    match where_clause {
        Some(predicate) => predicate_holds(graph, row, predicate, params),
        None => Ok(true),
    }
}

/// The comprehension WHERE gate — the optional-WHERE spelling both
/// comprehension loops read (the list and pattern comprehensions' shared
/// retaining-projection loop and the map comprehension's own), the
/// comprehension twin of [`where_clause_holds`]. It deliberately
/// skips the horizontal-aggregate rewrite [`predicate_holds`] applies
/// first: that rewrite folds an aggregate over the row's row-bound list
/// variables once per row — a per-row device of the MATCH-predicate
/// family — while a comprehension's WHERE is evaluated once per bound
/// element, where the row's list variable is the source being iterated,
/// not a row of an aggregation; and the front end rejects an aggregate
/// in these positions outright (`validate/aggregation/non_aggregating`),
/// so there is nothing here to fold. The evaluated predicate reads
/// through [`super::predicate_value_holds`] like every other runtime
/// predicate position.
pub(crate) fn comprehension_where_holds(
    graph: &ExecGraph<'_>,
    row: &Row,
    where_clause: Option<&Expr>,
    params: &BTreeMap<String, Value>,
) -> Result<bool> {
    match where_clause {
        Some(predicate) => super::predicate_value_holds(&eval(graph, row, predicate, params)?),
        None => Ok(true),
    }
}

#[cfg(test)]
thread_local! {
    /// Test-only: how many times [`candidate_passes`] ran on this thread
    /// — the observable behind LABS-853's "the clause filter runs once
    /// per row" contract (`exec/limit_where_tests.rs`).
    static CANDIDATE_PASSES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Test-only: read and reset this thread's [`candidate_passes`] count.
#[cfg(test)]
pub(crate) fn take_candidate_passes() -> u64 {
    CANDIDATE_PASSES.with(|c| c.replace(0))
}

/// Whether a candidate row passes the patterns' inline predicates and
/// the optional WHERE — the one filter policy the walk's sink, the
/// reduce tail, and the ordinary MATCH projection all consult: the
/// inline sweep plus the WHERE gate, composed.
pub(crate) fn candidate_passes(
    graph: &ExecGraph<'_>,
    patterns: &[PathPattern],
    where_clause: Option<&Expr>,
    row: &Row,
    params: &BTreeMap<String, Value>,
) -> Result<bool> {
    #[cfg(test)]
    CANDIDATE_PASSES.with(|c| c.set(c.get() + 1));
    if !inline_predicates_ok(graph, patterns, row, params)? {
        return Ok(false);
    }
    where_clause_holds(graph, row, where_clause, params)
}

/// The inline-predicates half: every pattern's element-level WHEREs plus
/// each path's own WHERE — the sweep `candidate_passes` runs before the
/// clause-level WHERE gate.
pub(crate) fn inline_predicates_ok(
    graph: &ExecGraph<'_>,
    patterns: &[PathPattern],
    row: &Row,
    params: &BTreeMap<String, Value>,
) -> Result<bool> {
    for path in patterns {
        if !check_elements(graph, &path.elements, row, params)? {
            return Ok(false);
        }
        if !where_clause_holds(graph, row, path.where_clause.as_deref(), params)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The per-element half of the candidate filter: every element's inline
/// WHERE, plus a quantified relationship's per-iteration predicate (its
/// variable is bound to each list value in turn, or evaluated once against
/// the row when the relationship is anonymous).
fn check_elements(
    graph: &ExecGraph<'_>,
    elements: &[PatternElement],
    row: &Row,
    params: &BTreeMap<String, Value>,
) -> Result<bool> {
    for element in elements {
        match element {
            PatternElement::Node(node) => {
                if !where_clause_holds(graph, row, node.where_clause.as_ref(), params)? {
                    return Ok(false);
                }
            }
            PatternElement::Relationship(rel) if rel.quantified => {
                if let Some(predicate) = &rel.where_clause {
                    if let Some(name) = &rel.variable {
                        let values = match row.get_name(name) {
                            Some(Value::List(values)) => Arc::clone(values),
                            // An unbound variable is deferred to the
                            // post-filter; a bound non-list is a broken
                            // invariant, kept loud.
                            None => continue,
                            Some(_) => {
                                return Err(relate_core::Error::internal(format!(
                                    "predicate variable '{name}' is bound to a non-list"
                                )))
                            }
                        };
                        for value in values.iter() {
                            let mut iteration = row.clone();
                            iteration.insert(name.to_string(), value.clone());
                            if !predicate_holds(graph, &iteration, predicate, params)? {
                                return Ok(false);
                            }
                        }
                    } else if !predicate_holds(graph, row, predicate, params)? {
                        // An anonymous quantified relationship has no
                        // iteration variable to bind: evaluate its WHERE once
                        // against the row, mirroring the walk.
                        return Ok(false);
                    }
                }
            }
            PatternElement::Relationship(rel) => {
                if !where_clause_holds(graph, row, rel.where_clause.as_ref(), params)? {
                    return Ok(false);
                }
            }
            // QPP predicates are checked against singleton iteration
            // bindings while the quantified operand is expanded.
            PatternElement::Quantified(_) => {}
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::test_support::agg_fn;
    use crate::graph::Graph;

    /// The evaluated row predicate is the rewrite path too: a WHERE on a
    /// horizontal aggregate folds the row-bound list instead of aggregating
    /// vertically.
    #[test]
    fn eval_match_predicate_folds_row_bound_list() {
        let graph = Graph::default();
        let graph = ExecGraph::read(&graph);
        let mut row = Row::new();
        row.insert(
            "x".into(),
            Value::List(vec![Value::Int(1), Value::Null, Value::Int(3)].into()),
        );
        let expr = agg_fn("max");
        assert_eq!(
            eval_match_predicate(&graph, &row, &expr, &BTreeMap::new()).unwrap(),
            Value::Int(3)
        );
    }
}
