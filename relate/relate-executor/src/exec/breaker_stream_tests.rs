//! The state-holding consumers' declared bounds (ADR-0660): the aggregation,
//! `collect`, the percentiles, an unlimited `ORDER BY`, a star aggregation
//! and the join build side each consume the clause's batch stream and hold
//! only their declared state, charged (ADR-0463); `DISTINCT` is the
//! first-seen pass that holds only its charged seen-set. Every pin compares
//! two input sizes, so what it asserts is the state's growth, not the
//! input's.

use std::collections::BTreeMap;

use relate_core::Value;
use relate_ir::{Expr, Projection};

use super::aggregate::{Aggregation, GroupSubclauses};
use super::feed::RowStream;
use super::hub_join::take_hub_join_engagements;
use super::order::OrderKeys;
use super::test_support::{build, run_metered, Built};
use crate::graph::Graph;
use crate::overlay::ExecGraph;
use crate::row::{Row, RowNames};

/// The one flatness tolerance: a consumer's retained figure at `4 * n`
/// stays within this factor of its figure at `n`, so the input's own rows
/// are the only thing that grows.
const FLATNESS_TOLERANCE: f64 = 1.25;

/// The one ratio assert every pin below reads: the larger-input figure may
/// not rise past [`FLATNESS_TOLERANCE`] times the smaller-input one.
fn assert_flat(at_n: f64, at_4n: f64, label: &str) {
    assert!(
        at_4n <= at_n * FLATNESS_TOLERANCE,
        "{label}: {at_4n} vs {at_n} (beyond {FLATNESS_TOLERANCE}x)"
    );
}

/// Bytes per input row at `n`: the figure a linear (one-container) shape
/// keeps constant and a retaining-every-row shape grows.
fn per_row(peak: u64, n: usize) -> f64 {
    peak as f64 / n as f64
}

/// The per-row form of [`assert_flat`]: the consumer's peak per input row
/// at `4 * n` stays flat against its figure at `n`.
fn assert_per_row_flat(cypher: impl Fn(usize) -> String, n: usize, label: &str) {
    let g = Graph::default();
    let (_, at_n) = run_metered(&g, &cypher(n));
    let (_, at_4n) = run_metered(&g, &cypher(n * 4));
    let (a, b) = (per_row(at_n, n), per_row(at_4n, n * 4));
    eprintln!("{label}: per row at N = {a}, at 4N = {b}");
    assert_flat(a, b, label);
}

/// The aggregation's declared state is the group table: a global `count(*)`
/// over a parked generator folds the rows as they arrive, so the peak does
/// not move with the input count.
#[test]
fn an_aggregation_holds_only_its_group_table() {
    let g = Graph::default();
    let (rows, at_n) = run_metered(&g, "UNWIND range(1, 20000) AS i RETURN count(*) AS c");
    assert_eq!(rows[0][0], Value::Int(20_000));
    let (rows, at_4n) = run_metered(&g, "UNWIND range(1, 80000) AS i RETURN count(*) AS c");
    assert_eq!(rows[0][0], Value::Int(80_000));
    eprintln!("aggregation: peak at N = {at_n}, at 4N = {at_4n}");
    assert_flat(at_n as f64, at_4n as f64, "aggregation peak");
}

/// `collect` retains its operands and nothing else: its peak per input row
/// is the operand's own figure at both sizes.
#[test]
fn a_collect_holds_only_its_retained_operands() {
    assert_per_row_flat(
        |n| format!("UNWIND range(1, {n}) AS i RETURN collect(i % 7) AS xs"),
        5_000,
        "collect",
    );
}

/// A percentile retains its sample pool, shared by the plain calls over one
/// operand: the per-row figure is flat at both sizes and a second call adds
/// nothing.
#[test]
fn a_percentile_holds_only_its_sample_pool() {
    assert_per_row_flat(
        |n| format!("UNWIND range(1, {n}) AS i RETURN percentileDisc(i % 1000, 0.5) AS p"),
        5_000,
        "percentile",
    );
    let g = Graph::default();
    let one = "UNWIND range(1, 20000) AS i RETURN percentileDisc(i % 1000, 0.5) AS p";
    let two = "UNWIND range(1, 20000) AS i \
               RETURN percentileDisc(i % 1000, 0.5) AS p, percentileCont(i % 1000, 0.5) AS q";
    let (_, at_one) = run_metered(&g, one);
    let (_, at_two) = run_metered(&g, two);
    eprintln!("percentile pool: one call = {at_one}, two = {at_two}");
    assert_flat(
        at_one as f64,
        at_two as f64,
        "a second percentile over one operand grew the pool",
    );
}

/// The unlimited `ORDER BY` is a breaker that holds the rows it sorts and
/// nothing else: its peak per input row is the row's own figure at both
/// sizes.
#[test]
fn an_unlimited_order_by_holds_only_the_rows_it_sorts() {
    assert_per_row_flat(
        |n| format!("UNWIND range(1, {n}) AS i RETURN i ORDER BY i"),
        5_000,
        "unlimited order by",
    );
}

fn item(expr: Expr, alias: &str) -> Projection {
    Projection {
        expr,
        alias: Some(alias.into()),
        source_text: None,
    }
}

/// The bytes a star aggregation bills after folding `groups` distinct `k`
/// values over `groups` input rows. The shape comes from the first row (a
/// `*` key groups by every carried variable), and the group table — the
/// shape's declared state — is the only thing the fold retains.
fn star_billed(groups: i64) -> u64 {
    let g = Graph::default();
    let graph = ExecGraph::read(&g);
    let items = [item(Expr::Star, "*"), item(Expr::CountStar, "n")];
    let order = OrderKeys::new(&graph, &[], &items, false);
    let params = BTreeMap::new();
    let names = RowNames::new(&["k".to_string()]);
    // One probe row teaches the `*` key its shape; the fold then streams
    // the batches. The probe is the star aggregation's one read of a row
    // other than through the stream.
    let probe = [Row::from_names(&names, vec![Value::Int(0)])];
    let mut aggregation = Aggregation::open(
        &graph,
        &probe,
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
    let mut stream =
        RowStream::emitting((0..groups).map(|i| {
            Ok::<Row, relate_core::Error>(Row::from_names(&names, vec![Value::Int(i % 7)]))
        }));
    aggregation.consume(&graph, &mut stream).expect("consumed");
    let bytes = aggregation.billed_bytes();
    assert_eq!(aggregation.finish(&graph).expect("finished").len(), 7);
    bytes
}

/// The star aggregation's declared state is its group table: folding 4x the
/// input rows into the same seven groups bills the same table.
#[test]
fn a_star_aggregation_holds_only_its_groups() {
    let (at_n, at_4n) = (star_billed(1_000), star_billed(4_000));
    eprintln!("star aggregation: bill at N = {at_n}, at 4N = {at_4n}");
    assert_eq!(at_4n, at_n, "the group table grew with the input count");
}

/// `fan` comments on one tag and one place: the comma pair
/// `(c:Comment)-[:commentHasTag]->(t:Tag), (c)-[:commentIsLocatedIn]->(l:Place)`
/// has one `c` row per comment on side A and one `t`/`l` row on side B.
fn hub_pair(fan: usize) -> Built {
    let mut labels: Vec<&[&str]> = vec![&["Comment"][..]; fan + 2];
    labels[fan] = &["Tag"];
    labels[fan + 1] = &["Place"];
    let (tag, place) = (fan, fan + 1);
    let mut rels = Vec::new();
    for c in 0..fan {
        rels.push((c, tag, "commentHasTag"));
        rels.push((c, place, "commentIsLocatedIn"));
    }
    build(&labels, &rels)
}

/// The join build side holds the side-A rows it indexes, charged: its peak
/// per side-A row stays flat as the side grows, and the join engages (the
/// nested loop never ran).
#[test]
fn the_join_build_side_holds_only_the_rows_it_indexes() {
    // The fold-only projection is what lets the join take the pair (a
    // projecting sink keeps the nested loop), and it folds the joined rows
    // into the aggregation's own state.
    // `sum` stays on match-then-aggregate (the aggregate folds take only
    // count/distinct shapes), so the pairing reaches the pattern walk the
    // join rides.
    let cypher = "MATCH (c:Comment)-[:commentHasTag]->(t:Tag), \
                  (c)-[:commentIsLocatedIn]->(l:Place) \
                  RETURN sum(c.id) AS n";
    let measure = |fan: usize| {
        let built = hub_pair(fan);
        let _ = take_hub_join_engagements();
        let (rows, peak) = run_metered(&built.graph, cypher);
        assert_eq!(
            rows[0][0],
            Value::Int((fan as i64 - 1) * fan as i64 / 2),
            "one joined row per comment"
        );
        assert_eq!(take_hub_join_engagements(), 1, "the hub join took the pair");
        peak
    };
    let (at_n, at_4n) = (measure(200), measure(800));
    let (a, b) = (per_row(at_n, 200), per_row(at_4n, 800));
    eprintln!("join build: per row at N = {a}, at 4N = {b}");
    assert_flat(a, b, "join build per row");
}

/// `DISTINCT` is the first-seen pass, not a breaker: it retains only its
/// charged seen-set, so the peak does not move with the input count, and
/// the output keeps the input order.
#[test]
fn distinct_is_a_first_seen_pass_independent_of_the_input_count() {
    let g = Graph::default();
    let (rows, at_n) = run_metered(&g, "UNWIND range(1, 20000) AS i RETURN DISTINCT i % 7 AS k");
    assert_eq!(rows.len(), 7);
    assert_eq!(rows[0][0], Value::Int(1), "first-seen keeps input order");
    let (rows, at_4n) = run_metered(&g, "UNWIND range(1, 80000) AS i RETURN DISTINCT i % 7 AS k");
    assert_eq!(rows.len(), 7);
    eprintln!("distinct: peak at N = {at_n}, at 4N = {at_4n}");
    assert_flat(at_n as f64, at_4n as f64, "distinct peak");
}
