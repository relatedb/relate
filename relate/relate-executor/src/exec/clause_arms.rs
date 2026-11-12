//! The non-trivial clause arms the dispatcher delegates to: CREATE, MERGE,
//! DELETE, SET, REMOVE, and UNWIND. The dispatcher keeps the loop, the
//! MATCH arm (which needs its local fold machinery), and the shared
//! clause-exit reporting.

use std::collections::BTreeMap;
use std::rc::Rc;

use relate_core::{Result, Value};
use relate_ir::{Clause, Expr, MatchMode};
use relate_names::SmallName;

use crate::ctx::{ExecCtx, QueryCtx};
use crate::eval::eval;
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::create::{
    create_patterns, merge_match_pattern, patterns_have_graph_sensitive_expr,
    reject_null_merge_props, resolve_create_patterns,
};
use super::match_clause::match_paths;
use super::one_path::PathFilters;
use super::pushdown::Residual;
use super::scope::refresh_rows;
use super::unwind::{UnwindFeed, UnwindStats};
use super::write::{apply_resolved_removes, apply_sets, delete_values, resolve_removes};

/// The CREATE clause arm: charge the driving rows, resolve graph-sensitive
/// patterns per row, create, and refresh the bindings.
pub(super) fn exec_create(
    graph: &ExecGraph<'_>,
    rows: Vec<Row>,
    patterns: &[relate_ir::PathPattern],
    params: &BTreeMap<String, Value>,
    qctx: QueryCtx<'_>,
) -> Result<Vec<Row>> {
    let mut rows = rows;

    // MATCH (and any earlier clause) drops its row charge on
    // return, but this clause keeps that vector until it
    // finishes. Bill it for the whole write, and again as
    // created entities land in the rows.
    let mut driving = relate_core::tokens::WorkspaceCharge::retain_with(|| {
        crate::workspace::row_buffer_bytes(&rows)
    })?;
    // Hoist the graph-sensitive decision to the pattern source: the
    // prepared patterns (when needed) are built before any write lands,
    // then one loop creates every row.
    let prepared: Option<Vec<Vec<relate_ir::PathPattern>>> =
        if patterns_have_graph_sensitive_expr(patterns) {
            Some(
                rows.iter()
                    .map(|row| {
                        resolve_create_patterns(graph, row, patterns, params, qctx.cypher_version)
                    })
                    .collect::<Result<Vec<_>>>()?,
            )
        } else {
            None
        };
    for n in 0..rows.len() {
        let row_patterns = prepared.as_ref().map_or(patterns, |p| p[n].as_slice());
        create_patterns(
            graph,
            &mut rows[n],
            row_patterns,
            params,
            qctx.cypher_version,
        )?;
        if n % 64 == 63 {
            driving.resize_with(|| crate::workspace::row_buffer_bytes(&rows))?;
        }
    }
    driving.resize_with(|| crate::workspace::row_buffer_bytes(&rows))?;
    refresh_rows(graph, &mut rows);
    driving.resize_with(|| crate::workspace::row_buffer_bytes(&rows))?;
    Ok(rows)
}

/// The MERGE clause arm: match the pattern per row, create-and-set on a
/// miss, set on a hit, then refresh the bindings.
pub(super) fn exec_merge(
    graph: &ExecGraph<'_>,
    rows: Vec<Row>,
    pattern: &relate_ir::PathPattern,
    on_create: &[relate_ir::SetItem],
    on_match: &[relate_ir::SetItem],
    params: &BTreeMap<String, Value>,
    qctx: QueryCtx<'_>,
) -> Result<Vec<Row>> {
    let mut rows = rows;

    // The match/null-check constraints drop MERGE's own self-referencing
    // tautologies; the create path keeps the original pattern (see
    // `merge_match_pattern`).
    let match_pattern = merge_match_pattern(pattern);
    let mut next = Vec::new();
    for row in rows {
        reject_null_merge_props(graph, &row, &match_pattern, params, qctx.cypher_version)?;
        let none = Residual::empty();
        // Resolved per row: MERGE creates between rows, so no table (and
        // no reach cache in it) may outlive the graph state it read.
        let filters = PathFilters::resolve_all(
            graph,
            std::slice::from_ref(&match_pattern),
            MatchMode::DifferentRelationships,
        );
        let matched = match_paths(
            ExecCtx::new(graph, params, qctx.cypher_version),
            std::slice::from_ref(&match_pattern),
            &filters,
            MatchMode::DifferentRelationships,
            row.clone(),
            &none,
            None,
            None,
        )?;
        if matched.is_empty() {
            let mut created = row;
            create_patterns(
                graph,
                &mut created,
                std::slice::from_ref(pattern),
                params,
                qctx.cypher_version,
            )?;
            apply_sets(graph, &mut created, on_create, params, qctx.cypher_version)?;
            next.push(created);
        } else {
            for mut m in matched {
                apply_sets(graph, &mut m, on_match, params, qctx.cypher_version)?;
                next.push(m);
            }
        }
    }
    rows = next;
    refresh_rows(graph, &mut rows);
    Ok(rows)
}

/// The DELETE clause arm: resolve the expressions, delete the values, and
/// refresh the surviving bindings as tombstones.
pub(super) fn exec_delete(
    graph: &ExecGraph<'_>,
    rows: Vec<Row>,
    detach: bool,
    expressions: &[Expr],
    params: &BTreeMap<String, Value>,
    qctx: QueryCtx<'_>,
) -> Result<Vec<Row>> {
    let mut rows = rows;

    let resolved = rows
        .iter()
        .flat_map(|row| {
            expressions
                .iter()
                .map(|expr| eval(graph, row, expr, params, qctx.cypher_version))
        })
        .collect::<Result<Vec<_>>>()?;
    delete_values(graph, &resolved, detach)?;
    // The deleted binding stays addressable as an empty
    // tombstone (`refresh_rows`), which a later `MATCH` on the
    // same variable still matches (`node_ok`'s snapshot path)
    // while the store no longer holds the node.
    refresh_rows(graph, &mut rows);
    Ok(rows)
}

/// The SET clause arm: apply each item per row and refresh the bindings.
pub(super) fn exec_set(
    graph: &ExecGraph<'_>,
    rows: Vec<Row>,
    items: &[relate_ir::SetItem],
    params: &BTreeMap<String, Value>,
    qctx: QueryCtx<'_>,
) -> Result<Vec<Row>> {
    let mut rows = rows;

    for row in rows.iter_mut() {
        apply_sets(graph, row, items, params, qctx.cypher_version)?;
    }
    refresh_rows(graph, &mut rows);
    Ok(rows)
}

/// The REMOVE clause arm: resolve and apply each removal, then refresh.
pub(super) fn exec_remove(
    graph: &ExecGraph<'_>,
    rows: Vec<Row>,
    items: &[relate_ir::RemoveItem],
    params: &BTreeMap<String, Value>,
    qctx: QueryCtx<'_>,
) -> Result<Vec<Row>> {
    let mut rows = rows;

    let resolved = rows
        .iter()
        .map(|row| resolve_removes(graph, row, items, params, qctx.cypher_version))
        .collect::<Result<Vec<_>>>()?;
    for items in resolved {
        apply_resolved_removes(graph, items)?;
    }
    refresh_rows(graph, &mut rows);
    Ok(rows)
}

/// The UNWIND clause arm in its materializing form: the same row generator
/// the fused aggregation consumes, drained into a held vector with the
/// ADR-0463 value-buffer charge and cancellation sampling. `tail` is the
/// clause list after this UNWIND (the dispatcher slices it once for
/// `Source::plan`).
pub(super) fn exec_unwind(
    graph: &ExecGraph<'_>,
    tail: &[Clause],
    rows: Vec<Row>,
    expression: &Expr,
    alias: &SmallName,
    params: &BTreeMap<String, Value>,
    qctx: QueryCtx<'_>,
) -> Result<Vec<Row>> {
    UnwindFeed::new(
        ExecCtx::new(graph, params, qctx.cypher_version),
        tail,
        rows,
        expression,
        alias,
        Rc::new(UnwindStats::new(false)),
    )
    .materialize()
}

#[cfg(test)]
mod tests {
    use crate::exec::test_support::try_exec_cypher;
    use crate::graph::Graph;
    use relate_core::{CapacityKind, Error, Value};

    /// ADR-0463 round 3: UNWIND materializes one clone of the carried row
    /// per list element, so its charge is `elements × (row width + 1)` slots,
    /// not one slot per element. A wide row (200 bindings) over a 30k list
    /// must hit the typed per-allocation refusal; the old element-only charge
    /// stayed far under the 256 MiB cap while the row clones allocated
    /// hundreds of MiB.
    #[test]
    fn unwinding_a_wide_row_charges_the_row_clone() {
        let width = 200usize;
        let mut query = String::from("UNWIND [1] AS seed WITH ");
        for i in 0..width {
            if i > 0 {
                query.push_str(", ");
            }
            query.push_str(&format!("seed + {i} AS a{i}"));
        }
        query.push_str(" UNWIND range(1, 30000) AS x RETURN ");
        query.push_str(
            &(0..width)
                .map(|i| format!("a{i}"))
                .collect::<Vec<_>>()
                .join(" + "),
        );
        query.push_str(" + x AS s");
        let error = try_exec_cypher(&Graph::default(), &query)
            .err()
            .expect("a wide-row UNWIND must be refused");
        assert!(
            matches!(
                error,
                Error::Capacity {
                    limit,
                    kind: CapacityKind::Expression,
                    ..
                } if limit == crate::memory::EXPRESSION_BUDGET_BYTES
            ),
            "{error}"
        );
    }

    /// LABS-1246: an aggregation fed by `UNWIND range(...)` folds the rows as
    /// they are generated, so its state does not depend on the range length.
    /// Two million elements under a 1 MiB expression cap refuse when the list
    /// and the rows are materialized and pass when streamed.
    #[test]
    fn a_range_unwind_into_an_aggregation_holds_no_rows() {
        use crate::exec::test_support::exec_cypher;
        let mut g = Graph::default();
        g.configure_expression_memory_limit(Some(1 << 20));
        let result = exec_cypher(
            &g,
            "UNWIND range(0, 1999999) AS i RETURN i % 4 AS b, count(*) AS n ORDER BY b",
        );
        assert_eq!(
            result.rows,
            (0..4)
                .map(|b| vec![Value::Int(b), Value::Int(500_000)])
                .collect::<Vec<_>>()
        );
    }

    /// The streamed path reads the same answers as the materialized one: the
    /// same `range()` argument contract (negative and empty ranges, a step
    /// that does not land on the end, the extreme-bound size arithmetic) and
    /// the same scalar and null sources.
    #[test]
    fn a_streamed_unwind_matches_the_materialized_source() {
        use crate::exec::test_support::{exec_cypher, try_exec_cypher};
        let g = Graph::default();
        let same = |streamed: &str, listed: &str| {
            assert_eq!(
                exec_cypher(&g, streamed).rows,
                exec_cypher(&g, listed).rows,
                "{streamed}"
            );
        };
        same(
            "UNWIND range(5, 1, -2) AS i RETURN count(*) AS n, sum(i) AS s",
            "UNWIND [5, 3, 1] AS i RETURN count(*) AS n, sum(i) AS s",
        );
        same(
            "UNWIND range(1, 0) AS i RETURN count(*) AS n, sum(i) AS s",
            "UNWIND [] AS i RETURN count(*) AS n, sum(i) AS s",
        );
        same(
            "UNWIND range(1, 10, 4) AS i RETURN count(*) AS n, max(i) AS m",
            "UNWIND [1, 5, 9] AS i RETURN count(*) AS n, max(i) AS m",
        );
        same(
            "UNWIND range(9223372036854775800, 9223372036854775807, 3) AS i RETURN count(*) AS n",
            "UNWIND [9223372036854775800, 9223372036854775803, 9223372036854775806] AS i RETURN count(*) AS n",
        );
        same(
            "UNWIND null AS i RETURN count(*) AS n",
            "UNWIND [] AS i RETURN count(*) AS n",
        );
        same(
            "UNWIND 7 AS i RETURN count(*) AS n, sum(i) AS s",
            "UNWIND [7] AS i RETURN count(*) AS n, sum(i) AS s",
        );
        assert!(try_exec_cypher(&g, "UNWIND range(1, 3, 0) AS i RETURN count(*)").is_err());
        assert!(try_exec_cypher(
            &g,
            "UNWIND range(1, -9223372036854775808) AS i RETURN count(*)"
        )
        .is_err());
    }

    /// A retaining aggregate keeps its refusal: `collect` over the same
    /// streamed range still charges what it holds.
    #[test]
    fn a_streamed_collect_is_still_charged() {
        use crate::exec::test_support::try_exec_cypher;
        let mut g = Graph::default();
        g.configure_expression_memory_limit(Some(1 << 20));
        let error = try_exec_cypher(&g, "UNWIND range(0, 1999999) AS i RETURN collect(i) AS xs")
            .err()
            .expect("a retained collect over two million values must be refused");
        assert!(
            matches!(
                error,
                Error::Capacity {
                    kind: CapacityKind::Expression,
                    ..
                }
            ),
            "{error}"
        );
    }

    /// Two fused UNWINDs in one statement each drive their own generator:
    /// the second one's range comes from the first one's aggregate.
    #[test]
    fn two_streamed_unwinds_in_one_statement_do_not_share_state() {
        use crate::exec::test_support::exec_cypher;
        let g = Graph::default();
        let result = exec_cypher(
            &g,
            "UNWIND range(1, 4) AS a WITH count(*) AS c \
             UNWIND range(1, c * 10) AS b RETURN count(*) AS n, sum(b) AS s",
        );
        assert_eq!(result.rows, vec![vec![Value::Int(40), Value::Int(820)]]);
    }
}
