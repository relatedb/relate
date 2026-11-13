//! ADR-0171: the per-label snapshot probe keeps `+labels` / `-labels` equal
//! to the old census while checking a bounded number of snapshot nodes —
//! and ADR-0595: the apply-tail drain reclaims the postings the removal
//! sites killed, respecting the same snapshot horizon.

use std::collections::BTreeMap;

use crate::exec::test_support::{exec_autocommit, exec_write, write_scratch, write_scratch_at};
use crate::graph::{take_label_probe_checks, Graph, QueryStats};
use crate::redo::RedoOp;

/// Run `q` on a fresh scratch at `ts` (nothing is committed). Returns its
/// stats and how many snapshot ids label accounting checked.
fn probe_at(g: &Graph, ts: u64, q: &str) -> (QueryStats, u64) {
    let scratch = write_scratch_at(g, ts);
    take_label_probe_checks();
    let stats = exec_write(g, &scratch, q).unwrap().stats;
    (stats, take_label_probe_checks())
}

fn probe(g: &Graph, q: &str) -> (QueryStats, u64) {
    probe_at(g, g.commit_ts(), q)
}

/// Run `q` at the current commit and apply its redo as the next commit. The
/// apply's tail drain reclaims the commit's own dying postings (ADR-0595);
/// ids a still-pinned snapshot can see stay.
fn commit_query(g: &Graph, q: &str) -> QueryStats {
    exec_autocommit(g, q).unwrap().stats
}

fn seed(g: &Graph, label: &str, n: usize) {
    for _ in 0..n {
        g.create_node(vec![label.into()], BTreeMap::new()).unwrap();
    }
}

fn labels(stats: &QueryStats) -> (i64, i64) {
    (stats.labels_added, stats.labels_removed)
}

#[test]
fn label_probe_inspections_independent_of_graph_size() {
    for n in [1_000, 10_000] {
        let g = Graph::default();
        seed(&g, "Seed", n);
        let (create_seed, seed_checks) = probe(&g, "CREATE (:Seed)");
        assert_eq!(labels(&create_seed), (0, 0));
        let (create_fresh, fresh_checks) = probe(&g, "CREATE (:Fresh)");
        assert_eq!(labels(&create_fresh), (1, 0));
        let (delete_one, delete_checks) = probe(&g, "MATCH (n:Seed) WHERE id(n) = 0 DELETE n");
        assert_eq!(delete_one.nodes_deleted, 1);
        assert_eq!(labels(&delete_one), (0, 0));
        // A gain stops at the first visible carrier, an uninterned label is
        // not probed, and a loss needs one other carrier besides the deleted
        // node.
        assert_eq!(
            [seed_checks, fresh_checks, delete_checks],
            [1, 0, 2],
            "[CREATE (:Seed), CREATE (:Fresh), DELETE one :Seed] checks at {n} :Seed nodes"
        );
    }
}

#[test]
fn deleting_all_of_a_label_checks_at_most_its_set() {
    let g = Graph::default();
    seed(&g, "Other", 2_000);
    seed(&g, "L", 300);
    let (stats, checks) = probe(&g, "MATCH (n:L) DELETE n");
    assert_eq!(stats.nodes_deleted, 300);
    assert_eq!(labels(&stats), (0, 1));
    // The last carrier's loss must rule out every other id, each once.
    assert_eq!(checks, 300);
}

#[test]
fn create_after_committed_delete_skips_the_stale_id() {
    let g = Graph::default();
    commit_query(&g, "CREATE (:A)");
    // A transaction pinned at the pre-delete snapshot: its pin keeps the
    // deleted node's id in `label_index` for `A` (ADR-0595's requeue), so
    // a later probe still checks it and skips it.
    let pin = g.pin_now().unwrap();
    let deleted = commit_query(&g, "MATCH (n:A) DELETE n");
    assert_eq!(labels(&deleted), (0, 1));
    let (stats, checks) = probe(&g, "CREATE (:A)");
    drop(pin);
    assert_eq!(stats.nodes_created, 1);
    assert_eq!(labels(&stats), (1, 0));
    assert_eq!(checks, 1);
}

#[test]
fn relabeled_stale_id_does_not_count() {
    let g = Graph::default();
    commit_query(&g, "CREATE (:A:B)");
    // A pin keeps the relabeled id's `A` posting in the index (ADR-0595
    // requeues it), so the probe really checks the stale id and skips it.
    let pin = g.pin_now().unwrap();
    assert_eq!(labels(&commit_query(&g, "MATCH (n:A) REMOVE n:A")), (0, 1));
    let (stats, _) = probe(&g, "CREATE (:A)");
    assert_eq!(labels(&stats), (1, 0));
    let (stats, _) = probe(&g, "MATCH (n:B) SET n:A");
    drop(pin);
    assert_eq!(labels(&stats), (1, 0));
}

#[test]
fn parity_with_the_census() {
    let g = Graph::default();
    seed(&g, "A", 2);
    seed(&g, "B", 1);
    let cases: [(&str, (i64, i64)); 11] = [
        ("CREATE (:New)", (1, 0)),
        ("CREATE (:A)", (0, 0)),
        ("CREATE (:New), (:New)", (1, 0)),
        ("CREATE (n:New) DELETE n", (0, 0)),
        ("MATCH (n:A) WITH n LIMIT 1 DELETE n", (0, 0)),
        ("MATCH (n:A) DELETE n", (0, 1)),
        ("MATCH (n:B) DELETE n", (0, 1)),
        ("MATCH (n:B) SET n:C", (1, 0)),
        ("MATCH (n:B) REMOVE n:B", (0, 1)),
        ("MATCH (n:A) DELETE n CREATE (:A)", (0, 0)),
        ("MATCH (n:B) DELETE n CREATE (:B)", (0, 0)),
    ];
    for (q, want) in cases {
        assert_eq!(labels(&probe(&g, q).0), want, "{q}");
    }
}

/// A scratch keeps answering at its own `snapshot_ts`, as an explicit
/// transaction does while other commits land (ADR-0171) — the pin such a
/// transaction holds is also what keeps its postings through ADR-0595's
/// drains.
#[test]
fn old_snapshot_ignores_later_commits() {
    let g = Graph::default();
    seed(&g, "Q", 1);
    let ts0 = g.commit_ts();
    let pin = g.pin_now().unwrap();
    commit_query(&g, "CREATE (:P)");
    commit_query(&g, "MATCH (n:Q) DELETE n");
    // `P` exists now but not at ts0; `Q` is gone now but was there at ts0.
    assert_eq!(labels(&probe_at(&g, ts0, "CREATE (:P)").0), (1, 0));
    assert_eq!(labels(&probe_at(&g, ts0, "CREATE (:Q)").0), (0, 0));
    assert_eq!(labels(&probe(&g, "CREATE (:P)").0), (0, 0));
    assert_eq!(labels(&probe(&g, "CREATE (:Q)").0), (1, 0));
    drop(pin);
}

/// `restore_scratch` (CALL … IN TRANSACTIONS) shares the memo, so a
/// restore keeps what the failed batch learned about the snapshot.
#[test]
fn restored_scratch_keeps_the_memo() {
    let g = Graph::default();
    seed(&g, "Seed", 100);
    let scratch = write_scratch(&g);
    let before = scratch.borrow().clone();
    take_label_probe_checks();
    let stats = exec_write(&g, &scratch, "CREATE (:Seed)").unwrap().stats;
    assert_eq!(labels(&stats), (0, 0));
    assert_eq!(take_label_probe_checks(), 1);
    *scratch.borrow_mut() = before;
    let stats = exec_write(&g, &scratch, "CREATE (:Seed)").unwrap().stats;
    assert_eq!(labels(&stats), (0, 0));
    assert_eq!(take_label_probe_checks(), 0);
}

/// Probes on an old snapshot run while another thread applies commits that
/// add the same label (`index_add` holds `nodes` then takes `label_index`).
/// The probe never nests the two guards, so this cannot deadlock, and every
/// answer stays at the snapshot.
#[test]
fn probes_overlap_concurrent_applies() {
    let g = Graph::default();
    seed(&g, "L", 2);
    let ts0 = g.commit_ts();
    let first_new = g.next_node_id();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            for i in 0..300 {
                let op = RedoOp::create_node(first_new + i, vec!["L".into()], BTreeMap::new());
                g.apply_redo(vec![op], g.commit_ts() + 1).unwrap();
            }
        });
        for _ in 0..300 {
            let (stats, _) = probe_at(&g, ts0, "MATCH (n:L) WHERE id(n) = 0 DELETE n");
            assert_eq!(labels(&stats), (0, 0));
            let (stats, _) = probe_at(&g, ts0, "MATCH (n:L) DELETE n");
            assert_eq!((stats.nodes_deleted, labels(&stats)), (2, (0, 1)));
        }
    });
}

/// ADR-0595: the removal sites suspect their dying postings and the same
/// apply's tail drain reclaims them, so a store that never checkpoints —
/// in-memory, or durable between checkpoints — does not accumulate them
/// and no later probe pays for the walk.
#[test]
fn an_applying_commit_reclaims_the_postings_it_killed() {
    let g = Graph::default();
    seed(&g, "L", 300);
    let lid = g.label_id("L").unwrap();
    assert_eq!(g.label_count_by_id(lid), 300);
    // The deleting commit suspects at the tombstone and drains at its
    // own tail: no probe walk, no checkpoint GC.
    commit_query(&g, "MATCH (n:L) DELETE n");
    assert_eq!(
        g.label_count_by_id(lid),
        0,
        "the deleting commit reclaimed the postings it killed"
    );
    // The ladder does not accumulate: create and clear again.
    commit_query(&g, "CREATE (:L)");
    assert_eq!(g.label_count_by_id(lid), 1);
    commit_query(&g, "MATCH (n:L) DELETE n");
    assert_eq!(g.label_count_by_id(lid), 0);
    // A label drop reclaims its posting too.
    commit_query(&g, "CREATE (:M)");
    let mid = g.label_id("M").unwrap();
    assert_eq!(g.label_count_by_id(mid), 1);
    commit_query(&g, "MATCH (n:M) REMOVE n:M");
    assert_eq!(
        g.label_count_by_id(mid),
        0,
        "the removing commit reclaimed the dropped posting"
    );
}

/// ADR-0595: the drain respects the snapshot horizon — a pin at an old
/// snapshot keeps every posting it can see, and the reclamation happens
/// on the first apply after the pin drops.
#[test]
fn a_pinned_snapshot_defers_the_reclamation() {
    let g = Graph::default();
    seed(&g, "L", 1);
    let pin = g.pin_now().unwrap();
    commit_query(&g, "MATCH (n:L) DELETE n");
    let lid = g.label_id("L").unwrap();
    assert_eq!(
        g.label_count_by_id(lid),
        1,
        "the pinned snapshot keeps the posting it can see"
    );
    drop(pin);
    // The requeued candidate is drained by the next applying commit —
    // no probe needs to walk it.
    commit_query(&g, "CREATE (:Other)");
    assert_eq!(
        g.label_count_by_id(lid),
        0,
        "the reclamation happened once the pin dropped"
    );
}

/// ADR-0595: a posting re-added inside the same apply as its removal
/// survives the drain — its live version carries the label again.
#[test]
fn a_readded_label_keeps_its_posting() {
    let g = Graph::default();
    commit_query(&g, "CREATE (:A:B)");
    let aid = g.label_id("A").unwrap();
    // One statement, one apply: the REMOVE suspects the posting, the SET
    // re-adds it, and the tail drain finds the live carrier.
    commit_query(&g, "MATCH (n:A) REMOVE n:A SET n:A");
    assert_eq!(g.label_count_by_id(aid), 1);
    // A later drain does not take it either — the candidate is gone.
    commit_query(&g, "CREATE (:Other)");
    assert_eq!(
        g.label_count_by_id(aid),
        1,
        "the re-added posting survives later drains"
    );
}

/// ADR-0595: the empty-label bucket reclaims in both directions — a
/// deleted label-less node's posting dies with it, and a first label
/// takes the node out of the bucket at the apply that adds it.
#[test]
fn the_empty_bucket_reclaims_in_both_directions() {
    let g = Graph::default();
    commit_query(&g, "CREATE (b)");
    let empty = g.label_id("").unwrap();
    assert_eq!(g.label_count_by_id(empty), 1);
    commit_query(&g, "MATCH (n) DELETE n");
    assert_eq!(
        g.label_count_by_id(empty),
        0,
        "a deleted label-less node's empty-bucket posting dies with it"
    );
    commit_query(&g, "CREATE (b)");
    assert_eq!(g.label_count_by_id(empty), 1);
    commit_query(&g, "MATCH (n) SET n:First");
    assert_eq!(
        g.label_count_by_id(empty),
        0,
        "a first label takes the node out of the empty bucket"
    );
    assert_eq!(g.label_count_by_id(g.label_id("First").unwrap()), 1);
}

/// ADR-0595: the empty bucket answers the pin horizon like a labelled
/// bucket — a pinned snapshot that can still see a deleted label-less
/// node keeps its posting until the pin drops.
#[test]
fn the_empty_bucket_respects_the_pin() {
    let g = Graph::default();
    commit_query(&g, "CREATE (b)");
    let empty = g.label_id("").unwrap();
    let pin = g.pin_now().unwrap();
    commit_query(&g, "MATCH (n) DELETE n");
    assert_eq!(
        g.label_count_by_id(empty),
        1,
        "the pinned snapshot keeps the empty-bucket posting it can see"
    );
    drop(pin);
    commit_query(&g, "CREATE (:Other)");
    assert_eq!(
        g.label_count_by_id(empty),
        0,
        "the empty-bucket reclamation happens once the pin drops"
    );
}
