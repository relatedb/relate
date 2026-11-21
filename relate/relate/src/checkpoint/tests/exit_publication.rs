//! The exit-publication suite (ADR-0646): what a close-path checkpoint
//! writes — nothing at all for an unchanged session over a published
//! snapshot, exactly a dump for a session that committed or replayed a
//! pending tail, and the first dump for a bootstrap that never
//! checkpointed — and the explicit `checkpoint()` boundary that always
//! writes. Split from `tests.rs` (the no-1k-line rule).

use super::*;

/// The markdown-codec open options every test in this suite opens with.
fn md_opts() -> crate::OpenOptions {
    crate::OpenOptions {
        snapshot_codec: Some(SnapshotCodecKind::Markdown),
        ..Default::default()
    }
}

/// A store's published image, stamped for the skip tests: the bytes
/// and the modification time the skip must leave untouched (a rewrite
/// — even of identical bytes through the tmp/rename publication —
/// stamps a fresh mtime).
fn published_image(path: &std::path::Path) -> (Vec<u8>, std::time::SystemTime) {
    let bytes = std::fs::read(path).unwrap();
    let mtime = std::fs::metadata(path).unwrap().modified().unwrap();
    (bytes, mtime)
}

/// The exit checkpoint of an unchanged session over a published
/// snapshot writes nothing: no re-render, no manifest rewrite, not
/// even a same-content re-publication. Proven on the markdown codec —
/// whose exit checkpoint otherwise re-walks and re-renders every vault
/// file — by the sidecar staying byte- and mtime-identical.
#[test]
fn unchanged_exit_keeps_the_published_markdown_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vault");
    let path_str = path.to_str().unwrap();
    {
        let db = Database::open_with(path_str, md_opts()).unwrap();
        db.connect()
            .unwrap()
            .query("CREATE (n:Person {name: 'Ada'})")
            .unwrap();
    }
    let sidecar = path.join(".relate").join("manifest.json");
    let before = published_image(&sidecar);
    {
        let db = Database::open_with(path_str, md_opts()).unwrap();
        let r = db
            .connect()
            .unwrap()
            .query("MATCH (n:Person) RETURN n.name AS name")
            .unwrap();
        assert_eq!(r.rows[0][0].as_string(), Some("Ada"));
    }
    assert_eq!(
        published_image(&sidecar),
        before,
        "an unchanged exit checkpoint must not rewrite the sidecar"
    );
    assert!(
        !path.join(".relate").join("checkpoint-journal").exists(),
        "an unchanged exit checkpoint must not stage a journal"
    );
}

/// The same skip on the Parquet codec's root manifest.
#[cfg(feature = "parquet")]
#[test]
fn unchanged_exit_keeps_the_published_parquet_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_str().unwrap();
    {
        let db = Database::open(path).unwrap();
        db.connect()
            .unwrap()
            .query("CREATE (n:Person {name: 'Ada'})")
            .unwrap();
    }
    let manifest = dir.path().join("manifest.json");
    let before = published_image(&manifest);
    {
        let db = Database::open(path).unwrap();
        let r = db
            .connect()
            .unwrap()
            .query("MATCH (n:Person) RETURN n.name AS name")
            .unwrap();
        assert_eq!(r.rows[0][0].as_string(), Some("Ada"));
    }
    assert_eq!(
        published_image(&manifest),
        before,
        "an unchanged exit checkpoint must not rewrite the manifest"
    );
}

/// The skip never swallows a real write's publication: a session that
/// commits still exits through a dump, and the reopened store sees
/// both sessions' nodes.
#[test]
fn written_exit_publishes_over_the_skip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_str().unwrap();
    {
        let db = Database::open_with(path, md_opts()).unwrap();
        db.connect()
            .unwrap()
            .query("CREATE (n:Person {name: 'Ada'})")
            .unwrap();
    }
    {
        let db = Database::open_with(path, md_opts()).unwrap();
        db.connect()
            .unwrap()
            .query("CREATE (n:Person {name: 'Grace'})")
            .unwrap();
    }
    let db = Database::open_with(path, md_opts()).unwrap();
    let r = db
        .connect()
        .unwrap()
        .query("MATCH (n:Person) RETURN count(n) AS n")
        .unwrap();
    assert_eq!(r.rows[0][0], Value::Int(2));
}

/// A replayed tail still publishes at exit: the WAL predicate the
/// skip consults stays true until a dump has covered the leaked
/// commits, so the exit checkpoint runs over the published image and
/// advances it.
#[test]
fn replayed_tail_still_publishes_at_exit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_str().unwrap();
    {
        let db = Database::open(path).unwrap();
        db.connect()
            .unwrap()
            .query("CREATE (n:Person {name: 'Ada'})")
            .unwrap();
    }
    let manifest = dir.path().join("manifest.json");
    let published = published_image(&manifest);
    {
        // Leak a second commit over the published snapshot, then let a
        // clean session replay it: that session writes nothing itself,
        // so only the pending tail can make its exit checkpoint run.
        let db = Database::open(path).unwrap();
        db.connect()
            .unwrap()
            .query("CREATE (n:Person {name: 'Grace'})")
            .unwrap();
        db.leak_without_checkpoint();
    }
    {
        let db = Database::open(path).unwrap();
        let r = db
            .connect()
            .unwrap()
            .query("MATCH (n:Person) RETURN count(n) AS n")
            .unwrap();
        assert_eq!(r.rows[0][0], Value::Int(2));
    }
    assert_ne!(
        published_image(&manifest),
        published,
        "the exit checkpoint after a replayed tail must publish"
    );
}

/// The skip's boundary: an explicit `checkpoint()` still writes over
/// the same unchanged store — the user's force-dump keeps its
/// always-writes contract (and the codec validations that run only
/// there), so the sidecar is rewritten even though the close shape
/// would have skipped.
#[test]
fn explicit_checkpoint_always_writes_over_the_published_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vault");
    let path_str = path.to_str().unwrap();
    {
        let db = Database::open_with(path_str, md_opts()).unwrap();
        db.connect()
            .unwrap()
            .query("CREATE (n:Person {name: 'Ada'})")
            .unwrap();
    }
    let sidecar = path.join(".relate").join("manifest.json");
    let published = published_image(&sidecar);
    let db = Database::open_with(path_str, md_opts()).unwrap();
    db.checkpoint().unwrap();
    let rewritten = published_image(&sidecar);
    assert_ne!(
        rewritten.1, published.1,
        "an explicit checkpoint re-publishes — a fresh mtime — over the unchanged store"
    );
    // The close shape over the same state writes nothing.
    let before_close = published_image(&sidecar);
    db.checkpoint_if_pending().unwrap();
    assert_eq!(published_image(&sidecar), before_close);
}

/// A bootstrap that never checkpoints keeps its first dump: the exit
/// of a zero-commit session over an empty store still pins the codec
/// by writing the first manifest, because no published image exists
/// to keep.
#[test]
fn bootstrap_exit_pins_the_codec_with_zero_commits() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_str().unwrap();
    {
        let db = Database::open_with(
            path,
            crate::OpenOptions {
                snapshot_codec: Some(SnapshotCodecKind::Json),
                ..Default::default()
            },
        )
        .unwrap();
        db.connect().unwrap().query("RETURN 1").unwrap();
    }
    assert!(
        dir.path().join("manifest.json").exists(),
        "the bootstrap exit checkpoint pins the codec"
    );
}
