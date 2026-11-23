//! Commit-path tests for group flushing, preflight, and safe horizons.

use super::super::*;
use super::harness::*;
use crate::{Database, Value};
use relate_executor::BatchCommit;
use std::collections::BTreeMap;

#[test]
fn group_flush_shares_one_fsync() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_str().unwrap();
    let db = Database::open(path).unwrap();
    db.enable_commit_timing();
    db.clear_commit_timing();
    let shared = db.shared();
    let n = 12usize;
    let waiters: Vec<Arc<CommitWaiter>> = (0..n).map(|_| CommitWaiter::new()).collect();
    let batch: Vec<_> = (0..n)
        .map(|i| slot(node_id(i), ordinal(i) + 1, 0, &waiters[i]))
        .collect();
    flush_group(&shared, batch).unwrap();
    for waiter in &waiters {
        waiter.wait().unwrap();
    }
    let buf = shared.timing.lock().unwrap();
    assert_eq!(
        buf.sync_calls, 1,
        "one group must issue exactly one sync_all"
    );
    assert_eq!(buf.samples.len(), n, "one sync sample per record");
    drop(buf);
    let mut c = db.connect().unwrap();
    let r = c.query("MATCH (n:N) RETURN count(n) AS n").unwrap();
    assert_eq!(r.rows[0][0], Value::Int(n as i64));
}

/// Disjoint creates from racing connections must all commit. The exact
/// number of fsyncs depends on how threads happen to batch, so this only
/// asserts deterministic outcomes: no thread panics and the exact final
/// node count.
#[test]
fn concurrent_disjoint_creates_all_commit() {
    use std::sync::Arc;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_str().unwrap();
    let db = Arc::new(Database::open(path).unwrap());
    let n = 12usize;
    spawn_person_create_storm(&db, n);
    let mut c = db.connect().unwrap();
    let r = c.query("MATCH (n:Person) RETURN count(n) AS n").unwrap();
    assert_eq!(r.rows[0][0], Value::Int(n as i64));
}

#[test]
fn group_committed_records_replay_after_crash() {
    use std::sync::{Arc, Barrier};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_str().unwrap();
    let n = 8usize;
    {
        let db = Arc::new(Database::open(path).unwrap());
        let barrier = Arc::new(Barrier::new(n));
        let mut hs = Vec::new();
        for i in 0..n {
            let db = Arc::clone(&db);
            let barrier = Arc::clone(&barrier);
            hs.push(std::thread::spawn(move || {
                barrier.wait();
                let mut c = db.connect().unwrap();
                c.query(&format!("CREATE (n:Person {{name: 'p{i}'}})"))
                    .unwrap();
            }));
        }
        for h in hs {
            h.join().unwrap();
        }
        match Arc::try_unwrap(db) {
            Ok(db) => db.leak_without_checkpoint(),
            Err(_) => panic!("connections still live"),
        }
    }
    let db = Database::open(path).unwrap();
    let mut c = db.connect().unwrap();
    let r = c.query("MATCH (n:Person) RETURN count(n) AS n").unwrap();
    assert_eq!(r.rows[0][0], Value::Int(n as i64));
}

/// An invalid later slot rejects all eligible waiters before any frame.
#[test]
fn applicability_preflight_completes_all_waiters_in_batch() {
    use std::collections::BTreeMap;

    let db = Database::open(":memory:").unwrap();
    let shared = db.shared();

    // A bad op: CreateRel references node ids that don't exist, so
    // ordered validation rejects it before WAL append.
    let bad = RedoOp::CreateRel {
        id: 1,
        start: 9_999,
        end: 9_998,
        rel_type: "KNOWS".into(),
        properties: relate_core::PropertyMap::share(BTreeMap::new()),
    };
    let good = RedoOp::CreateNode {
        id: 1,
        labels: Arc::new(vec!["P".into()]),
        properties: relate_core::PropertyMap::share(BTreeMap::new()),
    };

    // Batch: good, bad, good — the middle one fails preflight.
    let waiters: Vec<Arc<CommitWaiter>> = (0..3).map(|_| CommitWaiter::new()).collect();
    let mut batch = Vec::new();
    for (i, op) in [good.clone(), bad, good].into_iter().enumerate() {
        batch.push(slot_with_ops(vec![op], ordinal(i) + 1, 0, &waiters[i]));
    }

    let res = flush_group(&shared, batch);
    assert!(res.is_err(), "the bad CreateRel must fail preflight");
    assert_eq!(shared.wal.lock().unwrap().lsn, 0);
    assert!(shared.graph.all_node_ids().is_empty());

    // Every waiter's result slot must be filled (no hang on `wait()`).
    for (i, w) in waiters.iter().enumerate() {
        let filled = w.result.lock().unwrap().is_some();
        assert!(filled, "waiter {i} was not completed — would hang");
    }
}

#[test]
fn oversized_commit_is_refused_at_admission_and_leaves_wal_and_graph_intact_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    // A 1 MiB cap (ADR-0650's knob) keeps the oversized payload cheap: the
    // property below is exactly 1 MiB, and one op's record crosses it.
    let db = Database::open_with(
        path.to_str().unwrap(),
        crate::OpenOptions {
            wal_payload_limit: Some(1 << 20),
            ..Default::default()
        },
    )
    .unwrap();
    let shared = db.shared();
    // The ops are encoded once at admission, so the payload cap refuses an
    // oversized commit before it joins the queue: nothing reaches the WAL
    // and the commit before it stays durable.
    try_wal_commit(&shared, 0, 1, vec![node_op(0)], 0, 0).unwrap();
    let wal_file = path.join("wal/00000001.log");
    let before = std::fs::read(&wal_file).unwrap();
    let big = RedoOp::CreateNode {
        id: 1,
        labels: Arc::new(vec!["N".into()]),
        properties: relate_core::PropertyMap::share(BTreeMap::from([(
            "big".into(),
            Value::String("x".repeat(1 << 20).into()),
        )])),
    };
    let error = try_wal_commit(&shared, 0, 2, vec![big], 0, 0).unwrap_err();
    // ADR-0494: the refusal is the capacity family's WalPayload kind, its
    // communicative line naming the WAL record and the cap (both figures
    // in MiB).
    assert!(
        matches!(
            error,
            relate_core::Error::Capacity {
                kind: relate_core::CapacityKind::WalPayload,
                ..
            }
        ),
        "{error:?}"
    );
    assert!(error.to_string().contains("single WAL record"), "{error}");
    assert!(error.to_string().contains("1 MB"), "{error}");
    assert_eq!(shared.wal.lock().unwrap().lsn, 1);
    assert_eq!(std::fs::read(&wal_file).unwrap(), before);
    assert_eq!(shared.graph.all_node_ids().len(), 1);
    drop(shared);
    db.leak_without_checkpoint();
    let reopened = Database::open(path.to_str().unwrap()).unwrap();
    assert_eq!(reopened.inner.graph.all_node_ids().len(), 1);
}

#[test]
fn two_valid_candidates_share_exactly_one_barrier_and_keep_order() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path().to_str().unwrap()).unwrap();
    db.enable_commit_timing();
    let shared = db.shared();
    let waiters: Vec<_> = (0..2).map(|_| CommitWaiter::new()).collect();
    let batch: Vec<_> = (0..2)
        .map(|i| slot(node_id(i), ordinal(i) + 1, 0, &waiters[i]))
        .collect();
    flush_group(&shared, batch).unwrap();
    // Each waiter learns the commit timestamp its own slot was assigned.
    for (i, waiter) in waiters.iter().enumerate() {
        assert_eq!(waiter.wait().unwrap(), ordinal(i) + 1);
    }
    assert_eq!(shared.timing.lock().unwrap().sync_calls, 1);
    assert_eq!(
        shared
            .wal
            .lock()
            .unwrap()
            .replay(0)
            .unwrap()
            .iter()
            .map(|r| r.commit_ts)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(shared.graph.all_node_ids(), [0, 1]);
}

#[test]
fn group_and_replica_share_safe_horizon_allocators_and_checkpoint_pin() {
    for replica in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();
        let db = untimed_file_db(path);
        let shared = db.shared();
        let ops = vec![
            node_op(0),
            node_op(1),
            RedoOp::CreateRel {
                id: 0,
                start: 0,
                end: 1,
                rel_type: "LINK".into(),
                properties: relate_core::PropertyMap::share(BTreeMap::new()),
            },
        ];
        // Force this one commit to close its segment and schedule the
        // checkpoint, without writing thousands of unrelated records.
        shared.wal.lock().unwrap().bytes = 64 * 1024 * 1024;
        if replica {
            assert_eq!(db.apply_replicated(7, ops).unwrap(), 1);
        } else {
            let waiter = CommitWaiter::new();
            let candidate = slot_with_ops(ops, 7, 0, &waiter);
            flush_group(&shared, vec![candidate]).unwrap();
            waiter.wait().unwrap();
        }
        assert_eq!(shared.safe_lsn.load(Ordering::Acquire), 1);
        assert_eq!(shared.safe_ts.load(Ordering::Acquire), 7);
        assert_eq!(shared.graph.commit_ts(), 7);
        assert!(shared.next_node_id.load(Ordering::Relaxed) >= 2);
        assert!(shared.next_rel_id.load(Ordering::Relaxed) >= 1);
        assert!(shared.next_txn_id.load(Ordering::Relaxed) >= 8);
        crate::checkpoint::wait_checkpoint_idle(&shared).unwrap();
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["checkpoint_lsn"], 1);
        assert_eq!(manifest["commit_ts"], 7);
        drop(shared);
        db.leak_without_checkpoint();
        let reopened = untimed_file_db(path);
        assert_eq!(
            reopened
                .connect()
                .unwrap()
                .query("MATCH (n:N)-[:LINK]->(m:N) RETURN n, m")
                .unwrap()
                .rows
                .len(),
            1
        );
    }
}

/// The per-batch commit sink (ADR-0210) must hand back the commit
/// timestamp its own slot was assigned — never a re-read of the live
/// commit clock, which a foreign commit can advance after the publication
/// guard is released (the scratch would then rebase past another
/// transaction's writes). Pinned here at the source: successive batch
/// commits return exactly the assigned, strictly increasing timestamps.
#[test]
fn batch_commit_returns_its_own_assigned_commit_ts() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path().to_str().unwrap()).unwrap();
    let shared = db.shared();
    let sink = GroupBatchCommit::new(Arc::clone(&shared));
    let ts1 = sink.commit_batch(vec![node_op(0)], 0).unwrap();
    assert_eq!(ts1, shared.graph.commit_ts());
    // A foreign commit in between must not leak into the next batch's
    // returned timestamp base: the sink returns what ITS slot was
    // assigned, whatever the clock reads afterwards.
    let ts2 = sink.commit_batch(vec![node_op(1)], ts1).unwrap();
    assert_eq!(ts2, ts1 + 1);
    assert_eq!(ts2, shared.graph.commit_ts());
    assert_eq!(shared.graph.all_node_ids(), [0, 1]);
}

/// ADR-0439's group overlay is wired where the commit loop merges a checked
/// slot's delta: `admit_slot` calls `ConstraintAdmission::accept`, and the
/// hold it records is what rejects a later slot of the same group. The
/// property harness drives `check_with_earlier` plus `accept` itself, so it
/// cannot observe this wiring — dropping the `accept` merge leaves that
/// harness, and the rest of this suite, green.
///
/// This test drives a real two-slot batch through `flush_group` (hand-built
/// slots are its deterministic form: a scheduler-driven auto-commit or
/// explicit-transaction group cannot be forced to coalesce its slots, which
/// is why the race tests above retry instead of pinning one batch). Slot 0
/// creates `(:Person {email: 'dup'})` and must be accepted, so its
/// introduction enters the group overlay; slot 1 introduces the same tuple
/// and must be rejected with the retryable `Error::Conflict`. If the accept
/// merge is a no-op, slot 1 sees no overlay hold, both records commit, and
/// the assertions below fail.
#[test]
fn group_overlay_hold_from_an_earlier_slot_rejects_a_later_duplicate() {
    let db = person_email_db();
    let shared = db.shared();
    // Both statements pin before either commits: the shared snapshot is the
    // state the constraint was installed at, so only the group admission can
    // separate the two introductions.
    let snapshot_ts = shared.graph.commit_ts();
    // The constraint's own DDL record already owns an LSN; the group adds at
    // most one more.
    let lsn_before = shared.wal.lock().unwrap().lsn;
    let waiters: Vec<_> = (0..SIDES).map(|_| CommitWaiter::new()).collect();
    let batch: Vec<_> = (0..SIDES)
        .map(|i| {
            // The slot encodes the ops it carries, as admission does.
            slot_with_ops(
                vec![RedoOp::CreateNode {
                    id: node_id(i),
                    labels: Arc::new(vec!["Person".into()]),
                    properties: relate_core::PropertyMap::share(BTreeMap::from([(
                        "email".into(),
                        Value::String("dup".into()),
                    )])),
                }],
                ordinal(i) + 1,
                snapshot_ts,
                &waiters[i],
            )
        })
        .collect();
    flush_group(&shared, batch).unwrap();
    waiters[0]
        .wait()
        .expect("the first slot is accepted and its hold enters the overlay");
    assert_eq!(
        waiters[1].wait().unwrap_err(),
        Error::Conflict,
        "the earlier accepted slot's hold must reject the later duplicate"
    );
    // Only the accepted record reached the WAL and the graph.
    assert_eq!(shared.wal.lock().unwrap().lsn, lsn_before + 1);
    assert_eq!(shared.graph.all_node_ids(), [0]);
    assert_eq!(count(&db, "MATCH (n:Person) RETURN count(n)"), 1);
}

/// A dense growth rung is charged once per group, never once per
/// group-mate (REL8-1785). The committed span cannot advance mid-group, so
/// a walk that re-seeds each slot's estimate from it re-charges the same
/// rung on every mate crossing it — near the logical cap a KB-scale write
/// is then refused with `Error::Capacity` on a group-mate's multi-MB
/// reservation (the fraud loadtest's intermittent capacity errors). The
/// walk instead carries the spans its accepted slots advanced to, so the
/// mate places `Inside` the reserved rung.
///
/// Hand-built slots are the deterministic two-slot group (the overlay test
/// above), and the limit is positioned by
/// [`bounded_crossing_store`](super::harness::bounded_crossing_store)'s
/// probe: it admits the crossing plus half its cost of slack, and a second
/// charge of the same rung busts it.
#[test]
fn group_charges_a_span_rung_once_not_once_per_mate() {
    let rel = |id: i64| RedoOp::CreateRel {
        id,
        start: 0,
        end: 1,
        rel_type: "KNOWS".into(),
        properties: relate_core::PropertyMap::share(BTreeMap::new()),
    };
    let (db, shared) = bounded_crossing_store(vec![node_op(0), node_op(1)], rel(60_000));
    let snapshot = shared.graph.commit_ts();
    let waiters: Vec<_> = (0..2).map(|_| CommitWaiter::new()).collect();
    let batch: Vec<_> = [rel(60_000), rel(60_001)]
        .into_iter()
        .enumerate()
        .map(|(i, op)| slot_with_ops(vec![op], ordinal(i) + 2, snapshot, &waiters[i]))
        .collect();
    flush_group(&shared, batch).unwrap();
    waiters[0]
        .wait()
        .expect("the crossing slot's reservation fits the limit");
    waiters[1].wait().expect(
        "the mate places inside the rung the crossing reserved: it pays its own record, not the rung again",
    );
    assert_eq!(count(&db, "MATCH ()-[r:KNOWS]->() RETURN count(r)"), 2);

    // The limit still refuses a genuinely new rung — and a refused slot
    // must not advance the group's seed either: in one group, the write
    // crossing the new rung past the headroom is refused, and the mate
    // placing inside the rung that refused write would have reserved is
    // refused too, because it re-charges from the committed span instead
    // of riding a phantom reservation.
    let snapshot = shared.graph.commit_ts();
    let waiters: Vec<_> = (0..2).map(|_| CommitWaiter::new()).collect();
    let batch: Vec<_> = [rel(200_000), rel(190_000)]
        .into_iter()
        .enumerate()
        .map(|(i, op)| slot_with_ops(vec![op], ordinal(i) + 4, snapshot, &waiters[i]))
        .collect();
    flush_group(&shared, batch).unwrap();
    assert!(
        matches!(waiters[0].wait().unwrap_err(), Error::Capacity { .. }),
        "a write crossing a new rung past the headroom must stay refused"
    );
    assert!(
        matches!(waiters[1].wait().unwrap_err(), Error::Capacity { .. }),
        "the mate must not place inside a refused slot's phantom rung"
    );
    assert_eq!(count(&db, "MATCH ()-[r:KNOWS]->() RETURN count(r)"), 2);
}

/// The adjacency tier's dense rungs follow the same one-charge-per-group
/// rule (REL8-1785): the committed adjacency span cannot advance mid-group,
/// so a walk that re-seeds each slot's side admission from it re-charges
/// the same rung on every mate crossing it — the same intermittent
/// `Error::Capacity` class the chain spans had. The walk's seed carries the
/// adjacency spans too, so the mate places inside the reserved rung.
///
/// The endpoints sit at committed high node ids no rel touches (the node
/// chain spans are already past them, so only the adjacency term crosses),
/// and the rel ids stay low so the rel chain spans never move. Same
/// [`bounded_crossing_store`](super::harness::bounded_crossing_store)
/// probe as the chain-span test above.
#[test]
fn group_charges_an_adjacency_rung_once_not_once_per_mate() {
    let rel = |id: i64, start: i64| RedoOp::CreateRel {
        id,
        start,
        end: 0,
        rel_type: "KNOWS".into(),
        properties: relate_core::PropertyMap::share(BTreeMap::new()),
    };
    let base_ops = vec![
        node_op(0),
        node_op(60_000),
        node_op(60_001),
        node_op(190_000),
        node_op(200_000),
    ];
    let (db, shared) = bounded_crossing_store(base_ops, rel(0, 60_000));
    let snapshot = shared.graph.commit_ts();
    let waiters: Vec<_> = (0..2).map(|_| CommitWaiter::new()).collect();
    let batch: Vec<_> = [rel(0, 60_000), rel(1, 60_001)]
        .into_iter()
        .enumerate()
        .map(|(i, op)| slot_with_ops(vec![op], ordinal(i) + 2, snapshot, &waiters[i]))
        .collect();
    flush_group(&shared, batch).unwrap();
    waiters[0]
        .wait()
        .expect("the crossing slot's reservation fits the limit");
    waiters[1].wait().expect(
        "the mate places inside the adjacency rung the crossing reserved: it pays its own record, not the rung again",
    );
    assert_eq!(count(&db, "MATCH ()-[r:KNOWS]->() RETURN count(r)"), 2);

    // A genuinely new adjacency rung stays refused, and a refused slot
    // leaves no phantom rung: the mate re-charges from the committed span
    // instead of riding the refused write's reservation, so it is refused
    // too.
    let snapshot = shared.graph.commit_ts();
    let waiters: Vec<_> = (0..2).map(|_| CommitWaiter::new()).collect();
    let batch: Vec<_> = [rel(2, 200_000), rel(3, 190_000)]
        .into_iter()
        .enumerate()
        .map(|(i, op)| slot_with_ops(vec![op], ordinal(i) + 4, snapshot, &waiters[i]))
        .collect();
    flush_group(&shared, batch).unwrap();
    assert!(
        matches!(waiters[0].wait().unwrap_err(), Error::Capacity { .. }),
        "a write crossing a new adjacency rung past the headroom must stay refused"
    );
    assert!(
        matches!(waiters[1].wait().unwrap_err(), Error::Capacity { .. }),
        "the mate must not place inside a refused slot's phantom adjacency rung"
    );
    assert_eq!(count(&db, "MATCH ()-[r:KNOWS]->() RETURN count(r)"), 2);
}
