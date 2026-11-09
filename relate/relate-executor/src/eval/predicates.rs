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
    cypher_version: Option<u32>,
) -> Result<Value> {
    // A predicate with no aggregate anywhere inside it rewrites to a
    // structurally identical clone (the horizontal-aggregate transform
    // only fires on an aggregate function node), so the hot paths — the
    // per-candidate residual conjuncts, evaluated once per scanned node —
    // skip the deep clone and drop of the rewrite and evaluate the
    // original tree directly (GBL SF0.1 q8: the CONTAINS seed filter paid
    // one full Expr clone per forum). An aggregate-bearing predicate
    // keeps the rewrite: the clone is the price of the transformed tree.
    if relate_ir::is_agg_expr(expr) {
        let rewritten = rewrite_horizontal_aggregates(graph, row, expr, params, cypher_version)?;
        eval(graph, row, &rewritten, params, cypher_version)
    } else {
        eval(graph, row, expr, params, cypher_version)
    }
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
    cypher_version: Option<u32>,
) -> Result<bool> {
    super::predicate_value_holds(&eval_match_predicate(
        graph,
        row,
        predicate,
        params,
        cypher_version,
    )?)
}

/// The optional-WHERE gate — the one spelling of "a WHERE clause that is
/// absent imposes no constraint": `None` holds, `Some` holds iff the
/// predicate holds. Every optional-WHERE site reads this, never a
/// hand-spelled `if let Some(...)`.
fn where_clause_holds(
    graph: &ExecGraph<'_>,
    row: &Row,
    where_clause: Option<&Expr>,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<bool> {
    match where_clause {
        Some(predicate) => predicate_holds(graph, row, predicate, params, cypher_version),
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
    cypher_version: Option<u32>,
) -> Result<bool> {
    #[cfg(test)]
    CANDIDATE_PASSES.with(|c| c.set(c.get() + 1));
    if !inline_predicates_ok(graph, patterns, row, params, cypher_version)? {
        return Ok(false);
    }
    where_clause_holds(graph, row, where_clause, params, cypher_version)
}

/// The inline-predicates half: every pattern's element-level WHEREs plus
/// each path's own WHERE — the sweep `candidate_passes` runs before the
/// clause-level WHERE gate.
pub(crate) fn inline_predicates_ok(
    graph: &ExecGraph<'_>,
    patterns: &[PathPattern],
    row: &Row,
    params: &BTreeMap<String, Value>,
    cypher_version: Option<u32>,
) -> Result<bool> {
    for path in patterns {
        if !check_elements(graph, &path.elements, row, params, cypher_version)? {
            return Ok(false);
        }
        if !where_clause_holds(
            graph,
            row,
            path.where_clause.as_deref(),
            params,
            cypher_version,
        )? {
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
    cypher_version: Option<u32>,
) -> Result<bool> {
    for element in elements {
        match element {
            PatternElement::Node(node) => {
                if !where_clause_holds(
                    graph,
                    row,
                    node.where_clause.as_ref(),
                    params,
                    cypher_version,
                )? {
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
                            if !predicate_holds(
                                graph,
                                &iteration,
                                predicate,
                                params,
                                cypher_version,
                            )? {
                                return Ok(false);
                            }
                        }
                    } else if !predicate_holds(graph, row, predicate, params, cypher_version)? {
                        // An anonymous quantified relationship has no
                        // iteration variable to bind: evaluate its WHERE once
                        // against the row, mirroring the walk.
                        return Ok(false);
                    }
                }
            }
            PatternElement::Relationship(rel) => {
                if !where_clause_holds(
                    graph,
                    row,
                    rel.where_clause.as_ref(),
                    params,
                    cypher_version,
                )? {
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
            eval_match_predicate(&graph, &row, &expr, &BTreeMap::new(), None).unwrap(),
            Value::Int(3)
        );
    }
}
