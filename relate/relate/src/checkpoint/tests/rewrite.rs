//! The copy-on-write shard rewrite (ADR-0643 Decision 6, ADR-0183
//! Decision 4): an automatic, mid-load checkpoint rewrites only the shards
//! a dirty id seeds and publishes every clean part verbatim — no clean
//! shard is read. Split from `dump_concurrency.rs` (one theme per module).
//! Parquet-only, gated whole at its `mod` in `tests.rs`: every unit reads the
//! Parquet manifest's typed shard parts.

use super::*;

use relate_test_support::read_log::ReadLogVfs;

/// The published node parts of the store at `dir`, typed as the manifest
/// carries them (`ShardPart`: path, id window, rows, bytes).
fn node_parts(dir: &std::path::Path) -> Vec<relate_storage::ShardPart> {
    manifest(dir)
        .expect("the store published a manifest")
        .node_parts
}

/// ADR-0022 Decision 2: the automatic checkpoint is copy-on-write. A small
/// write to one shard rewrites that shard alone — the clean neighbour is
/// republished verbatim and its file is never opened for reading. A
/// coalescing dump (ADR-0183 Decision 2) absorbs the neighbour, reads it and
/// merges the two parts, which fails both assertions.
#[test]
fn automatic_checkpoint_rewrites_only_the_dirty_shard() {
    let dir = tempfile::tempdir().unwrap();
    let inner: Arc<dyn relate_storage::Vfs> = Arc::new(relate_storage::NativeVfs::new(dir.path()));
    let vfs = Arc::new(ReadLogVfs::new(inner));
    let db = Database::open_vfs(
        Arc::clone(&vfs) as Arc<dyn relate_storage::Vfs>,
        // The crate's one timer-off writable preset, for both opens: the VFS
        // argument is what distinguishes them.
        crate::test_opts::writable(),
    )
    .unwrap();
    {
        let mut c = db.connect().unwrap();
        c.query("CREATE (:Low {n: 1})").unwrap();
        // A second node far from id 0 lands in a window of its own — the
        // default target holds 262,144 ids a window — so the store gets two
        // node shards. Writes allocate from the shared counter.
        db.shared()
            .next_node_id
            .store(300_000, std::sync::atomic::Ordering::Relaxed);
        c.query("CREATE (:High {n: 2})").unwrap();
    }
    timed_checkpoint(&db.shared());
    let before = node_parts(dir.path());
    assert_eq!(before.len(), 2, "{before:?}");
    // Select by the id window each part covers — the manifest's parts are
    // the typed facts (path, window, rows, bytes), not list order.
    let dirty = before
        .iter()
        .find(|part| part.id_lo == 0)
        .expect("the low-id shard")
        .clone();
    let clean = before
        .iter()
        .find(|part| part.id_lo == 262_144)
        .expect("the high-id shard")
        .clone();

    db.connect()
        .unwrap()
        .query("MATCH (n:Low) SET n.n = 3")
        .unwrap();
    vfs.log().clear();
    timed_checkpoint(&db.shared());

    // The rewritten candidate is opened once for its preflight; of the
    // published shards, only the one holding the dirty id is read.
    let reread: Vec<String> = vfs
        .log()
        .reads_of("nodes/")
        .into_iter()
        .filter(|path| before.iter().any(|known| &known.path == path))
        .collect();
    assert_eq!(
        reread,
        vec![dirty.path.clone()],
        "only the shard holding the dirty id may be read"
    );
    let after = node_parts(dir.path());
    assert_eq!(after.len(), 2, "the clean shard must stay its own part");
    assert!(
        after.iter().any(|part| part.path == clean.path),
        "the clean shard is republished verbatim by path identity, got {after:?}"
    );
    assert!(
        !after.iter().any(|part| part.path == dirty.path),
        "the dirty shard is rewritten, got {after:?}"
    );
    db.leak_without_checkpoint();
    // Reopen over the same caller-supplied kind of store, as the first open
    // built it.
    let db = Database::open_vfs(
        Arc::new(relate_storage::NativeVfs::new(dir.path())),
        // The crate's one timer-off writable preset, for both opens: the VFS
        // argument is what distinguishes them.
        crate::test_opts::writable(),
    )
    .unwrap();
    let mut c = db.connect().unwrap();
    let r = c.query("MATCH (n) RETURN n.n AS n ORDER BY n.n").unwrap();
    assert_eq!(r.rows, vec![vec![Value::Int(2)], vec![Value::Int(3)]]);
}
