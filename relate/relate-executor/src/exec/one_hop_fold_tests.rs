//! The one-hop aggregate fold's start scan (ADR-0036, LABS-1838): the
//! WHERE's conjuncts ride the candidate funnel, so a seek on the start
//! variable (a selective `IN`, an equality) answers the scan instead of
//! the whole label bucket walking its adjacency — while everything the
//! funnel cannot seek (the rel's properties, the far node's, a null
//! bound) stays on the offer's per-hop row re-evaluation, which remains
//! the one authority on every row.
//!
//! The folded statements are spelled start-first (`(e:Entity)<-[r]-(m:Mention)`)
//! because the fold anchors on the pattern's first node and a seek
//! narrows only a start-side predicate; the written far-node-first
//! spelling still answers identically (its scan carries no seek on the
//! start variable, so nothing narrows) and the first test covers it.
//! Answer-set parity runs through the shared three-way differential
//! (`assert_written_agrees_with_held`, its statement pair from
//! `written_and_held`) behind this suite's one [`assert_agrees`]
//! wrapper; the walks probe runs once per test through the suite's
//! [`walked`] helper.

use super::test_support::{
    assert_written_agrees_with_held, exec_cypher, written, written_and_held, Builder,
};
use super::*;
use crate::graph::Graph;
use relate_core::{NodeId, Value};

/// Entities `e0..e3`, each with its mentions over one `RESOLVED_TO` hop
/// carrying the resolution `method` — the provenance shape whose
/// relationship-property group key is the one-hop fold's alone (the path
/// fold declines it). The rel table addresses entities by name, so the
/// edge facts read off the names they name. One mention shares its
/// entity's id, and the sought subset (`e0`, `e2`) differs from the
/// whole bucket so the walks count separates a seek-answered scan from
/// a label walk.
fn provenance() -> Graph {
    let b = Builder::new();
    let entities: BTreeMap<&str, NodeId> = ["e0", "e1", "e2", "e3"]
        .into_iter()
        .map(|id| (id, b.node(id, &["Entity"])))
        .collect();
    for (m, e, method) in [
        ("m0", "e0", "exact"),
        ("m1", "e0", "plural"),
        ("m2", "e0", "exact"),
        ("m3", "e1", "exact"),
        ("m4", "e2", "case"),
        ("m5", "e2", "alias"),
        ("e2", "e2", "exact"),
        ("m6", "e3", "exact"),
    ] {
        let mention = b.node(m, &["Mention"]);
        b.rel(
            mention,
            entities[e],
            "RESOLVED_TO",
            &[("method", Value::String(method.into()))],
        );
    }
    b.done()
}

/// One folded row: the entity id, the rel's method, the mention count.
fn grouped(ent: &str, method: &str, n: i64) -> Vec<Value> {
    vec![
        Value::String(ent.into()),
        Value::String(method.into()),
        Value::Int(n),
    ]
}

/// The suite's one differential call: over the shared fixture, the folded
/// statement, its held twin, and the walk-only engine must answer the
/// same rows — the `assert_agrees` spelling the streaming-fold suites
/// model. The answer set may be empty (the shared helper compares
/// unconditionally); the non-empty tests' golden rows carry the
/// fixture-matches fact.
fn assert_agrees(written: &str, held: &str) {
    assert_written_agrees_with_held(provenance, "folded", written, held);
}

/// The walks probe, spelled once: a fresh fixture, the neighbor-walk
/// counter drained on both sides of the statement's run — the rows it
/// answered beside the walks its start arm took.
fn walked(stmt: &str) -> (QueryResult, u64) {
    let g = provenance();
    take_neighbor_walks();
    let rows = exec_cypher(&g, stmt);
    (rows, take_neighbor_walks())
}

/// The aggregating `RETURN` every folded statement in this suite shares.
const REST: &str = "RETURN e.id AS ent, r.method AS method, count(m) AS n ORDER BY ent, method";

#[test]
fn one_hop_fold_start_scan_answers_from_the_wheres_seek() {
    let matched = "MATCH (e:Entity)<-[r:RESOLVED_TO]-(m:Mention) WHERE e.id IN ['e0', 'e2']";
    let (stmt, held) = written_and_held(matched, REST);
    assert_agrees(&stmt, &held);
    let (rows, walks) = walked(&stmt);
    assert_eq!(
        rows.rows,
        vec![
            grouped("e0", "exact", 2),
            grouped("e0", "plural", 1),
            grouped("e2", "alias", 1),
            grouped("e2", "case", 1),
            grouped("e2", "exact", 1),
        ]
    );
    // The sought starts are `e0` (3 mentions) and `e2` (3 mentions): only
    // their adjacencies walk. A scan-answered start arm would walk every
    // entity's — 8, the bucket's whole edge count.
    assert_eq!(walks, 6, "the seek answers the start scan: {walks}");
    // The written spelling (far node first) answers the same rows — its
    // start scan carries no seek on the start variable, so nothing
    // narrows and every row still comes from the offer's re-evaluation —
    // through the shared differential, the start-first twin the oracle.
    let written_matched =
        "MATCH (m:Mention)-[r:RESOLVED_TO]->(e:Entity) WHERE e.id IN ['e0', 'e2']";
    let written_stmt = written(written_matched, REST);
    assert_written_agrees_with_held(provenance, "the written spelling", &written_stmt, &stmt);
}

#[test]
fn one_hop_fold_start_scan_leaves_far_side_conjuncts_to_the_row() {
    // `m.id` is not a seek on the start variable: the funnel must not
    // evaluate it at the scan (m is unbound there — a scan-time read
    // would drop every candidate), and the offer's row re-evaluation
    // must apply it per hop.
    let matched =
        "MATCH (e:Entity)<-[r:RESOLVED_TO]-(m:Mention) WHERE e.id IN ['e0'] AND m.id = 'm1'";
    let (stmt, held) = written_and_held(matched, REST);
    assert_agrees(&stmt, &held);
    assert_eq!(
        exec_cypher(&provenance(), &stmt).rows,
        vec![grouped("e0", "plural", 1)]
    );
}

#[test]
fn one_hop_fold_start_scan_declines_a_null_bound_seek() {
    // The seek's bound (`m.id`) is unbound at scan time: the funnel
    // declines to the scan rather than serving a null that would render
    // the pattern empty (LABS-328's rule), so the whole bucket walks and
    // the row re-evaluation answers on the bound pair — the one mention
    // sharing its entity's id.
    let matched = "MATCH (e:Entity)<-[r:RESOLVED_TO]-(m:Mention) WHERE e.id = m.id";
    let (stmt, held) = written_and_held(matched, REST);
    assert_agrees(&stmt, &held);
    let (rows, walks) = walked(&stmt);
    assert_eq!(rows.rows, vec![grouped("e2", "exact", 1)]);
    assert_eq!(walks, 8, "the declined seek falls to the scan: {walks}");
}

#[test]
fn one_hop_fold_start_scan_answers_an_empty_seek_with_no_rows() {
    // The empty seek answers empty on every engine, so the differential
    // runs unguarded (its comparison admits an empty answer set) and the
    // emptiness and the walks probe are asserted directly.
    let matched = "MATCH (e:Entity)<-[r:RESOLVED_TO]-(m:Mention) WHERE e.id IN []";
    let (stmt, held) = written_and_held(matched, REST);
    assert_agrees(&stmt, &held);
    let (rows, walks) = walked(&stmt);
    assert!(rows.rows.is_empty());
    assert_eq!(walks, 0, "an empty seek walks nothing: {walks}");
}
