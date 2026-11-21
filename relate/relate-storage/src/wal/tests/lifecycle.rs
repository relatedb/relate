//! The append lifecycle tests: records-after reads across rotate and
//! recycle, the sync/barrier contract on file and bundle VFSes, and
//! rotate's fd and barrier behavior. Split from the root test module so
//! neither file creeps past the 1k-line boundary; the shared
//! frame-writing helpers stay there and arrive through the glob.

use super::*;

#[test]
fn records_after_limit_and_caught_up() {
    let (_dir, mut wal) = file_wal();
    append_n(&mut wal, 5);
    assert!(wal.records_after(0, 0).unwrap().is_empty());
    let page = wal.records_after(0, 2).unwrap();
    assert_eq!(page.iter().map(|r| r.lsn).collect::<Vec<_>>(), vec![1, 2]);
    let next = wal.records_after(2, 2).unwrap();
    assert_eq!(next.iter().map(|r| r.lsn).collect::<Vec<_>>(), vec![3, 4]);
    let last = wal.records_after(4, 8).unwrap();
    assert_eq!(last.iter().map(|r| r.lsn).collect::<Vec<_>>(), vec![5]);
    assert!(wal.records_after(5, 8).unwrap().is_empty());
    assert!(wal.records_after(99, 8).unwrap().is_empty());
}

#[test]
fn records_after_skips_closed_segments() {
    let (dir, mut wal) = file_wal();
    append_n(&mut wal, 4);
    wal.rotate().unwrap();
    append_n(&mut wal, 3);
    assert!(dir.path().join("00000001.log").exists());
    assert!(dir.path().join("00000002.log").exists());
    let tail = wal.records_after(4, 16).unwrap();
    assert_eq!(
        tail.iter().map(|r| r.lsn).collect::<Vec<_>>(),
        vec![5, 6, 7]
    );
    let mid = wal.records_after(2, 2).unwrap();
    assert_eq!(mid.iter().map(|r| r.lsn).collect::<Vec<_>>(), vec![3, 4]);
    let across = wal.records_after(3, 3).unwrap();
    assert_eq!(
        across.iter().map(|r| r.lsn).collect::<Vec<_>>(),
        vec![4, 5, 6]
    );
}

#[test]
fn records_after_hint_survives_rotate() {
    let (_dir, mut wal) = file_wal();
    append_n(&mut wal, 3);
    let first = wal.records_after(0, 3).unwrap();
    assert_eq!(first.last().map(|r| r.lsn), Some(3));
    wal.rotate().unwrap();
    append_n(&mut wal, 2);
    let next = wal.records_after(3, 16).unwrap();
    assert_eq!(next.iter().map(|r| r.lsn).collect::<Vec<_>>(), vec![4, 5]);
}

#[test]
fn records_after_after_recycle_of_hinted_segment() {
    let (dir, mut wal) = file_wal();
    append_n(&mut wal, 3);
    let first = wal.records_after(0, 3).unwrap();
    assert_eq!(first.last().map(|r| r.lsn), Some(3));
    let cut = wal.rotate().unwrap();
    append_n(&mut wal, 2);
    wal.recycle(cut).unwrap();
    assert!(!dir.path().join("00000001.log").exists());
    let rest = wal.records_after(3, 16).unwrap();
    assert_eq!(rest.iter().map(|r| r.lsn).collect::<Vec<_>>(), vec![4, 5]);
}

/// Durability barrier (ADR-0077), the one pin run for both VFS backends:
/// a synced record must survive the handle being dropped and the store
/// reopened, replaying intact — lsn, commit timestamp and ops. The bundle
/// arm runs the same assertion set as the file arm; only the backend
/// differs.
fn assert_synced_record_survives_reopen(open_store: impl Fn() -> Wal) {
    let ops = vec![RedoOp::DeleteNode { id: 7 }, RedoOp::DeleteRel { id: 9 }];
    {
        let mut wal = open_store();
        assert_eq!(wal.append_commit(42, &ops).unwrap(), 1);
        wal.sync().unwrap();
    } // handle closed here; only data flushed by the barrier can be replayed

    let reopened = open_store();
    assert_eq!(reopened.lsn, 1);
    let recs = reopened.replay(0).unwrap();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].lsn, 1);
    assert_eq!(recs[0].commit_ts, 42);
    assert_eq!(recs[0].ops, ops);
}

#[test]
fn sync_preserves_record_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    assert_synced_record_survives_reopen(|| {
        Wal::open(Arc::new(NativeVfs::new(dir.path())), "", 0).unwrap()
    });
}

#[test]
fn bundle_sync_preserves_record_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.rl");
    assert_synced_record_survives_reopen(|| {
        Wal::open(
            Arc::new(BundleVfs::open_or_create(&path).unwrap()),
            "wal",
            0,
        )
        .unwrap()
    });
}

#[test]
fn bundle_rotate_after_sync_does_not_add_a_second_barrier() {
    let (_dir, _vfs, mut wal) = bundle_wal();
    for i in 0..3 {
        wal.append_record(i + 1, &[]).unwrap();
    }
    assert_eq!(wal.barrier_calls, 0);
    wal.sync().unwrap();
    let after_sync = wal.barrier_calls;
    assert_eq!(after_sync, 1);
    wal.rotate().unwrap();
    assert_eq!(wal.barrier_calls, after_sync);
}

#[test]
fn bundle_recycle_deletes_virtual_segment() {
    let (_dir, vfs, mut wal) = bundle_wal();
    wal.append_commit(7, &[]).unwrap();
    let lsn = wal.rotate().unwrap();
    assert!(vfs.exists("wal/00000001.log").unwrap());
    assert!(vfs.exists("wal/00000002.log").unwrap());
    wal.recycle(lsn).unwrap();
    assert!(!vfs.exists("wal/00000001.log").unwrap());
    assert!(vfs.exists("wal/00000002.log").unwrap());
}

/// `sync` reports the barrier's elapsed time for the file backend and is
/// safe to call repeatedly (group commit syncs once per batch).
#[test]
fn file_sync_reports_timing() {
    let (_dir, mut wal) = file_wal();
    let stats = wal.append_commit_with_stats(1, &[]).unwrap();
    let again = wal.sync().unwrap();
    // A barrier with no new data is still legal and cheap; guard only
    // against a runaway (busy-wait / doubled barrier) in the hundreds of
    // seconds.
    assert!(
        stats.sync_ns < 600 * 1_000_000_000,
        "commit sync took {} ns",
        stats.sync_ns
    );
    assert!(again < 600 * 1_000_000_000, "sync took {again} ns");
}

#[test]
fn rotate_after_sync_does_not_add_a_second_barrier() {
    let (_dir, mut wal) = file_wal();
    // Group-commit appends: no barrier per record.
    for i in 0..3 {
        wal.append_record(i + 1, &[]).unwrap();
    }
    assert!(!wal.synced, "appends leave the open segment unsynced");
    assert_eq!(wal.barrier_calls, 0);
    wal.sync().unwrap();
    assert!(wal.synced);
    let after_sync = wal.barrier_calls;
    assert_eq!(after_sync, 1, "the group's commit barrier");
    wal.rotate().unwrap();
    assert_eq!(
        wal.barrier_calls, after_sync,
        "rotate after sync must not issue a second barrier"
    );
    assert!(!wal.synced, "the fresh segment's magic is not yet durable");
}

/// A rotate with no intervening `sync` must still barrier: the
/// checkpoint/manual-rotate paths do not sync first, so the segment's
/// appended bytes must be flushed by the rotate itself.
#[test]
fn rotate_without_sync_still_barriers() {
    let (_dir, mut wal) = file_wal();
    wal.append_record(1, &[]).unwrap();
    assert_eq!(wal.barrier_calls, 0);
    wal.rotate().unwrap();
    assert_eq!(wal.barrier_calls, 1);
}

/// Regression pin: the in-process WAL has no stable storage behind it, so
/// `sync` must stay a free no-op reporting 0 ns.
#[test]
fn memory_sync_is_noop() {
    let mut wal = Wal::memory();
    let stats = wal
        .append_commit_with_stats(1, &[RedoOp::DeleteNode { id: 1 }])
        .unwrap();
    assert_eq!(stats.sync_ns, 0);
    assert_eq!(wal.sync().unwrap(), 0);
    assert_eq!(wal.sync().unwrap(), 0);
}

/// Regression pin for the Windows checkpoint failures (os error 5):
/// `recycle` must never delete a segment whose handle is still open.
/// POSIX can't observe that failure mode (unlink of an open file
/// succeeds), so pin the invariant directly: after `rotate`, no fd in
/// this process references the rotated-out segment.
#[cfg(target_os = "linux")]
#[test]
fn rotate_leaves_no_open_fd_on_closed_segment() {
    let (dir, mut wal) = file_wal();
    append_n(&mut wal, 3);
    let seg1 = std::fs::canonicalize(dir.path().join("00000001.log")).unwrap();
    let cut = wal.rotate().unwrap();
    let still_open: Vec<_> = std::fs::read_dir("/proc/self/fd")
        .unwrap()
        .filter_map(|e| e.ok())
        .filter_map(|e| std::fs::read_link(e.path()).ok())
        .filter(|target| *target == seg1)
        .collect();
    assert!(
        still_open.is_empty(),
        "rotated segment still has open fds: {still_open:?}"
    );
    wal.recycle(cut).unwrap();
    assert!(!seg1.exists());
}
