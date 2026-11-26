//! The non-trivial clause arms the dispatcher delegates to: CREATE, MERGE,
//! DELETE, SET and REMOVE. The dispatcher keeps the loop and the shared
//! clause-exit reporting; the MATCH and CALL arms delegate beside their
//! executors (`match_clause.rs`, `procedures.rs`), and UNWIND beside its
//! generator (`unwind.rs`).

use std::collections::BTreeMap;

use relate_core::{Result, Value};
use relate_ir::{Expr, MatchMode};

use crate::ctx::ExecCtx;
use crate::eval::eval;
use crate::metrics::ParkedRecord;
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::create::{
    create_patterns, merge_match_pattern, patterns_have_graph_sensitive_expr,
    reject_null_merge_props, resolve_create_patterns,
};
use super::feed::RowStream;
use super::match_clause::match_paths;
use super::one_path::ResolvedPatterns;
use super::pushdown::Residual;
use super::scope::refresh_rows;
use super::write::{apply_resolved_removes, apply_sets, delete_values, resolve_removes};

/// The CREATE clause's input policy — the one owner of the graph-sensitive
/// decision: a graph-sensitive pattern set must resolve every row's
/// patterns against the pre-write graph before the first write lands, so
/// the whole input is held in one [`exec_create`] call (charged there);
/// every other form is ADR-0660's `pass` disposition — one bounded batch
/// written at a time through the barrier. The fact is resolved once here
/// and handed down; `exec_create` never re-asks it.
pub(super) fn create_from_stream<'a, 'b>(
    graph: &'b ExecGraph<'_>,
    mut stream: RowStream<'a>,
    patterns: &'b [relate_ir::PathPattern],
    params: &'b BTreeMap<String, Value>,
    record: Option<ParkedRecord<'a>>,
) -> Result<RowStream<'b>>
where
    'a: 'b,
{
    let graph_sensitive = patterns_have_graph_sensitive_expr(patterns);
    if graph_sensitive {
        // The whole-input hold is the held form it is — `probe()` and
        // `is_held()` stay meaningful (the consumer's eager page and the
        // projection's input charge read them) — carrying the clause's
        // PROFILE record so the dispatcher's deferral is honest on both
        // returns. The arm that picked the policy owns its derived form:
        // the prepared patterns are resolved here, before any write lands.
        let rows = stream.drain()?;
        let prepared = Some(
            rows.iter()
                .map(|row| resolve_create_patterns(graph, row, patterns, params))
                .collect::<Result<Vec<_>>>()?,
        );
        let rows = exec_create(graph, rows, patterns, params, prepared)?;
        Ok(RowStream::held_profiled(rows, record))
    } else {
        stream.write_barrier(
            move |rows| exec_create(graph, rows, patterns, params, None),
            record,
        )
    }
}

/// The CREATE clause arm: charge the driving rows, create, and refresh the
/// bindings. `prepared` is the caller's one resolution of the input policy
/// — the per-row pattern sets a graph-sensitive arm resolved before any
/// write landed, `None` for the barrier's batch-at-a-time pass (the
/// patterns are re-resolved per batch against the pre-batch graph).
pub(super) fn exec_create(
    graph: &ExecGraph<'_>,
    rows: Vec<Row>,
    patterns: &[relate_ir::PathPattern],
    params: &BTreeMap<String, Value>,
    prepared: Option<Vec<Vec<relate_ir::PathPattern>>>,
) -> Result<Vec<Row>> {
    let mut rows = rows;
    // MATCH (and any earlier clause) drops its row charge on return, but
    // this clause keeps two containers until it finishes: the driving
    // rows and — when the patterns are graph-sensitive — the prepared
    // per-row pattern buffer. The prepared buffer is loop-invariant
    // (built once above, never mutated), so its retained figure
    // ([`crate::workspace::prepared_pattern_bytes`]) is computed once
    // and both containers bill as the rows' live walk plus that
    // constant — at creation and at each resize — never re-walking the
    // pattern buffer (stellar round 2, F-2-18, closing the rust-review
    // Medium: the re-walk made the clause's accounting quadratic in
    // driving rows).
    let prepared_bytes = prepared
        .as_deref()
        .map_or(0, crate::workspace::prepared_pattern_bytes);
    // The clause's one live figure — the rows' buffer walk plus the
    // loop-invariant prepared-pattern bytes — so the four re-measures
    // below cannot disagree.
    let live = |rows: &[Row]| crate::workspace::row_buffer_bytes(rows) + prepared_bytes;
    // The caller's one pairing, made structural: each row's own resolved
    // pattern set when the arm prepared them, the shared patterns
    // otherwise — consumed as one iterator zipped against the row
    // positions, so a shorter prepared set fails the assert up front,
    // never a positional `p[n]` index the caller keeps true by convention.
    let per_row: Box<dyn Iterator<Item = &[relate_ir::PathPattern]> + '_> = match &prepared {
        Some(sets) => {
            assert_eq!(
                sets.len(),
                rows.len(),
                "the prepared pattern sets pair with the driving rows"
            );
            Box::new(sets.iter().map(Vec::as_slice))
        }
        None => Box::new(std::iter::repeat(patterns)),
    };
    let mut driving = relate_core::tokens::WorkspaceCharge::retain_with(|| live(&rows))?;
    for (n, row_patterns) in (0..rows.len()).zip(per_row) {
        create_patterns(graph, &mut rows[n], row_patterns, params)?;
        if n % 64 == 63 {
            driving.resize_with(|| live(&rows))?;
        }
    }
    driving.resize_with(|| live(&rows))?;
    refresh_rows(graph, &mut rows);
    driving.resize_with(|| live(&rows))?;
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
) -> Result<Vec<Row>> {
    let mut rows = rows;

    // The match/null-check constraints drop MERGE's own self-referencing
    // tautologies; the create path keeps the original pattern (see
    // `merge_match_pattern`).
    let match_pattern = merge_match_pattern(pattern);
    let mut next = Vec::new();
    for row in rows {
        reject_null_merge_props(graph, &row, &match_pattern, params)?;
        let none = Residual::empty();
        // Resolved per row: MERGE creates between rows, so no table (and
        // no reach cache in it) may outlive the graph state it read.
        let resolved = ResolvedPatterns::resolve(
            graph,
            std::slice::from_ref(&match_pattern),
            MatchMode::DifferentRelationships,
        );
        let matched = match_paths(
            ExecCtx::new(graph, params),
            &resolved,
            MatchMode::DifferentRelationships,
            row.clone(),
            &none,
            None,
            None,
        )?;
        if matched.is_empty() {
            let mut created = row;
            create_patterns(graph, &mut created, std::slice::from_ref(pattern), params)?;
            apply_sets(graph, &mut created, on_create, params)?;
            next.push(created);
        } else {
            for mut m in matched {
                apply_sets(graph, &mut m, on_match, params)?;
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
) -> Result<Vec<Row>> {
    let mut rows = rows;

    let resolved = rows
        .iter()
        .flat_map(|row| {
            expressions
                .iter()
                .map(|expr| eval(graph, row, expr, params))
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
) -> Result<Vec<Row>> {
    let mut rows = rows;

    for row in rows.iter_mut() {
        apply_sets(graph, row, items, params)?;
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
) -> Result<Vec<Row>> {
    let mut rows = rows;

    let resolved = rows
        .iter()
        .map(|row| resolve_removes(graph, row, items, params))
        .collect::<Result<Vec<_>>>()?;
    for items in resolved {
        apply_resolved_removes(graph, items)?;
    }
    refresh_rows(graph, &mut rows);
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use crate::exec::test_support::try_exec_cypher;
    use crate::graph::Graph;
    use relate_core::{CapacityKind, Error, Value};

    /// ADR-0660's write barrier: a write whose driving input is longer
    /// than one batch (`BATCH_ROWS`) applies batch by batch and still
    /// answers the whole input — `SET`, `REMOVE`, `MERGE` and `DELETE`
    /// each route their rows through one barrier.
    #[test]
    fn a_write_over_more_than_one_batch_answers_the_whole_input() {
        use crate::exec::test_support::exec_autocommit;
        let g = Graph::default();
        // 1500 driving rows: two batches (BATCH_ROWS = 1024).
        exec_autocommit(&g, "UNWIND range(0, 1499) AS i CREATE (:Barrier {id: i})").unwrap();
        let count = |cypher: &str| exec_autocommit(&g, cypher).unwrap().rows;
        assert_eq!(
            count("MATCH (n:Barrier) SET n.touched = n.id RETURN count(*) AS c"),
            vec![vec![Value::Int(1500)]]
        );
        assert_eq!(
            count("MATCH (n:Barrier) WHERE n.touched = n.id RETURN count(*) AS c"),
            vec![vec![Value::Int(1500)]]
        );
        assert_eq!(
            count("MATCH (n:Barrier) REMOVE n.touched RETURN count(*) AS c"),
            vec![vec![Value::Int(1500)]]
        );
        assert_eq!(
            count("MATCH (n:Barrier) WHERE n.touched IS NULL RETURN count(*) AS c"),
            vec![vec![Value::Int(1500)]]
        );
        assert_eq!(
            count("UNWIND range(1500, 2999) AS i MERGE (:Barrier {id: i}) RETURN count(*) AS c"),
            vec![vec![Value::Int(1500)]]
        );
        assert_eq!(
            count("MATCH (n:Barrier) DELETE n RETURN count(*) AS c"),
            vec![vec![Value::Int(3000)]]
        );
        assert_eq!(
            count("MATCH (n:Barrier) RETURN count(*) AS c"),
            vec![vec![Value::Int(0)]]
        );
    }

    /// ADR-0463 round 3: when UNWIND materializes (its follower is a
    /// state-holder, here an unlimited `ORDER BY`), it materializes one
    /// clone of the carried row per list element, so its charge is
    /// `elements × (row width + 1)` slots, not one slot per element. A wide
    /// row (200 bindings) over a 30k list must hit the typed per-allocation
    /// refusal; the old element-only charge stayed far under the 256 MiB cap
    /// while the row clones allocated hundreds of MiB. (A pass-through
    /// follower streams the same generator instead — ADR-0660 — so it is the
    /// state-holder below that pins the materializing charge.)
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
        query.push_str(" + x AS s ORDER BY s");
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
