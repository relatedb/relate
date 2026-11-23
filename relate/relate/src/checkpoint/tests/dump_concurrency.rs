//! The dump/commit-concurrency and closed-segment-backlog suite: what a
//! commit does while a checkpoint dump is in flight, and the budget's
//! back-pressure over the closed segments the dump recycles (ADR-0644).
//! Split from `tests.rs` (the no-1k-line rule); the `:memory:` recycle-only
//! checkpoints live in `recycle.rs`, the copy-on-write shard rewrite in
//! `rewrite.rs`.

use super::*;

use relate_storage::NativeVfs;
use std::sync::{mpsc, Arc};

#[test]
fn a_commit_that_lands_during_a_dump_neither_waits_nor_joins_its_cut() {
    let dir = tempfile::tempdir().unwrap();
    let (pause, dump_entered) = dump_pause();
    let db = pausing_store(Arc::new(NativeVfs::new(dir.path())), &pause);
    db.connect().unwrap().query("CREATE (:Prior)").unwrap();
    checkpoint_dump_in_flight(&db, &dump_entered);

    // The dump is provably in flight and holds no commit exclusion: this
    // commit must return without waiting for it.
    let (committed, committed_rx) = mpsc::channel();
    let writer = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || {
            db.connect().unwrap().query("CREATE (:During)").unwrap();
            committed.send(()).unwrap();
        })
    };
    let completed = committed_rx.recv_timeout(Duration::from_secs(5)).is_ok();
    // Release the held dump either way, so a regressed writer that did wait
    // on it can finish and the join cannot hang the suite; the assert
    // carries the verdict.
    pause.release.wait();
    writer.join().unwrap();
    assert!(
        completed,
        "the commit waited for the in-flight checkpoint dump"
    );
    assert_eq!(crate::test_opts::wal_lsn(&db).unwrap(), 2);

    wait_checkpoint_end(&db.shared());
    // The cut predates the commit: the dump published the prior record's
    // cut, and the commit stays in the WAL for the next checkpoint.
    assert_eq!(manifest_checkpoint_lsn(dir.path()), Some(1));
    assert_eq!(crate::test_opts::wal_lsn(&db).unwrap(), 2);
}

#[test]
fn a_commit_that_lands_during_a_dump_is_dumped_by_the_next_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let (pause, dump_entered) = dump_pause();
    let db = pausing_store(Arc::new(NativeVfs::new(dir.path())), &pause);
    db.connect().unwrap().query("CREATE (:Seed)").unwrap();
    checkpoint_dump_in_flight(&db, &dump_entered);

    // Lands after the dump's cut, so that dump cannot cover it.
    db.connect().unwrap().query("CREATE (:During)").unwrap();

    pause.release.wait();
    let shared = db.shared();
    wait_checkpoint_end(&shared);
    // The next checkpoint takes the marks the commit left behind, dumps
    // them, and recycles the segment that still carried the commit.
    db.checkpoint().unwrap();
    assert_eq!(crate::test_opts::wal_lsn(&db).unwrap(), 2);
    drop(shared);
    drop(db);

    let reopened =
        Database::open_vfs(Arc::new(NativeVfs::new(dir.path())), OpenOptions::default()).unwrap();
    let r = reopened
        .connect()
        .unwrap()
        .query("MATCH (n) RETURN count(n) AS n")
        .unwrap();
    assert_eq!(r.rows[0][0], Value::Int(2));
}

/// A dump that fails publishes nothing and fences nothing, and the ids its
/// cut took come back: the next checkpoint still covers the rows the failed
/// one would have.
#[test]
fn a_failed_dump_keeps_its_dirty_ids_and_fences_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path().to_str().unwrap()).unwrap();
    db.connect().unwrap().query("CREATE (:Seed)").unwrap();
    db.checkpoint().unwrap();
    let shared = db.shared();
    // Marked, then taken by a dump the graph's own validation rejects
    // before it writes.
    db.connect().unwrap().query("CREATE (:Second)").unwrap();
    let next = shared.graph.next_node_id();
    shared.graph.set_next_node_id(-1);
    assert!(db.checkpoint().is_err());
    shared.graph.set_next_node_id(next);
    // Nothing is fenced: a commit still lands.
    db.connect().unwrap().query("CREATE (:Third)").unwrap();
    db.checkpoint().unwrap();
    assert_eq!(manifest_checkpoint_lsn(dir.path()), Some(3));
    drop(shared);
    drop(db);

    // The second node's WAL record was recycled by the successful
    // checkpoint, so only the handed-back dirty id keeps it in the snapshot.
    let reopened =
        Database::open_vfs(Arc::new(NativeVfs::new(dir.path())), OpenOptions::default()).unwrap();
    let r = reopened
        .connect()
        .unwrap()
        .query("MATCH (n) RETURN count(n) AS n")
        .unwrap();
    assert_eq!(r.rows[0][0], Value::Int(3));
}

/// The timer's trigger fences no more than the explicit one: a dump the
/// graph's own validation rejects fails the timed checkpoint, publishes
/// nothing, and leaves every later commit free to land (ADR-0643 §4).
#[test]
fn a_failed_timed_dump_fences_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path().to_str().unwrap()).unwrap();
    db.connect().unwrap().query("CREATE (:Seed)").unwrap();
    db.checkpoint().unwrap();
    let shared = db.shared();
    db.connect().unwrap().query("CREATE (:Second)").unwrap();
    let published = manifest_checkpoint_lsn(dir.path());
    let next = shared.graph.next_node_id();
    shared.graph.set_next_node_id(-1);
    timed_checkpoint(&shared);
    shared.graph.set_next_node_id(next);
    assert_eq!(
        manifest_checkpoint_lsn(dir.path()),
        published,
        "the rejected dump published nothing, so the failure path ran"
    );
    // Nothing is fenced: a commit still lands and the store still checkpoints.
    db.connect().unwrap().query("CREATE (:Third)").unwrap();
    db.checkpoint().unwrap();
    assert_eq!(manifest_checkpoint_lsn(dir.path()), Some(3));
}

/// The load-path stall: a commit group that arrives while a checkpoint dump
/// is in flight must not wait the dump out.
///
/// Pre-fix, `flush_group` blocks in `admit_publication` until
/// `checkpoint_busy.busy` clears, so the group pays the whole dump. The VFS
/// holds the dump at its first write, so the dump is provably still in flight
/// while the group runs; the group's latency must stay a small multiple of the
/// same group's no-dump latency, not the dump's length.
#[test]
fn commit_group_does_not_wait_out_an_in_flight_checkpoint_dump() {
    const COMMITS: usize = 20;
    const LIMIT: u32 = 10;

    let (db, pause, dump_entered) = pausable_dump_db();
    // A committed node gives the checkpoint an active segment to rotate.
    db.connect().unwrap().query("CREATE (:Seed)").unwrap();

    // No-dump latency for the same group.
    let baseline = time_commit_group(&db, COMMITS);

    let checkpoint = start_held_dump(&db, &pause, dump_entered);

    let during = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || time_commit_group(&db, COMMITS))
    };
    // Hold the dump open for well over the group's own cost, so a group that
    // waits it out is unmistakable.
    std::thread::sleep((baseline * (2 * LIMIT)).max(Duration::from_millis(100)));
    pause.release.wait();

    let during = during.join().unwrap();
    checkpoint.join().unwrap();

    assert!(
        during <= baseline * LIMIT,
        "the commit group waited out the in-flight checkpoint dump: {COMMITS} commits took \
         {during:?} while the dump was held open vs {baseline:?} with no checkpoint \
         ({}x, limit {LIMIT}x)",
        during.as_secs_f64() / baseline.as_secs_f64()
    );
}

/// Close `count` WAL segments the cheap way — an empty replicated record and
/// a direct rotation each — while a dump is held open, so its recycle cannot
/// cover them and the store's closed-segment backlog grows without a commit
/// crossing a rotation threshold.
fn close_segments(db: &Database, count: usize) {
    let shared = db.shared();
    for _ in 0..count {
        advance_wal(db, shared.graph.commit_ts() + 1);
        shared.wal.lock().unwrap().rotate().unwrap();
    }
}

/// The budget's floor: with the closed-segment backlog under it, a commit
/// completes while a dump runs — it never waits for the dump.
#[test]
fn commits_complete_while_a_dump_runs_with_the_backlog_under_the_bound() {
    const WAIT: Duration = Duration::from_secs(5);

    let (db, pause, dump_entered) = pausable_dump_db();
    db.connect().unwrap().query("CREATE (:Seed)").unwrap();
    let checkpoint = start_held_dump(&db, &pause, dump_entered);
    // The dump's cut closed one segment; one more direct rotation keeps the
    // backlog under the budget.
    close_segments(&db, 1);
    assert!(
        db.shared().wal.lock().unwrap().closed_segments() <= CLOSED_SEGMENT_BUDGET,
        "the backlog must stay under the budget for this test"
    );

    let (done, completed) = mpsc::channel();
    let writer = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || {
            db.connect().unwrap().query("CREATE (:Under)").unwrap();
            done.send(()).unwrap();
        })
    };
    let completed_while_held = completed.recv_timeout(WAIT).is_ok();
    pause.release.wait();
    writer.join().unwrap();
    checkpoint.join().unwrap();

    assert!(
        completed_while_held,
        "a commit under the closed-segment budget waited for the dump"
    );
}

/// The budget's ceiling: over it, the writer waits for the in-flight dump
/// and its recycle instead of growing the WAL without bound, and completes
/// once the dump releases.
#[test]
fn a_writer_waits_when_the_closed_segment_backlog_exceeds_the_bound() {
    const SETTLE: Duration = Duration::from_millis(500);
    const RELEASE: Duration = Duration::from_secs(10);

    let (db, pause, dump_entered) = pausable_dump_db();
    db.connect().unwrap().query("CREATE (:Seed)").unwrap();
    let checkpoint = start_held_dump(&db, &pause, dump_entered);
    // Closing a segment takes a commit — [`Wal::rotate`] no-ops on an empty
    // segment — so the writer that carries the backlog past the budget is
    // itself the writer under test: its commit takes the back-pressure gate
    // the moment the backlog crosses `CLOSED_SEGMENT_BUDGET` and waits for
    // the held dump, which is the behavior this test pins.
    let (done, completed) = mpsc::channel();
    let writer = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || {
            close_segments(&db, CLOSED_SEGMENT_BUDGET + 1);
            done.send(()).unwrap();
        })
    };
    let waited = completed.recv_timeout(SETTLE).is_err();
    assert!(
        db.shared().wal.lock().unwrap().closed_segments() > CLOSED_SEGMENT_BUDGET,
        "the backlog must exceed the budget for this test"
    );
    // Release the held dump either way, so a regressed writer that did not
    // wait can still finish and the join cannot hang the suite; the asserts
    // carry the verdict.
    pause.release.wait();
    let finished = completed.recv_timeout(RELEASE).is_ok();
    writer.join().unwrap();
    checkpoint.join().unwrap();

    assert!(
        waited,
        "a commit completed while the closed-segment backlog was over the budget"
    );
    assert!(
        finished,
        "the writer never completed once the dump released"
    );
}
