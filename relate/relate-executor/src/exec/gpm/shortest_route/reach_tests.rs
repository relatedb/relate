//! The reach cache (`reach_cache.rs`): an all-pairs `MATCH` binds both
//! endpoints before the shortest-path pattern, so consecutive input rows
//! share a start. The cache may only answer "unreachable"; every row,
//! its trail and its order must be the per-pair path's. The oracle is the
//! same query with the cache refused (`with_reach_cap(0)`, every
//! non-trivial reach set over the cap) compared row for row in output
//! order, and the GPM enumeration compared as a multiset.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use relate_core::Value;
use relate_ir::Direction;

use crate::exec::test_support::{exec_cypher, run_metered};
use crate::graph::Graph;

use super::fixtures::{counting, gpm, labelled, sorted, DirectionExt, DIRS};
use super::reach_cache::{take_reach_counts, with_reach_cap};

/// Corpus seed; a failure names it with the graph index.
const SEED: u64 = 0x1438_0001;

/// `x` start nodes labelled `X`, then `y` labelled `Y`, and `rels` random
/// `R` relationships among them (self-loops and parallels included).
fn graph(rng: &mut StdRng, x: usize, y: usize, rels: usize) -> Graph {
    let rels: Vec<_> = (0..rels)
        .map(|_| (rng.random_range(0..x + y), rng.random_range(0..x + y)))
        .collect();
    labelled(x, x + y, &rels)
}

/// The cross product followed by the shortest-path pattern, in the
/// selector spellings the gate and the keyword forms use.
fn queries(dir: Direction, bound: &str) -> Vec<(String, bool)> {
    let (l, r) = dir.arrows();
    let pat = format!("(a){l}[r:R{bound}]{r}(b)");
    let ret = "RETURN a.id, b.id, length(p), p";
    vec![
        (
            format!("MATCH (a:X), (b:Y) MATCH p = ANY SHORTEST {pat} {ret}"),
            false,
        ),
        (
            format!("MATCH (a:X), (b:Y) MATCH p = ALL SHORTEST {pat} {ret}"),
            true,
        ),
        (
            format!("MATCH (a:X), (b:Y) MATCH p = shortestPath({pat}) {ret}"),
            false,
        ),
        (
            format!("MATCH (a:X), (b:Y) MATCH p = allShortestPaths({pat}) {ret}"),
            true,
        ),
        (
            format!("MATCH (a:X), (b) WHERE b <> a MATCH p = shortestPath({pat}) {ret}"),
            false,
        ),
    ]
}

#[test]
fn the_cache_leaves_every_row_and_its_order_unchanged() {
    let mut rng = StdRng::seed_from_u64(SEED);
    let mut skipped = 0;
    for i in 0..24 {
        let g = graph(&mut rng, 4, 9, 14 + i % 8);
        for dir in DIRS {
            for bound in ["*1..1", "*1..2", "*..3", "*0..2", "*..5"] {
                for (q, all) in queries(dir, bound) {
                    let (cached, _) = counting(|| exec_cypher(&g, &q).rows);
                    skipped += take_reach_counts().1;
                    let refused = with_reach_cap(0, || exec_cypher(&g, &q).rows);
                    assert_eq!(cached, refused, "graph {i} {dir:?} {bound}: {q}");
                    // The independent oracle: the enumeration engine. A
                    // single-path selector may pick another of several tied
                    // trails (ADR-0568), so it agrees on (start, end,
                    // length); an all-path selector on the trails too.
                    let _ = take_reach_counts();
                    let key = |rows: Vec<Vec<Value>>| {
                        let cut = rows
                            .into_iter()
                            .map(|r| {
                                let mut cells = r[..3].to_vec();
                                if let (true, Value::Path(p)) = (all, &r[3]) {
                                    // A trail is its relationships, whichever
                                    // end the engine read it from.
                                    let mut ids: Vec<_> = p.rels.iter().map(|x| x.id).collect();
                                    ids.sort();
                                    cells.push(Value::String(format!("{ids:?}").into()));
                                }
                                cells
                            })
                            .collect();
                        sorted(cut)
                    };
                    assert_eq!(
                        key(cached),
                        key(gpm(&g, &q)),
                        "graph {i} {dir:?} {bound}: {q}"
                    );
                }
            }
        }
    }
    assert!(
        skipped > 0,
        "the corpus never answered a pair from the cache"
    );
}

#[test]
fn one_reach_set_per_start_and_fewer_expansions() {
    // 12 starts, 60 ends, sparse: most pairs are unreachable.
    let mut rng = StdRng::seed_from_u64(SEED ^ 1);
    let g = graph(&mut rng, 12, 60, 40);
    let q =
        "MATCH (a:X), (b:Y) MATCH p = ANY SHORTEST (a)-[:R*1..3]-(b) RETURN a.id, b.id, length(p)";
    let _ = take_reach_counts();
    let (cached, with) = counting(|| exec_cypher(&g, q).rows);
    let (builds, skips) = take_reach_counts();
    let (refused, without) = counting(|| with_reach_cap(0, || exec_cypher(&g, q).rows));
    let _ = take_reach_counts();
    assert_eq!(cached, refused);
    // 720 pairs; the clause runs per batch of input rows, each batch
    // building at most one set per start it holds.
    assert!(builds * 10 < 720, "{builds} reach sets for 720 pairs");
    assert!(
        skips > 0,
        "unreachable pairs were not answered from the set"
    );
    assert!(
        with.expands * 2 < without.expands,
        "expansions with the cache {} vs without {}",
        with.expands,
        without.expands
    );
}

#[test]
fn interleaved_starts_build_no_reach_set_and_stay_correct() {
    let mut rng = StdRng::seed_from_u64(SEED ^ 2);
    let g = graph(&mut rng, 2, 8, 12);
    // Rows alternate start 0, 1, 0, 1: no two consecutive pairs share one.
    let q = "UNWIND [[0, 2], [1, 3], [0, 4], [1, 5], [0, 6], [1, 7]] AS pr \
             MATCH (a:X {id: pr[0]}), (b:Y {id: pr[1]}) \
             MATCH p = ANY SHORTEST (a)-[:R*1..3]-(b) RETURN a.id, b.id, length(p)";
    let _ = take_reach_counts();
    let cached = exec_cypher(&g, q).rows;
    assert_eq!(take_reach_counts().0, 0, "an interleaved order built a set");
    assert_eq!(cached, with_reach_cap(0, || exec_cypher(&g, q).rows));
    assert_eq!(sorted(cached), gpm(&g, q));
}

#[test]
fn a_start_over_the_cap_is_refused_once_and_searched_per_pair() {
    // A path 0-1-2-...-9 under the starts: reach sets hold several nodes.
    let rels: Vec<_> = (0..9).map(|i| (i, i + 1)).collect();
    let g = labelled(2, 10, &rels);
    let q =
        "MATCH (a:X), (b:Y) MATCH p = ANY SHORTEST (a)-[:R*1..9]-(b) RETURN a.id, b.id, length(p)";
    let _ = take_reach_counts();
    let capped = with_reach_cap(3, || exec_cypher(&g, q).rows);
    let (builds, skips) = take_reach_counts();
    assert_eq!(builds, 2, "one refused build per start, not one per row");
    assert_eq!(skips, 0);
    assert_eq!(capped, exec_cypher(&g, q).rows);
    assert_eq!(capped.len(), 16);
}

/// Peak workspace charge of the gate form with 8 starts and `ends` extra
/// isolated end nodes: pairs grow with `ends`, the reach sets do not.
fn peak_with(ends: usize) -> u64 {
    let (x, linked) = (8, 24);
    let rels: Vec<_> = (0..x + linked - 1).map(|i| (i, i + 1)).collect();
    let g = labelled(x, x + linked + ends, &rels);
    let q = "MATCH (a:X), (b:Y) MATCH p = ANY SHORTEST (a)-[:R*1..3]-(b) \
             RETURN count(*) AS pairs, sum(length(p)) AS hops";
    let (rows, peak) = run_metered(&g, q);
    assert!(matches!(rows[0][0], Value::Int(n) if n > 0));
    peak
}

#[test]
fn peak_memory_is_independent_of_the_pair_count() {
    let (small, large) = (peak_with(2_000), peak_with(8_000));
    eprintln!("reach cache: peak at 2000 ends = {small}, at 8000 = {large}");
    assert!(
        large as f64 <= small as f64 * 1.25 + 4096.0,
        "peak {large} at 4x the pairs vs {small}"
    );
}

#[test]
fn a_set_past_its_budget_is_retried_later_not_built_per_pair() {
    // One start over a 200-node path plus 20 isolated ends: its reach set
    // (200) is past the budget of the 2nd pair (128) and within the 8th's.
    let rels: Vec<_> = (0..200).map(|i| (i, i + 1)).collect();
    let g = labelled(1, 221, &rels);
    let q = "MATCH (a:X), (b:Y) MATCH p = ANY SHORTEST (a)-[:R*]-(b) RETURN count(*) AS n";
    let _ = take_reach_counts();
    let rows = exec_cypher(&g, q).rows;
    let (builds, skips) = take_reach_counts();
    assert_eq!(rows[0][0], Value::Int(200));
    assert_eq!(builds, 2, "one failed attempt, then the set");
    assert_eq!(skips, 20, "the isolated ends are answered from the set");
}
