//! Memory independent of path count: a `MATCH` feeding an aggregation folds
//! its rows as they are produced, so the workspace peak does not grow with
//! the number of path rows. Each statement runs at N and 4N path rows and
//! the peak workspace charge at 4N must stay within 1.25x of the peak at N.

use relate_core::Value;

use super::test_support::{build, exec_cypher, run_metered, Built};
use crate::graph::Graph;

const PATHS: usize = 4_000;
const MIB: u64 = 1024 * 1024;

/// A hub (node 0) over `fan` followers, each with one follower of its own:
/// `*1..2` from the hub yields `2 * fan` path rows.
fn hub(fan: usize) -> Built {
    let labels = vec![&["N"][..]; 1 + 2 * fan];
    let mut rels = Vec::new();
    for c in 0..fan {
        rels.push((0, 1 + c, "E"));
        rels.push((1 + c, 1 + fan + c, "E"));
    }
    build(&labels, &rels)
}

fn assert_flat(aggregation: &str) {
    let cypher = format!("MATCH (h:N {{id: 0}})-[:E*1..2]->(e) RETURN {aggregation}");
    let small = hub(PATHS / 2);
    let large = hub(PATHS * 2);
    let (small_rows, at_n) = run_metered(&small.graph, &cypher);
    let (large_rows, at_4n) = run_metered(&large.graph, &cypher);
    assert!(!small_rows.is_empty() && !large_rows.is_empty(), "{cypher}");
    eprintln!("{aggregation}: peak at N = {at_n}, at 4N = {at_4n}");
    assert!(
        at_4n as f64 <= at_n as f64 * 1.25,
        "{cypher}: peak {at_4n} at 4N vs {at_n} at N"
    );
}

#[test]
fn count_star_peak_is_independent_of_path_count() {
    assert_flat("count(*) AS c");
}

#[test]
fn count_distinct_peak_is_independent_of_path_count() {
    // The distinct endpoints grow with N, so the seen-set cannot be flat:
    // group every row onto one of a few endpoints instead.
    assert_flat("count(DISTINCT e.id % 7) AS c");
}

#[test]
fn sum_peak_is_independent_of_path_count() {
    assert_flat("sum(e.id) AS s");
}

#[test]
fn group_by_peak_is_independent_of_path_count() {
    assert_flat("e.id % 7 AS k, count(*) AS c");
}

/// The q15 shape: `count(DISTINCT end)` over 100,000 path rows under a
/// 16 MiB per-value expression limit answers exactly. The fold owns each
/// row it is offered, so the distinct operands it keeps are charged in full
/// (a caller-held clone used to share their payload and hide ~300 B/row).
#[test]
fn count_distinct_end_over_many_paths_fits_the_expression_limit() {
    let fan = 50_000;
    let mut built = hub(fan);
    built
        .graph
        .configure_expression_memory_limit(Some(16 * MIB));
    let rows = exec_cypher(
        &built.graph,
        "MATCH (h:N {id: 0})-[:E*1..2]->(e) RETURN count(DISTINCT e) AS c",
    )
    .rows;
    assert_eq!(rows[0][0], Value::Int(2 * fan as i64));
}

/// `UNWIND` feeding `count(*)` keeps no per-row state: 2,000,001 rows under
/// the same limit, with a peak that does not depend on the range size.
#[test]
fn unwind_count_star_is_independent_of_range_size() {
    let mut g = Graph::default();
    g.configure_expression_memory_limit(Some(16 * MIB));
    let (rows, small) = run_metered(&g, "UNWIND range(0, 20000) AS i RETURN count(*) AS c");
    assert_eq!(rows[0][0], Value::Int(20_001));
    let (rows, large) = run_metered(&g, "UNWIND range(0, 2000000) AS i RETURN count(*) AS c");
    assert_eq!(rows[0][0], Value::Int(2_000_001));
    eprintln!("unwind: peak at 20001 rows = {small}, at 2000001 = {large}");
    assert!(large as f64 <= small as f64 * 1.25 + 4096.0);
}

/// `count(DISTINCT s)` over N and 4N distinct strings: the retained charge
/// grows only by the seen-set's key bytes (the per-key cost at 4N stays
/// within 1.25x of the per-key cost at N); no operand is kept per key.
#[test]
fn count_distinct_retained_charge_is_the_seen_set_keys_only() {
    let g = Graph::default();
    let per_key = |n: u64| {
        let cypher = format!("UNWIND range(1, {n}) AS i RETURN count(DISTINCT toString(i)) AS c");
        let (rows, peak) = run_metered(&g, &cypher);
        assert_eq!(rows[0][0], Value::Int(n as i64));
        peak as f64 / n as f64
    };
    let (at_n, at_4n) = (per_key(5_000), per_key(20_000));
    eprintln!("count(DISTINCT): per key at N = {at_n}, at 4N = {at_4n}");
    assert!(at_4n <= at_n * 1.25, "per-key {at_4n} at 4N vs {at_n} at N");
}
