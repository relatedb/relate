//! The overlay's seeks and expands merge the committed graph with the
//! scratch: expansion sees scratch creates and hides deleted rels, and
//! the equality seek and bucket length stay current across writes made
//! after an index pair was built.

use std::collections::BTreeMap;

use super::test_support::run_write;
use super::ExecGraph;
use crate::exec::test_support::{
    commit_scratch, exec_write, exec_write_uncommitted, write_scratch,
};
use crate::graph::Graph;
use relate_core::Value;

#[test]
fn overlay_expand_sees_scratch_create() {
    let r = run_write(
        "CREATE (a:A {id: 1})-[:R]->(b:B {id: 2}) \
         WITH a MATCH (a)-[:R]->(x) RETURN x.id AS id",
    );
    assert_eq!(r.rows.len(), 1);
    assert_eq!(r.rows[0][0].as_int(), Some(2));
}

#[test]
fn overlay_expand_hides_deleted_rel() {
    let g = Graph::default();
    let setup = write_scratch(&g);
    exec_write(&g, &setup, "CREATE (:A {id: 1})-[:R]->(:B {id: 2})").unwrap();
    commit_scratch(&g, setup);
    let result = exec_write_uncommitted(
        &g,
        "MATCH (a:A {id: 1})-[r:R]->() DELETE r \
         WITH a MATCH (a)-[:R]->(x) RETURN x",
    );
    assert!(result.rows.is_empty(), "deleted overlay rel still visible");
}

/// ADR-0172 §3: `set_property_node` and `add_labels` keep an already-built
/// `prop_ix` pair current, so the overlay equality seek (and the uniqueness
/// probe) sees nodes whose property or label changed after the build.
#[test]
fn prop_ix_seek_sees_set_and_added_label_after_the_pair_is_built() {
    let g = Graph::default();
    let a = g
        .create_node(
            vec!["A".into()],
            BTreeMap::from([("x".into(), Value::Int(1))]),
        )
        .unwrap();
    let b = g
        .create_node(
            vec!["B".into()],
            BTreeMap::from([("x".into(), Value::Int(2))]),
        )
        .unwrap();
    let scratch = write_scratch(&g);
    let view = ExecGraph::write(&g, &scratch);
    let labels = ["A".to_string()];
    let props = BTreeMap::from([("x".to_string(), Value::Int(2))]);
    assert_eq!(view.lookup_nodes(&labels, &props), Some(vec![]));
    view.set_property_node(a, "x", Value::Int(2));
    view.add_labels(b, &labels);
    assert_eq!(view.lookup_nodes(&labels, &props), Some(vec![a, b]));
}

/// LABS-803: a multi-label property seek's scratch additions satisfy the
/// seek's WHOLE predicate — the canonical `node_matches` conjunction
/// (every sought label, every sought property under Cypher equality),
/// the same predicate the base's scan tail applies, so a scratch node
/// with the property but only one of the labels stays out and the
/// merged answer's halves agree by construction.
#[test]
fn multi_label_property_seek_admits_only_whole_label_scratch_nodes() {
    let g = Graph::default();
    let scratch = write_scratch(&g);
    let view = ExecGraph::write(&g, &scratch);
    let both = view
        .create_node(
            vec!["A".into(), "B".into()],
            BTreeMap::from([("x".into(), Value::Int(1))]),
        )
        .unwrap();
    let _one_label_only = view
        .create_node(
            vec!["A".into()],
            BTreeMap::from([("x".into(), Value::Int(1))]),
        )
        .unwrap();
    let labels = ["A".to_string(), "B".to_string()];
    let props = BTreeMap::from([("x".to_string(), Value::Int(1))]);
    assert_eq!(view.lookup_nodes(&labels, &props), Some(vec![both]));
}

/// ADR-0184: `equality_bucket_len` sums the committed and the scratch
/// bucket without materializing candidates, and counts a NaN as empty.
#[test]
fn equality_bucket_len_sums_committed_and_scratch() {
    let g = Graph::default();
    for _ in 0..2 {
        g.create_node(
            vec!["P".into()],
            BTreeMap::from([("a".into(), Value::Int(1))]),
        )
        .unwrap();
    }
    g.create_node(
        vec!["P".into()],
        BTreeMap::from([("a".into(), Value::Int(2))]),
    )
    .unwrap();
    let scratch = write_scratch(&g);
    let view = ExecGraph::write(&g, &scratch);
    assert_eq!(view.equality_bucket_len("P", "a", &Value::Int(1)), 2);
    assert_eq!(view.equality_bucket_len("P", "a", &Value::Int(2)), 1);
    // A scratch node posted after the pairs were built joins the sum.
    view.create_node(
        vec!["P".into()],
        BTreeMap::from([("a".into(), Value::Int(1))]),
    )
    .unwrap();
    assert_eq!(view.equality_bucket_len("P", "a", &Value::Int(1)), 3);
    assert_eq!(
        view.equality_bucket_len("P", "a", &Value::Float(f64::NAN)),
        0
    );
}

/// LABS-1085: a keyed lookup draws its scratch additions from the
/// scratch equality index (one `(label, key)` pair) and re-verifies each
/// candidate with `node_matches`, so the answer equals the old full scan
/// over the statement's creates: a created node is found by key, a value
/// moved away after the pair was built drops out (postings are additions
/// only — the re-verify is what filters the stale id), Cypher numeric
/// equality holds across the int/float key normalization, and a
/// multi-label seek keeps its whole conjunction.
#[test]
fn a_keyed_lookup_finds_statement_creates_through_the_scratch_index() {
    let g = Graph::default();
    let scratch = write_scratch(&g);
    let view = ExecGraph::write(&g, &scratch);
    let a = view
        .create_node(
            vec!["A".into(), "B".into()],
            BTreeMap::from([("k".into(), Value::Int(1))]),
        )
        .unwrap();
    let only_a = view
        .create_node(
            vec!["A".into()],
            BTreeMap::from([("k".into(), Value::Int(1))]),
        )
        .unwrap();
    let a_label = ["A".to_string()];
    let k1 = BTreeMap::from([("k".to_string(), Value::Int(1))]);
    // Found by key among the statement's creates (builds the pair).
    assert_eq!(view.lookup_nodes(&a_label, &k1), Some(vec![a, only_a]));
    // Cypher equality: 1.0 = 1 through the normalized index key.
    let k1_float = BTreeMap::from([("k".to_string(), Value::Float(1.0))]);
    assert_eq!(
        view.lookup_nodes(&a_label, &k1_float),
        Some(vec![a, only_a])
    );
    // The whole label conjunction: only `a` carries both labels.
    let ab = ["A".to_string(), "B".to_string()];
    assert_eq!(view.lookup_nodes(&ab, &k1), Some(vec![a]));
    // A value moved away after the pair was built: the stale posting
    // stays in the index, the re-verify filters it out.
    view.set_property_node(only_a, "k", Value::Int(2));
    assert_eq!(view.lookup_nodes(&a_label, &k1), Some(vec![a]));
    let k2 = BTreeMap::from([("k".to_string(), Value::Int(2))]);
    assert_eq!(view.lookup_nodes(&a_label, &k2), Some(vec![only_a]));
}

/// The scratch half runs in the base half's equality mode, so one
/// lookup's two halves always answer with one relation (LABS-1085
/// review). Scan-served (no index serves the pair): `=` in both halves —
/// a zoned-DateTime DST-overlap twin created in the statement is found,
/// the same answer the base's scan gives its committed twin. Index-served
/// (ADR-0446's measured identity semantics, group_key.rs:48-55): the
/// seek does not find the `=`-equal partner — and the base's indexed
/// seek behaves identically for its committed twin, so the halves agree
/// there too. The overlap pair is the centralized fixture
/// (`relate-test-support::temporal::dst_overlap_half`).
#[test]
fn the_scratch_half_matches_the_base_half_s_equality_mode() {
    use relate_test_support::temporal::dst_overlap_half;

    let g = Graph::default();
    let scratch = write_scratch(&g);
    let view = ExecGraph::write(&g, &scratch);
    let early = dst_overlap_half(false);
    let late = dst_overlap_half(true);
    let first = view
        .create_node(
            vec!["E".into()],
            BTreeMap::from([("t".into(), early.clone())]),
        )
        .unwrap();
    let second = view
        .create_node(
            vec!["E".into()],
            BTreeMap::from([("t".into(), late.clone())]),
        )
        .unwrap();
    assert_ne!(first, second);
    // The equality the predicate walks (`node_matches`'s `eq_non_null`):
    // `=`-equal across the overlap.
    assert!(
        early.eq_non_null(&late),
        "the halves are `=`-equal by wall clock + zone"
    );
    let needle = BTreeMap::from([("t".to_string(), early.clone())]);
    let label = ["E".to_string()];
    // Scan-served: no index on (E, t) — the `=`-equal twin is found, the
    // same answer the base's scan gives committed twins.
    assert_eq!(
        view.lookup_nodes(&label, &needle),
        Some(vec![first, second])
    );

    // Index-served: declare the range index — now BOTH halves key on the
    // value's identity, and the seek finds only its own half.
    crate::exec::test_support::exec_autocommit(&g, "CREATE INDEX FOR (e:E) ON (e.t)").unwrap();
    assert_eq!(view.lookup_nodes(&label, &needle), Some(vec![first]));
}

/// ADR-0614: a keyed lookup no declared index covers serves from the
/// base's lazy equality postings, and the scratch half answers from its
/// identity index — one relation in both halves over a populated base,
/// the shape a within-transaction bulk load runs (base bucket + scratch
/// keyed arm, never a scratch scan). The decline keeps `=` in both
/// halves: the base's committed DST-overlap twin and the statement's
/// scratch twin are found together by one lookup.
#[test]
fn postings_served_lookup_merges_base_and_scratch_halves() {
    use relate_test_support::temporal::dst_overlap_half;

    let g = Graph::default();
    // Committed side: one node per lookup's pair.
    let base_hit = g
        .create_node(
            vec!["A".into()],
            BTreeMap::from([("x".into(), Value::Int(1))]),
        )
        .unwrap();
    let base_twin = g
        .create_node(
            vec!["E".into()],
            BTreeMap::from([("t".into(), dst_overlap_half(false))]),
        )
        .unwrap();
    let scratch = write_scratch(&g);
    let view = ExecGraph::write(&g, &scratch);
    let added = view
        .create_node(
            vec!["A".into()],
            BTreeMap::from([("x".into(), Value::Int(1))]),
        )
        .unwrap();
    let _other_value = view
        .create_node(
            vec!["A".into()],
            BTreeMap::from([("x".into(), Value::Int(2))]),
        )
        .unwrap();
    let scratch_twin = view
        .create_node(
            vec!["E".into()],
            BTreeMap::from([("t".into(), dst_overlap_half(true))]),
        )
        .unwrap();

    // Served (no declared index on (A, x)): the base's postings bucket
    // and the scratch's identity index answer one lookup — the
    // different-value node stays out of both halves.
    let a = ["A".to_string()];
    let x1 = BTreeMap::from([("x".to_string(), Value::Int(1))]);
    let mut served = vec![base_hit, added];
    served.sort_unstable();
    assert_eq!(view.lookup_nodes(&a, &x1), Some(served));

    // Declined (a zoned-datetime value): both halves scan with `=` —
    // the committed twin and the scratch twin found together, the
    // answer an identity seek in either half would narrow.
    let e = ["E".to_string()];
    let needle = BTreeMap::from([("t".to_string(), dst_overlap_half(false))]);
    let mut twins = vec![base_twin, scratch_twin];
    twins.sort_unstable();
    assert_eq!(view.lookup_nodes(&e, &needle), Some(twins));
}
