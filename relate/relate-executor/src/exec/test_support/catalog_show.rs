//! Catalog-show pins: run a `SHOW ...` statement and assert its row-key order.
use std::collections::BTreeMap;

use crate::ctx::QueryCtx;
use crate::graph::Graph;
use crate::overlay::ExecGraph;
use crate::row::Row;

use super::write::exec_autocommit;

/// Runs a `SHOW …` statement through the real catalog path (parse →
/// `execute_catalog` projection) against `graph` and returns the raw
/// rows — the one harness every catalog-show test module reads (the
/// indexes, constraints, and databases key-order pins), never a second
/// pasted scaffold per module.
pub(crate) fn show_catalog_rows(graph: &Graph, query: &str) -> Vec<Row> {
    let view = ExecGraph::read(graph);
    let stmt = relate_parser::parse(query).expect("parse");
    let clause = stmt.clauses.into_iter().next().expect("one clause");
    crate::exec::catalog::execute_catalog(
        &view,
        &clause,
        vec![Row::new()],
        &BTreeMap::new(),
        QueryCtx::new(None, query),
    )
    .expect("catalog rows")
}

/// The catalog rows' YIELD-* key-order pin, the one spelling every
/// column-table test reads (the indexes, constraints, and databases
/// pins): against a fresh graph — the pin's documented contract, built
/// here rather than threaded through every call as a ceremony
/// parameter — after `setup` commits (`None` when the statement needs
/// no setup), every `show` statement's row must carry keys in exactly
/// `expected` order (`Row` is an IndexMap and `catalog_result` reads
/// the first row's key order), so `SHOW … YIELD *` exposes the Neo4j
/// ordering — the producer's hand-spelled pair list can never drift
/// from the front table it renders.
pub(crate) fn assert_row_keys_match(setup: Option<&str>, show: &str, expected: &[String]) {
    let graph = Graph::default();
    if let Some(setup) = setup {
        exec_autocommit(&graph, setup).unwrap();
    }
    let rows = show_catalog_rows(&graph, show);
    assert!(!rows.is_empty());
    let expected_keys: Vec<&str> = expected.iter().map(String::as_str).collect();
    for row in &rows {
        let keys: Vec<&str> = row.keys().map(String::as_str).collect();
        assert_eq!(
            keys, expected_keys,
            "the `{show}` rows must carry keys in the front column-table order"
        );
    }
}
