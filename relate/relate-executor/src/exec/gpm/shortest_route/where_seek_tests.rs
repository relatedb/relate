//! WHERE-borne endpoint seeks of the shortest route (ADR-0556, extended by
//! the LABS-1653 change): an endpoint whose `=` / `IN` constraint lives in
//! the `MATCH`'s `WHERE` seeks like an inline property map does. Every
//! shape is checked against the enumerating GPM walk (rows, or the same
//! typed error), and the anchor arm and expansion counts pin that the seek
//! happened — or, where no index serves the conjunct, did not.

use relate_core::{CapacityKind, Error};

use crate::exec::test_support::{exec_autocommit, try_exec_cypher};
use crate::graph::Graph;

use super::fixtures::{counting, gpm, run, sorted, Selector, SELECTORS};
use super::route::{with_list_limit, AnchorArm};
use super::with_forced_gpm;

/// 200 `:N` nodes (`id` text, `g` parity) on a ring with `*3` chords;
/// `indexed` declares the RANGE index the point seeks need.
fn ring(indexed: bool) -> Graph {
    let g = Graph::default();
    if indexed {
        exec_autocommit(&g, "CREATE INDEX FOR (n:N) ON (n.id)").unwrap();
    }
    exec_autocommit(
        &g,
        "UNWIND range(0, 199) AS i CREATE (:N {id: toString(i), g: i % 2})",
    )
    .unwrap();
    exec_autocommit(
        &g,
        "MATCH (a:N), (b:N) WHERE (toInteger(a.id) + 1) % 200 = toInteger(b.id) \
         OR (toInteger(a.id) * 3) % 200 = toInteger(b.id) CREATE (a)-[:R]->(b)",
    )
    .unwrap();
    g
}

/// `ids` as a Cypher list literal of text ids, with a null and a duplicate
/// spliced in (neither may change the answer).
fn list_with_noise(ids: std::ops::Range<usize>) -> String {
    let mut items: Vec<String> = ids.map(|i| format!("'{i}'")).collect();
    items.push("null".into());
    let first = items.first().expect("a non-empty id range").clone();
    items.push(first);
    format!("[{}]", items.join(", "))
}

/// The route's answer to `q` is the GPM walk's — rows, or the same error —
/// and returns the route's anchor arms and expansion count.
fn parity(g: &Graph, q: &str) -> (Vec<AnchorArm>, u64) {
    let (routed, counts) = counting(|| try_exec_cypher(g, q));
    let (oracle, oracle_counts) = counting(|| with_forced_gpm(|| try_exec_cypher(g, q)));
    assert_eq!(
        oracle_counts.route_runs, 0,
        "the oracle must bypass the route for {q}"
    );
    match (routed, oracle) {
        (Ok(r), Ok(o)) => assert_eq!(sorted(r.rows), sorted(o.rows), "rows differ for {q}"),
        (Err(r), Err(o)) => assert_eq!(r, o, "errors differ for {q}"),
        (r, o) => panic!("route {:?} vs gpm {:?} for {q}", r.is_ok(), o.is_ok()),
    }
    assert!(counts.route_runs > 0, "the route must take {q}");
    (counts.arms, counts.expands)
}

const BODY: &str = "-[:R*..6]->";

#[test]
fn a_where_equality_end_seeks_the_pair() {
    let g = ring(true);
    let by_where = format!(
        "MATCH p = ANY SHORTEST (a:N {{id: '0'}}){BODY}(b:N) WHERE b.id = '150' \
         RETURN a.id, b.id, length(p)"
    );
    let by_map = format!(
        "MATCH p = ANY SHORTEST (a:N {{id: '0'}}){BODY}(b:N {{id: '150'}}) \
         RETURN a.id, b.id, length(p)"
    );
    let (arms, expands) = parity(&g, &by_where);
    assert_eq!(arms, vec![AnchorArm::Pair]);
    let (map_arms, map_expands) = parity(&g, &by_map);
    assert_eq!(map_arms, arms);
    assert_eq!(
        expands, map_expands,
        "the WHERE form must cost the map form"
    );
}

/// The q31 shape: an UNWIND of ids, each bound into a WHERE equality on
/// the end. The work is the pair searches, however many nodes the graph
/// has — the same expansions as the row-bound spelling.
#[test]
fn an_unwound_end_equality_costs_the_bound_pair_form() {
    let g = ring(true);
    let by_where = format!(
        "UNWIND ['150', '17', '99'] AS t MATCH p = ANY SHORTEST (a:N {{id: '0'}}){BODY}(b:N) \
         WHERE b.id = t RETURN t, length(p)"
    );
    let bound = format!(
        "UNWIND ['150', '17', '99'] AS t MATCH (a:N {{id: '0'}}), (b:N {{id: t}}) \
         MATCH p = ANY SHORTEST (a){BODY}(b) RETURN t, length(p)"
    );
    let (arms, expands) = parity(&g, &by_where);
    assert_eq!(arms, vec![AnchorArm::Pair; 3]);
    let (_, bound_expands) = parity(&g, &bound);
    assert_eq!(expands, bound_expands);
}

/// The q30 shape: both ends in `IN` lists past the pair bound — one
/// forward fan per start, lists carrying nulls and duplicates.
#[test]
fn in_lists_on_both_ends_fan_from_the_listed_start() {
    let g = ring(true);
    let q = format!(
        "MATCH p = ANY SHORTEST (a:N){BODY}(b:N) \
         WHERE a.id IN {} AND b.id IN {} RETURN a.id, b.id, length(p)",
        list_with_noise(0..6),
        list_with_noise(100..140)
    );
    let (arms, expands) = parity(&g, &q);
    assert_eq!(arms, vec![AnchorArm::FanForward]);
    // The seek lists the 6 starts: 6 forward fans, not one per `:N` node.
    let (_, unindexed) = parity(&ring(false), &q);
    assert!(
        expands * 5 < unindexed,
        "{expands} vs unindexed {unindexed}"
    );
}

#[test]
fn an_end_only_in_list_fans_backward_and_all_selectors_agree() {
    let g = ring(true);
    for Selector { open, close, .. } in SELECTORS {
        let q = format!(
            "MATCH p = {open}(a:N){BODY}(b:N) WHERE b.id IN {} \
             RETURN a.id, b.id, length(p)",
            list_with_noise(7..9)
        );
        let q = format!("{q}{close}");
        let (arms, _) = parity(&g, &q);
        assert_eq!(arms, vec![AnchorArm::FanBackward], "{open}");
    }
}

/// An endpoint-only conjunct beside others (a mixed `AND`) still seeks;
/// the other conjuncts apply as the residual.
#[test]
fn mixed_conjuncts_seek_and_the_residual_still_filters() {
    let g = ring(true);
    let q = format!(
        "MATCH p = ANY SHORTEST (a:N {{id: '0'}}){BODY}(b:N) \
         WHERE b.id IN ['150', '17', '99', '4'] AND b.g = 1 AND length(p) >= 1 \
         RETURN b.id, length(p)"
    );
    let (arms, _) = parity(&g, &q);
    assert_eq!(arms, vec![AnchorArm::Pair]);
}

/// An `OR` with an arm no index serves stays unseeked (the funnel serves a
/// disjunction only when every arm is served): the start scan fans forward
/// as before, same rows.
#[test]
fn an_unserved_or_does_not_seek() {
    let g = ring(true);
    let q = format!(
        "MATCH p = ANY SHORTEST (a:N {{id: '0'}}){BODY}(b:N) \
         WHERE b.id = '150' OR b.g = 1 RETURN b.id, length(p)"
    );
    let (arms, _) = parity(&g, &q);
    assert_eq!(arms, vec![AnchorArm::FanForward]);
}

/// No index serves the equality: the side keeps scan territory, the arm and
/// rows are exactly the unseeked behavior.
#[test]
fn without_an_index_the_where_equality_is_not_seeked() {
    let g = ring(false);
    let q = format!(
        "MATCH p = ANY SHORTEST (a:N {{id: '0'}}){BODY}(b:N) WHERE b.id = '150' \
         RETURN b.id, length(p)"
    );
    let (arms, _) = parity(&g, &q);
    assert_eq!(arms, vec![AnchorArm::FanForward]);
    let both = format!(
        "MATCH p = ANY SHORTEST (a:N){BODY}(b:N) \
         WHERE a.id IN {} AND b.id IN {} RETURN a.id, b.id, length(p)",
        list_with_noise(0..30),
        list_with_noise(100..130)
    );
    let (arms, _) = parity(&g, &both);
    assert_eq!(arms, vec![AnchorArm::FanForward]);
}

/// The legacy function spelling and a same-node pair: both ends allow the
/// same node, so the 51N23 rejection (or its filtered absence) must be what
/// the unseeked route (no index) answers — the walk oracle does not raise
/// 51N23 for the function spelling, so the unindexed route is the baseline.
#[test]
fn same_node_pairs_keep_the_unseeked_answer() {
    let (seeked, plain) = (ring(true), ring(false));
    let outcome = |g: &Graph, q: &str| {
        let (res, counts) = counting(|| try_exec_cypher(g, q));
        match res {
            Ok(r) => {
                assert!(counts.route_runs > 0, "the route must take {q}");
                Ok(sorted(r.rows))
            }
            Err(e) => Err(e),
        }
    };
    for q in [
        "MATCH p = shortestPath((a:N)-[*..6]->(b:N)) \
         WHERE a.id IN ['1', '2'] AND b.id IN ['1', '2'] RETURN a.id, b.id",
        "MATCH p = shortestPath((a:N)-[*..6]->(b:N)) \
         WHERE a.id IN ['1', '2'] AND b.id IN ['1', '2'] AND a <> b RETURN a.id, b.id",
        "MATCH p = ANY SHORTEST (a:N)-[:R*..6]->(b:N) \
         WHERE a.id IN ['1', '2'] AND b.id IN ['1', '2'] RETURN a.id, b.id, length(p)",
    ] {
        assert_eq!(outcome(&seeked, q), outcome(&plain, q), "{q}");
    }
    parity(
        &seeked,
        "MATCH p = ANY SHORTEST (a:N)-[:R*..6]->(b:N) \
         WHERE a.id IN ['1', '2'] AND b.id IN ['1', '2'] RETURN a.id, b.id, length(p)",
    );
}

/// A seeked side past the listing limit is the typed capacity refusal,
/// never a truncated list — the same refusal the property-map form gives.
#[test]
fn an_over_limit_where_seek_is_a_typed_refusal() {
    let g = ring(true);
    let q = format!(
        "MATCH p = ANY SHORTEST (a:N){BODY}(b:N) \
         WHERE a.id IN {} AND b.id IN {} RETURN count(p)",
        list_with_noise(0..20),
        list_with_noise(100..120)
    );
    let err = with_list_limit(4, || try_exec_cypher(&g, &q))
        .err()
        .expect("past the listing limit must refuse");
    assert!(
        matches!(
            err,
            Error::Capacity {
                kind: CapacityKind::SeekCandidates,
                ..
            }
        ),
        "{err:?}"
    );
    // Within the limit the same query answers, as the walk does.
    let (rows, _) = run(&g, &q);
    assert_eq!(sorted(rows), gpm(&g, &q));
}
