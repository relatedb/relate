//! The `:memory:` recycle-only checkpoints (ADR-0105): a store whose
//! snapshot VFS is not durable writes no snapshot — its automatic
//! checkpoints only recycle closed WAL segments, and a row committed into
//! the open segment survives a reopen through the WAL alone. Split from
//! `dump_concurrency.rs` (one theme per module).

use super::*;

use relate_storage::{MemoryVfs, Vfs};
use std::sync::Arc;

fn memory_wal_segments(db: &Database) -> Vec<String> {
    let mut names = db.shared().store.vfs().list_dir("wal").unwrap();
    names.sort();
    names
}

fn assert_checkpoint_idle(shared: &Shared) {
    let state = shared.checkpoint_busy.lock().unwrap();
    assert!(!state.busy);
}

/// The shared-VFS `:memory:` store the recycle-only tests reopen: no timer,
/// so only the checkpoint under test runs.
fn shared_memory_vfs() -> (Arc<dyn Vfs>, OpenOptions) {
    let opts = OpenOptions {
        checkpoint_interval: Some(Duration::ZERO),
        ..OpenOptions::default()
    };
    (Arc::new(MemoryVfs::new()), opts)
}

fn assert_memory_row_survives_reopen(vfs: Arc<dyn Vfs>, opts: OpenOptions) {
    let reopened = Database::open_vfs(vfs, opts).unwrap();
    let r = reopened
        .connect()
        .unwrap()
        .query("MATCH (n:Kept) RETURN count(n) AS n")
        .unwrap();
    assert_eq!(r.rows[0][0], Value::Int(1));
}

#[test]
fn memory_rotation_checkpoint_recycles_without_snapshot() {
    let (vfs, opts) = shared_memory_vfs();
    let db = Database::open_vfs(Arc::clone(&vfs), opts.clone()).unwrap();
    // Empty replicated commits reach the WAL's commit threshold cheaply,
    // closing the first segment; its recycle-only checkpoint deletes it.
    let rotated = (1..=50_000u64).any(|commit_ts| {
        advance_wal(&db, commit_ts);
        memory_wal_segments(&db).iter().any(|n| n == "00000002.log")
    });
    assert!(rotated, "WAL never rotated");
    // A row committed into the open segment is never checkpointed: the
    // recycle-only checkpoint recycles the closed segments without rotating
    // the open one into the recycle.
    db.connect().unwrap().query("CREATE (:Kept)").unwrap();
    let shared = db.shared();
    schedule_checkpoint(&shared);
    assert_checkpoint_idle(&shared);
    assert!(!shared.wal.lock().unwrap().has_closed_segments());
    assert_eq!(memory_wal_segments(&db), vec!["00000002.log".to_string()]);
    assert_eq!(db.snapshot_data_bytes().unwrap(), 0);
    db.leak_without_checkpoint();
    assert_memory_row_survives_reopen(vfs, opts);
}

#[test]
fn memory_automatic_checkpoint_recycles_without_snapshot() {
    let (vfs, opts) = shared_memory_vfs();
    let db = Database::open_vfs(Arc::clone(&vfs), opts.clone()).unwrap();
    db.connect().unwrap().query("CREATE (:Kept)").unwrap();
    let shared = db.shared();
    checkpoint_with(&shared, Trigger::Timed).unwrap();
    assert_checkpoint_idle(&shared);
    assert!(!shared.wal.lock().unwrap().has_closed_segments());
    assert_eq!(memory_wal_segments(&db), vec!["00000001.log".to_string()]);
    assert_eq!(db.snapshot_data_bytes().unwrap(), 0);
    db.leak_without_checkpoint();
    assert_memory_row_survives_reopen(vfs, opts);
}
