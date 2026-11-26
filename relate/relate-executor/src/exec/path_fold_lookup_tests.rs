//! ADR-0541's lookup-count protocol for the path fold: with the plan's
//! tokens resolved, the fold's label checks and property reads take ids
//! from the plan, so `Interner::get_id` calls do not grow with the number
//! of candidates. Split from `path_fold_tests.rs` (the answer-shape
//! suite) so each concern keeps a file with headroom.

use super::test_support::{exec_cypher_resolved, person_named};
use super::*;
use crate::graph::Graph;

/// The protocol's two pinned sample sizes: a small fixture and one large
/// enough that a per-candidate lookup would show.
const SMALL: i64 = 10;
const LARGE: i64 = 500;

/// One protocol sample: the query's rows and the `get_id` calls the run
/// made.
struct Sample {
    rows: Vec<Vec<Value>>,
    calls: u64,
}

/// One sample of the ADR-0541 protocol: `build` fills a fresh graph with
/// `n` candidates, then `query` runs over the resolved plan.
fn sample(query: &str, n: i64, build: impl Fn(&Graph, i64)) -> Sample {
    let g = Graph::default();
    g.begin_write().unwrap();
    build(&g, n);
    g.commit_write();
    let _ = crate::store::take_get_id_calls();
    let result = exec_cypher_resolved(&g, query);
    Sample {
        rows: result.rows,
        calls: crate::store::take_get_id_calls(),
    }
}

/// Run the protocol at both pinned samples and assert the `get_id` count
/// is flat; the two samples come back for the caller's own row
/// assertions.
fn samples(query: &str, build: impl Fn(&Graph, i64)) -> (Sample, Sample) {
    let small = sample(query, SMALL, &build);
    let large = sample(query, LARGE, &build);
    assert_eq!(
        small.calls, large.calls,
        "{query}: get_id calls grew with candidates"
    );
    (small, large)
}

/// ADR-0541: with the plan's tokens resolved, the fold's label checks and
/// property reads take ids from the plan — `Interner::get_id` calls do not
/// grow with the number of candidates.
#[test]
fn path_fold_lookups_do_not_grow_with_candidates() {
    let (small, large) = samples(
        "MATCH (c:Comment)-[:commentHasCreator]->(p:Person) \
         WHERE c.content IS NOT NULL AND p.ID = 1 \
         RETURN COUNT(DISTINCT c.ID) AS n",
        |g, comments| {
            let author = person_named(g, 1, "Ada", "Local");
            for id in 0..comments {
                let comment = g
                    .create_node(
                        vec!["Comment".into()],
                        BTreeMap::from([
                            ("ID".into(), Value::Int(id)),
                            ("content".into(), Value::String("x".into())),
                        ]),
                    )
                    .unwrap();
                g.create_rel(comment, author, "commentHasCreator".into(), BTreeMap::new())
                    .unwrap();
            }
        },
    );
    assert_eq!(small.rows, vec![vec![Value::Int(SMALL)]]);
    assert_eq!(large.rows, vec![vec![Value::Int(LARGE)]]);
}

/// ADR-0541: a property key the store never interned reads as absent from
/// the plan's token alone — the equality matches no row and the per-query
/// `get_id` calls (planning, the seek decision) do not grow with the
/// number of candidates, in every predicate shape that carries the key.
#[test]
fn absent_property_key_reads_do_not_grow_with_candidates() {
    let none: &[Vec<Value>] = &[];
    // An aggregate over no matching row still answers one zero row.
    let zero = [vec![Value::Int(0)]];
    for (query, expected) in [
        ("MATCH (p:Person) WHERE p.ghost = 1 RETURN p", none),
        ("MATCH (p:Person) WHERE p.ghost IS NOT NULL RETURN p", none),
        ("MATCH (p:Person {ghost: 1}) RETURN p", none),
        (
            "MATCH (p:Person) WHERE p.ghost = 1 AND p.ID > 0 RETURN p.ID",
            none,
        ),
        (
            "MATCH (p:Person) WHERE p.ghost = 1 RETURN count(p.ghost)",
            zero.as_slice(),
        ),
    ] {
        let (small, large) = samples(query, |g, people| {
            for id in 0..people {
                person_named(g, id, "Ada", "Local");
            }
        });
        assert_eq!(small.rows, expected, "{query}");
        assert_eq!(large.rows, expected, "{query}");
    }
}

/// ADR-0541: a relationship property equality and a relationship or node
/// pattern map on a key the store never interned match no row with the
/// same `get_id` count at the two pinned samples — the pattern-map keys
/// and the relationship read carry the plan's token.
#[test]
fn absent_rel_and_pattern_map_keys_do_not_grow_with_candidates() {
    for query in [
        "MATCH (:Person)-[r:KNOWS]->(:Person) WHERE r.ghost = 1 RETURN r",
        "MATCH (:Person)-[r:KNOWS {ghost: 1}]->(:Person) RETURN r",
        "MATCH (a:Person)-[:KNOWS]->(b:Person {ghost: 1}) RETURN a",
    ] {
        let (small, large) = samples(query, |g, people| {
            let first = person_named(g, 0, "Ada", "Local");
            for id in 1..=people {
                let next = person_named(g, id, "Ada", "Local");
                g.create_rel(
                    first,
                    next,
                    "KNOWS".into(),
                    BTreeMap::from([("since".into(), Value::Int(id))]),
                )
                .unwrap();
            }
        });
        assert!(small.rows.is_empty(), "{query}");
        assert!(large.rows.is_empty(), "{query}");
    }
}
