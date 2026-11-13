//! LABS-1451: a group retains its key and its accumulators, never the input
//! row. The pins: the bytes a group is billed are independent of how wide
//! the input rows are and small in absolute terms; a keyed aggregation over
//! many node-keyed groups fits a budget the whole-row layout did not; and
//! every shape that reads the first row at the finish (a non-aggregate
//! item, an `ORDER BY` key, HAVING, a percentile argument) still answers
//! from the narrowed seed, while the shapes the analysis declines keep the
//! whole row and answer unchanged.

use std::collections::BTreeMap;
use std::sync::Arc;

use relate_core::tokens::{workspace_meter, LedgerGuard, TokenLedger};
use relate_core::Value;
use relate_ir::{Expr, Projection};

use super::aggregate::{Aggregation, GroupSubclauses};
use super::order::OrderKeys;
use super::test_support::{
    assert_expression_cap, exec_cypher, node_value, try_exec_cypher, Builder,
};
use crate::graph::Graph;
use crate::overlay::ExecGraph;
use crate::row::{Row, RowNames};
use crate::test_support::PeakPoolDouble;

fn item(expr: Expr, alias: &str) -> Projection {
    Projection {
        expr,
        alias: Some(alias.into()),
        source_text: None,
    }
}

/// Bytes billed after streaming `groups` one-row groups, keyed by the first
/// column `k` (`make_key(i)`), through `k, count(*)` with `extra` further
/// carried columns on every input row.
fn billed(groups: i64, extra: usize, make_key: impl Fn(i64) -> Value) -> u64 {
    let g = Graph::default();
    let graph = ExecGraph::read(&g);
    let items = [
        item(Expr::Variable("k".into()), "k"),
        item(Expr::CountStar, "n"),
    ];
    let order = OrderKeys::new(&[], &items, false);
    let params = BTreeMap::new();
    let mut aggregation = Aggregation::open(
        &graph,
        &[],
        &items,
        None,
        &params,
        &GroupSubclauses {
            order: &order,
            where_mode: None,
            import_scope: None,
        },
    )
    .expect("opened");
    let mut names = vec!["k".to_string()];
    names.extend((0..extra).map(|c| format!("c{c}")));
    let names = RowNames::new(&names);
    for i in 0..groups {
        let mut values = vec![make_key(i)];
        values.extend((0..extra).map(|c| Value::String(format!("payload-{i}-{c}").into())));
        aggregation
            .offer(&graph, Row::from_names(&names, values))
            .expect("offered");
    }
    let bytes = aggregation.billed_bytes();
    assert_eq!(
        aggregation.finish(&graph).expect("finished").len() as i64,
        groups
    );
    bytes
}

/// The memory shape: the billed bytes do not move when the input rows
/// carry ten more columns, and a node- or integer-keyed `count(*)` group is
/// billed under 100 bytes (the whole-row layout billed 600 to 1,700 for
/// the same shapes, growing with the row's width).
#[test]
fn a_groups_bill_is_independent_of_the_input_row_width() {
    for make_key in [node_value, |i: i64| Value::Int(i)] {
        let narrow = billed(4_096, 0, make_key);
        let wide = billed(4_096, 10, make_key);
        assert_eq!(narrow, wide, "ten extra carried columns changed the bill");
        let per_group = narrow / 4_096;
        assert!(
            per_group < 100,
            "a node/int-keyed count(*) group is billed {per_group} bytes"
        );
        eprintln!("FOOTPRINT per group = {per_group} B (billed {narrow})");
    }
}

/// `count(*)` is the only aggregate: the bill is the entry, the key, the
/// kept key column and eight counter bytes — and grows linearly.
#[test]
fn the_bill_grows_linearly_with_the_group_count() {
    let small = billed(2_048, 0, Value::Int);
    let large = billed(8_192, 0, Value::Int);
    let ratio = large as f64 / small as f64;
    assert!((3.0..5.5).contains(&ratio), "ratio {ratio}");
}

/// The scaled-down repro of the 1.4M-visitor failure: node-keyed groups,
/// each seeded by a one-edge match, under a budget the whole-row layout
/// (~600 bytes a group) exceeds many times over and the table's ~72 bytes
/// fit.
#[test]
fn a_node_keyed_count_over_many_groups_fits_a_budget_the_whole_row_layout_did_not() {
    let builder = Builder::new();
    let sink = builder.anon(&["S"]);
    for _ in 0..20_000 {
        let v = builder.anon(&["V"]);
        builder.rel(v, sink, "VIEWED", &[]);
        builder.rel(v, sink, "ADDED", &[]);
    }
    let mut g = builder.done();
    // 20,000 groups at the old ~600 bytes would charge ~12 MB.
    g.configure_expression_memory_limit(Some(6_000_000));
    for query in [
        "MATCH (v:V)-[:VIEWED|ADDED|BOUGHT]->() WITH v, count(*) AS n RETURN count(n) AS c",
        "MATCH (v:V)-[:VIEWED|ADDED|BOUGHT]->() WITH id(v) AS id, count(*) AS n RETURN count(n) AS c",
    ] {
        let result = try_exec_cypher(&g, query).expect("the group table fits the budget");
        assert_eq!(result.rows, vec![vec![Value::Int(20_000)]], "{query}");
    }
}

/// A lenient site that fails for every group parks one error per site and
/// group; those bytes are billed, so a high-cardinality query with several
/// failing sites over one operand meets the expression budget instead of
/// retaining past it.
#[test]
fn parked_lenient_failures_are_billed_to_the_expression_budget() {
    let mut g = Graph::default();
    g.configure_expression_memory_limit(Some(8_000_000));
    // The same groups without the failing sites fit.
    let control = try_exec_cypher(
        &g,
        "UNWIND range(1, 20000) AS i WITH i, count(*) AS c RETURN count(*) AS n",
    )
    .expect("the group table alone fits the budget");
    assert_eq!(control.rows, vec![vec![Value::Int(20_000)]]);
    let query = "UNWIND range(1, 20000) AS i WITH i, count(*) AS c \
         WHERE percentileCont(i.x, 0.1) > 0 OR percentileCont(i.x, 0.2) > 0 \
            OR percentileCont(i.x, 0.3) > 0 OR percentileCont(i.x, 0.4) > 0 \
         RETURN count(*) AS n";
    let Err(error) = try_exec_cypher(&g, query) else {
        panic!("the parked errors exceed the budget");
    };
    assert_expression_cap(&error, 8_000_000);
}

fn rows(g: &Graph, query: &str) -> Vec<Vec<Value>> {
    exec_cypher(g, query).rows
}

fn ints(values: &[i64]) -> Value {
    Value::List(Arc::new(values.iter().map(|i| Value::Int(*i)).collect()))
}

/// A mixed item reads the seed's variable beyond the key it restates.
#[test]
fn a_non_aggregate_leaf_in_a_mixed_item_reads_the_narrowed_seed() {
    let g = Graph::default();
    assert_eq!(
        rows(
            &g,
            "UNWIND [{g: 1, t: 'a'}, {g: 1, t: 'b'}, {g: 2, t: 'c'}] AS r \
             RETURN r.g AS g, count(*) + r.g AS c ORDER BY g"
        ),
        vec![
            vec![Value::Int(1), Value::Int(3)],
            vec![Value::Int(2), Value::Int(3)]
        ]
    );
}

/// A node-keyed item reads the first-seen key value: properties of the node
/// come from the kept value, not from the input row.
#[test]
fn an_item_over_a_kept_node_key_answers_from_the_key_value() {
    let builder = Builder::new();
    for (name, age) in [("ann", 30), ("bob", 40)] {
        let a = builder.prop_node(name, &["A"], &[("age", Value::Int(age))]);
        let m = builder.anon(&["M"]);
        builder.edge(a, m);
        builder.edge(a, m);
    }
    let g = builder.done();
    assert_eq!(
        rows(
            &g,
            "MATCH (a:A)-[:E]->(m) WITH a, count(*) AS c RETURN a.id AS name, a.age AS age, c ORDER BY name"
        ),
        vec![
            vec![Value::String("ann".into()), Value::Int(30), Value::Int(2)],
            vec![Value::String("bob".into()), Value::Int(40), Value::Int(2)],
        ]
    );
}

/// An `ORDER BY` key that restates a grouping expression the projection
/// renamed reads the retained variable.
#[test]
fn order_by_a_non_projected_key_variable_reads_the_narrowed_seed() {
    let g = Graph::default();
    assert_eq!(
        rows(
            &g,
            "UNWIND [{k: 1}, {k: 3}, {k: 3}, {k: 2}] AS r \
             RETURN r.k AS k, count(*) AS c ORDER BY r.k DESC"
        ),
        vec![
            vec![Value::Int(3), Value::Int(2)],
            vec![Value::Int(2), Value::Int(1)],
            vec![Value::Int(1), Value::Int(1)],
        ]
    );
}

/// HAVING reads a non-aggregate leaf (the grouping variable) beside an
/// aggregate call.
#[test]
fn having_reads_the_narrowed_seed() {
    let g = Graph::default();
    assert_eq!(
        rows(
            &g,
            "UNWIND range(1, 10) AS x WITH x % 3 AS g, sum(x) AS s \
             WHERE sum(x) > g * 10 RETURN g, s ORDER BY g"
        ),
        vec![
            vec![Value::Int(0), Value::Int(18)],
            vec![Value::Int(1), Value::Int(22)]
        ]
    );
}

/// A percentile's argument read from a row property comes from the group's
/// first row, kept in the narrowed seed.
#[test]
fn a_percentile_argument_from_a_row_property_reads_the_narrowed_seed() {
    let g = Graph::default();
    assert_eq!(
        rows(
            &g,
            "UNWIND [{g: 1, x: 1, p: 0.5}, {g: 1, x: 3, p: 0.5}, \
                    {g: 2, x: 10, p: 0.0}, {g: 2, x: 20, p: 0.0}] AS r \
             RETURN r.g AS g, percentileCont(r.x, r.p) AS m ORDER BY g"
        ),
        vec![
            vec![Value::Int(1), Value::Float(2.0)],
            vec![Value::Int(2), Value::Float(10.0)]
        ]
    );
}

/// DISTINCT and retaining aggregates keep their per-group state in their
/// own columns beside the key.
#[test]
fn distinct_and_collect_aggregates_keep_per_group_state() {
    let g = Graph::default();
    assert_eq!(
        rows(
            &g,
            "UNWIND [1, 1, 2, 3, 3] AS x \
             RETURN x % 2 AS g, count(DISTINCT x) AS d, collect(x) AS xs ORDER BY g"
        ),
        vec![
            vec![Value::Int(0), Value::Int(1), ints(&[2])],
            vec![Value::Int(1), Value::Int(2), ints(&[1, 1, 3, 3])],
        ]
    );
}

/// The conservative whole-row mode: a `*` item carries every in-scope
/// variable, so the group keeps its whole first row and answers unchanged.
#[test]
fn a_star_projection_keeps_the_whole_first_row() {
    let g = Graph::default();
    assert_eq!(
        rows(
            &g,
            "UNWIND [1, 2, 2] AS x WITH x, x * 10 AS y \
             RETURN *, count(*) AS c ORDER BY x"
        ),
        // The columns are `c, x, y`: the `*` expansion sorts by name.
        vec![
            vec![Value::Int(1), Value::Int(1), Value::Int(10)],
            vec![Value::Int(2), Value::Int(2), Value::Int(20)],
        ]
    );
}

/// The conservative whole-row mode for a captured scope: a pattern
/// comprehension in a non-key item reads outer variables by name, so the
/// group keeps its whole first row.
#[test]
fn a_pattern_scope_in_an_item_keeps_the_whole_first_row() {
    let builder = Builder::new();
    for name in ["ann", "bob"] {
        let a = builder.prop_node(name, &["A"], &[]);
        for _ in 0..if name == "ann" { 2 } else { 3 } {
            let m = builder.anon(&["M"]);
            builder.edge(a, m);
        }
    }
    let g = builder.done();
    assert_eq!(
        rows(
            &g,
            "MATCH (a:A) WITH a, count(*) + size([(a)-[:E]->(m) | m]) AS c \
             RETURN a.id AS name, c ORDER BY name"
        ),
        vec![
            vec![Value::String("ann".into()), Value::Int(3)],
            vec![Value::String("bob".into()), Value::Int(4)],
        ]
    );
}

/// A keyless `collect` of owned strings reserves those operands on the
/// workspace ledger, not only on the expression cap. The reservation
/// matches the bill and leaves with the aggregation.
#[test]
fn a_collect_reserves_its_operands_on_the_workspace_ledger() {
    let g = Graph::default();
    let graph = ExecGraph::read(&g);
    let pool = Arc::new(PeakPoolDouble::new(u64::MAX / 4));
    assert!(relate_core::tokens::PeakPool::try_grow(pool.as_ref(), 1));
    let mut ledger = TokenLedger::new(u64::MAX / 4, 1, pool);
    let _guard = LedgerGuard::install(&mut ledger);
    let items = [item(
        Expr::Function {
            name: "collect".into(),
            distinct: false,
            args: vec![Expr::Variable("x".into())],
        },
        "xs",
    )];
    let order = OrderKeys::new(&[], &items, false);
    let params = BTreeMap::new();
    let mut aggregation = Aggregation::open(
        &graph,
        &[],
        &items,
        None,
        &params,
        &GroupSubclauses {
            order: &order,
            where_mode: None,
            import_scope: None,
        },
    )
    .expect("opened");
    let opened = workspace_meter().expect("ledger installed").0;
    let payload = "p".repeat(64);
    let rows = 16u64;
    for i in 0..rows {
        let mut row = Row::new();
        row.insert("x".into(), Value::String(format!("{payload}{i}").into()));
        aggregation.offer(&graph, row).expect("offered");
    }
    let current = workspace_meter().expect("ledger installed").0;
    assert!(
        current >= opened + rows * payload.len() as u64,
        "collect reserved {current} bytes after an open of {opened}, under {} of payload",
        rows * payload.len() as u64
    );
    assert_eq!(current, aggregation.billed_bytes());
    drop(aggregation);
    assert_eq!(workspace_meter().expect("ledger installed").0, 0);
}
