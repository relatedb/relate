//! LABS-1452: the plain cap's want rides into the batch chain, so a
//! `LIMIT` over a scan stops reading nodes once `SKIP+LIMIT` kept rows
//! exist. The pushdown rule is the cap gate's own (`plain_limit_cap`): the
//! clause's suffix is a plain `RETURN`/`WITH` with a limit and nothing
//! that could change which rows arrive or in what order. These tests pin
//! the three halves of that rule: the effect (the scan counter and the
//! PROFILE row count stay at the want, the workspace peak stays flat),
//! the answer (a differential against the unbounded chain, the per-row
//! path and the unlimited run's slice, over generated graphs, an
//! uncommitted overlay and chunk-crossing filters), and the non-pushable
//! shapes (each still scans the whole graph, with the same answer).

use proptest::collection::vec;
use proptest::prelude::*;

use super::batch::{
    take_batch_engagements, take_batch_scanned, with_batch_disabled, with_pushdown_disabled,
};
use super::chunk::CHUNK_ROWS;
use super::test_support::{
    assert_expression_cap, build, column, exec_autocommit, exec_cypher, exec_cypher_on, exec_write,
    exec_write_uncommitted, int_cell, profiled, try_exec_cypher, write_scratch, Built,
};
use super::QueryResult;
use crate::overlay::ExecGraph;
use crate::test_support::config_with_floor;

/// `n` nodes carrying `id: i`, labelled `Item` and, every third one,
/// `Commit` as well.
fn items(n: usize) -> Built {
    let both: &[&str] = &["Item", "Commit"];
    let one: &[&str] = &["Item"];
    let labels: Vec<&[&str]> = (0..n)
        .map(|i| if i % 3 == 0 { both } else { one })
        .collect();
    build(&labels, &[])
}

/// What the chain did for one run: how many clauses it took and how many
/// nodes it scanned.
struct Chain {
    engagements: u32,
    scanned: usize,
}

/// Run `cypher`, returning its rows and what the chain did. Both counters
/// are drained first, so nothing an earlier run left behind is read.
fn run_chain(run: impl Fn(&str) -> QueryResult, cypher: &str) -> (QueryResult, Chain) {
    take_batch_engagements();
    take_batch_scanned();
    let result = run(cypher);
    let chain = Chain {
        engagements: take_batch_engagements(),
        scanned: take_batch_scanned(),
    };
    (result, chain)
}

/// The effect: a capped scan reads exactly the wanted nodes, on every
/// plain-cap shape the gate admits (`RETURN`, `WITH`, `SKIP`, a labelled
/// scan, a parameterless `WHERE` that no row can drop).
#[test]
fn a_plain_limit_scans_only_the_wanted_nodes() {
    let g = items(3 * CHUNK_ROWS).graph;
    for (cypher, scanned, rows) in [
        ("MATCH (n) RETURN n LIMIT 1", 1, 1),
        ("MATCH (n) RETURN n LIMIT 10", 10, 10),
        ("MATCH (n) RETURN n SKIP 5 LIMIT 3", 8, 3),
        ("MATCH (n) WITH n LIMIT 1 RETURN n", 1, 1),
        ("MATCH (n:Commit) RETURN n LIMIT 1", 1, 1),
        ("MATCH (n:Item) RETURN n.id AS id LIMIT 700", 700, 700),
        // The want exceeds one chunk: the chain takes it in chunk-wide steps.
        ("MATCH (n) RETURN n LIMIT 1000", 1000, 1000),
        // More wanted than exist: the scan ends with the candidates.
        (
            "MATCH (n) RETURN n LIMIT 99999",
            3 * CHUNK_ROWS,
            3 * CHUNK_ROWS,
        ),
    ] {
        let (result, chain) = run_chain(|q| exec_cypher(&g, q), cypher);
        assert_eq!(chain.engagements, 1, "engagement for {cypher}");
        assert_eq!(chain.scanned, scanned, "scanned nodes for {cypher}");
        assert_eq!(result.rows.len(), rows, "rows for {cypher}");
    }
}

/// A filter that can drop rows scans chunk by chunk and stops after the
/// first chunk whose survivors fill the want — the bound is the want plus
/// one chunk, not the graph.
#[test]
fn a_filtered_limit_stops_after_the_chunk_that_fills_it() {
    let g = items(4 * CHUNK_ROWS).graph;
    // Ids are scanned ascending; the first survivor (id 1100) sits in the
    // third chunk, so three chunks are read and the fourth is not.
    let (result, chain) = run_chain(
        |q| exec_cypher(&g, q),
        "MATCH (n) WHERE n.id >= 1100 RETURN n.id AS id LIMIT 2",
    );
    assert_eq!(column(&result, "id", int_cell), vec![1100, 1101]);
    assert_eq!((chain.engagements, chain.scanned), (1, 3 * CHUNK_ROWS));
    // No survivor at all: the scan reads every chunk and answers empty.
    let (result, chain) = run_chain(
        |q| exec_cypher(&g, q),
        "MATCH (n) WHERE n.id < 0 RETURN n.id AS id LIMIT 2",
    );
    assert!(result.rows.is_empty());
    assert_eq!((chain.engagements, chain.scanned), (1, 4 * CHUNK_ROWS));
}

/// Acceptance: the PROFILE row count of the scan clause is the want, not
/// the node count.
#[test]
fn profile_reports_the_wanted_rows_for_the_scan() {
    let g = items(2000).graph;
    for (cypher, want) in [
        ("MATCH (n) RETURN n LIMIT 1", 1),
        ("MATCH (n:Commit) RETURN n LIMIT 4", 4),
        ("MATCH (n) WITH n LIMIT 2 RETURN n", 2),
    ] {
        let records = profiled(&g, cypher);
        assert_eq!(records[0].rows, want, "scan rows for {cypher}");
    }
}

/// Memory does not scale with the node count (ADR-0555). Under a small
/// statement budget a `LIMIT 1` scan over N and 4N nodes succeeds and
/// materializes the same row bytes: the finished container's charge covers
/// only the one row produced. The unbounded chain over the same graph
/// builds a row per node and is refused by that budget, which is what
/// makes the first half a guard rather than an observation.
#[test]
fn a_limit_one_scan_retains_only_the_row_it_returns() {
    const CAP: u64 = 16 * 1024;
    const QUERY: &str = "MATCH (n) RETURN n LIMIT 1";
    let mut first_row_bytes: Option<u64> = None;
    for n in [5_000, 20_000] {
        let mut built = items(n);
        built.graph.configure_expression_memory_limit(Some(CAP));
        let record = profiled(&built.graph, QUERY).remove(0);
        assert_eq!(record.rows, 1, "rows at {n} nodes");
        assert!(record.memory_bytes > 0, "row bytes at {n} nodes");
        assert_eq!(
            *first_row_bytes.get_or_insert(record.memory_bytes),
            record.memory_bytes,
            "row bytes at {n} nodes against the first size"
        );
        let refusal = with_pushdown_disabled(|| try_exec_cypher(&built.graph, QUERY))
            .err()
            .unwrap_or_else(|| panic!("the unbounded scan over {n} nodes fits the budget"));
        assert!(assert_expression_cap(&refusal, CAP) > CAP);
    }
}

/// The non-pushable shapes: each changes which rows arrive or in what
/// order, so the cap gate declines it and no want reaches the chain. A
/// shape is either served by the chain unbounded (every node scanned) or
/// by another operator (the chain never engages); the table pins which,
/// and the answer is the unpushed one either way.
#[test]
fn shapes_that_change_the_arriving_rows_are_not_pushed() {
    let n = 3 * CHUNK_ROWS;
    let g = items(n).graph;
    for (cypher, chain_serves) in [
        // ORDER BY: the top rows of a sort are not the scan's prefix.
        (
            "MATCH (n) RETURN n.id AS id ORDER BY n.id DESC LIMIT 1",
            false,
        ),
        // DISTINCT: duplicates must be dropped before the limit counts.
        ("MATCH (n) RETURN DISTINCT n.id % 2 AS m LIMIT 2", false),
        // A WHERE after the projection runs after the limit would.
        (
            "MATCH (n) WITH n LIMIT 1 WHERE n.id > 5 RETURN n.id AS id",
            false,
        ),
        // No limit, or a limit of zero: nothing bounds the scan.
        ("MATCH (n) RETURN n.id AS id SKIP 1", true),
        ("MATCH (n) RETURN n.id AS id", false),
        ("MATCH (n) RETURN n.id AS id LIMIT 0", true),
        // A later clause reads rows the limit would have cut.
        (
            "MATCH (n) MATCH (m:Commit {id: 0}) RETURN n.id AS id LIMIT 1",
            true,
        ),
        // A write clause between the scan and the limit.
        (
            "MATCH (n) SET n.seen = true RETURN n.id AS id LIMIT 1",
            true,
        ),
    ] {
        let (pushed, chain) = run_chain(|q| exec_write_uncommitted(&g, q), cypher);
        // Only a shape the chain serves has a second path to differ from:
        // the per-row path. The other shapes never reach the chain, so the
        // (0, 0) record below is their whole contract.
        if chain_serves {
            let per_row = with_batch_disabled(|| exec_write_uncommitted(&g, cypher));
            assert_eq!(pushed.rows, per_row.rows, "rows for {cypher}");
        }
        let expected = if chain_serves { (1, n) } else { (0, 0) };
        assert_eq!(
            (chain.engagements, chain.scanned),
            expected,
            "chain for {cypher}"
        );
    }
}

/// The unlimited run's slice is the pushed run's answer, and the three
/// paths — pushed, unbounded chain, per-row — agree.
fn assert_paths_agree(
    run: &impl Fn(&str) -> QueryResult,
    cypher: &str,
    unlimited: &str,
    s: usize,
    l: usize,
) {
    // Every shape the differential generates carries the plain cap, so the
    // chain must be what served the pushed run (not the per-row fallback).
    let (pushed, served) = run_chain(run, cypher);
    assert_eq!(served.engagements, 1, "the chain serves {cypher}");
    let chain = with_pushdown_disabled(|| run(cypher));
    let per_row = with_batch_disabled(|| run(cypher));
    assert_eq!(
        pushed.rows, chain.rows,
        "pushed vs unbounded chain: {cypher}"
    );
    assert_eq!(pushed.rows, per_row.rows, "pushed vs per-row: {cypher}");
    assert_eq!(pushed.columns, chain.columns, "columns: {cypher}");
    assert_eq!(pushed.columns, per_row.columns, "columns: {cypher}");
    let all = run(unlimited).rows;
    let want: Vec<_> = all.iter().skip(s).take(l).cloned().collect();
    assert_eq!(pushed.rows, want, "pushed vs the unlimited slice: {cypher}");
}

/// The query shapes the differential runs, over `(label, threshold,
/// skip, limit)`: each carries its unlimited twin for the slice oracle.
fn shapes(label: &str, threshold: i64, skip: usize, limit: usize) -> Vec<(String, String)> {
    let skip_clause = if skip == 0 {
        String::new()
    } else {
        format!(" SKIP {skip}")
    };
    let m = format!("MATCH (n{label})");
    let w = format!("{m} WHERE n.id >= {threshold}");
    vec![
        (
            format!("{m} RETURN n.id AS id{skip_clause} LIMIT {limit}"),
            format!("{m} RETURN n.id AS id"),
        ),
        (
            format!("{w} RETURN n.id AS id{skip_clause} LIMIT {limit}"),
            format!("{w} RETURN n.id AS id"),
        ),
        (
            format!("{m} RETURN n AS n{skip_clause} LIMIT {limit}"),
            format!("{m} RETURN n AS n"),
        ),
    ]
}

/// Generated graphs: labels from `{none, A, B, A+B}`, a deleted subset,
/// sizes straddling the chunk width; every shape answers the same on the
/// three paths and equals the unlimited slice.
mod generated {
    use super::*;

    fn label_sets() -> [&'static [&'static str]; 4] {
        [&[], &["A"], &["B"], &["A", "B"]]
    }

    proptest! {
        #![proptest_config(config_with_floor(24))]
        #[test]
        fn pushdown_equals_the_unpushed_answer(
            codes in vec(0usize..4, 0..(2 * CHUNK_ROWS + 80)),
            delete_mod in 2i64..9,
            label in 0usize..3,
            threshold in 0i64..700,
            skip in 0usize..6,
            limit in 1usize..40,
        ) {
            let sets = label_sets();
            let labels: Vec<&[&str]> = codes.iter().map(|&c| sets[c]).collect();
            let g = build(&labels, &[]).graph;
            // Deleted nodes leave holes in the id space the scan walks.
            exec_autocommit(&g, &format!("MATCH (n) WHERE n.id % {delete_mod} = 0 DELETE n"))
                .unwrap();
            let label = ["", ":A", ":B"][label];
            for (cypher, unlimited) in shapes(label, threshold, skip, limit) {
                let run = |q: &str| exec_cypher(&g, q);
                assert_paths_agree(&run, &cypher, &unlimited, skip, limit);
            }
        }
    }
}

/// An uncommitted overlay with created and deleted nodes: the pushed scan
/// sees the overlay's nodes and never the deleted ones, and answers what
/// the unpushed paths answer.
#[test]
fn an_uncommitted_overlay_answers_like_the_unpushed_paths() {
    let g = items(40).graph;
    let scratch = write_scratch(&g);
    exec_write(
        &g,
        &scratch,
        "CREATE (:Item {id: 9001}), (:Commit {id: 9002}), (:Item {id: 9003})",
    )
    .unwrap();
    exec_write(&g, &scratch, "MATCH (n) WHERE n.id < 10 DELETE n").unwrap();
    let view = ExecGraph::write(&g, &scratch);
    let run = |q: &str| exec_cypher_on(&view, q);
    for label in ["", ":Item", ":Commit"] {
        for (skip, limit) in [(0, 1), (0, 5), (3, 4), (0, 60), (25, 100)] {
            for (cypher, unlimited) in shapes(label, 15, skip, limit) {
                assert_paths_agree(&run, &cypher, &unlimited, skip, limit);
            }
        }
    }
    // The deleted nodes are gone and the created ones are there.
    let ids = column(&run("MATCH (n) RETURN n.id AS id"), "id", int_cell);
    assert!(ids.iter().all(|&i| i >= 10));
    assert!(ids.contains(&9001) && ids.contains(&9002) && ids.contains(&9003));
}
