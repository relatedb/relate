//! ADR-0623's adoption tests through `Database::open_vfs`: the read-only
//! refusal over an un-adopted legacy markdown sidecar, the inert
//! mirror-only legacy shape (a read-only open never renames it), and
//! the writable pass that adopts. Split from `persist.rs` under the
//! concern-module rule.

use super::persist::no_timer;
use crate::{Database, OpenMode, OpenOptions, SnapshotCodecKind, Value};

#[test]
fn a_vfs_read_only_open_refuses_a_legacy_markdown_sidecar() {
    use std::sync::Arc;

    use relate_storage::{MemoryVfs, Vfs};

    // ADR-0623: a read-only open through a caller-supplied VFS never
    // adopts — it cannot write — and an un-adopted legacy markdown
    // sidecar must not resolve as a fresh classic store (its WAL would
    // be probed at the wrong spelling): the refusal names the one
    // writable pass that adopts it.
    let vfs: Arc<dyn Vfs> = Arc::new(MemoryVfs::new());
    relate_storage::atomic_write(
        &*vfs,
        ".cypherlite/manifest.json",
        br#"{"codec":"markdown"}"#,
    )
    .unwrap();
    let err = Database::open_vfs(Arc::clone(&vfs), OpenOptions::with_mode(OpenMode::ReadOnly))
        .err()
        .expect("a read-only VFS open of an un-adopted legacy markdown store refuses closed");
    assert!(
        err.to_string()
            .contains("open it writable once to adopt the `.relate` spelling"),
        "{err}"
    );
    // The refused open moved nothing.
    assert!(vfs.exists(".cypherlite/manifest.json").unwrap());
    assert!(!vfs.exists(".relate/manifest.json").unwrap());
}

#[test]
fn a_vfs_read_only_open_leaves_an_inert_legacy_mirror_store_untouched() {
    use std::sync::Arc;

    use relate_storage::{NativeVfs, Vfs};

    // ADR-0623's inert shape: a legacy `.cypherlite/` carrying a
    // classic store's schema mirror and no markdown manifest. The
    // read-only leg refuses nothing here — and the open's mode threads
    // past the refusal into the construction, so the adoption never
    // runs: the read-only open proceeds with the legacy spelling
    // untouched, exactly as the native read-only open behaves. The
    // writable leg is unchanged: the same shape adopts.
    let dir = tempfile::tempdir().unwrap();
    let vfs: Arc<dyn Vfs> = Arc::new(NativeVfs::new(dir.path()));
    let json = OpenOptions {
        snapshot_codec: Some(SnapshotCodecKind::Json),
        ..no_timer()
    };
    // A real classic store — root envelope, graph, WAL — whose engine
    // sidecar (the schema mirror) is planted under the legacy name.
    {
        let db = Database::open_vfs(Arc::clone(&vfs), json.clone()).unwrap();
        let mut conn = db.connect().unwrap();
        conn.execute("CREATE (:N)").unwrap();
        db.checkpoint().unwrap();
    }
    std::fs::rename(dir.path().join(".relate"), dir.path().join(".cypherlite")).unwrap();

    // Read-only: the inert contract — the store opens, renames nothing.
    let db = Database::open_vfs(
        Arc::clone(&vfs),
        OpenOptions {
            snapshot_codec: Some(SnapshotCodecKind::Json),
            ..OpenOptions::with_mode(OpenMode::ReadOnly)
        },
    )
    .unwrap();
    let mut conn = db.connect().unwrap();
    assert_eq!(
        conn.query("MATCH (n) RETURN count(n) AS c").unwrap().rows[0][0],
        Value::Int(1)
    );
    drop(db);
    assert!(
        dir.path().join(".cypherlite/schema/schema.cypher").exists(),
        "a read-only open never renames the legacy engine directory"
    );
    assert!(
        !dir.path().join(".relate").exists(),
        "a read-only open must not adopt the inert legacy shape"
    );

    // Writable: the same shape adopts at the writable pass, unchanged.
    let db = Database::open_vfs(vfs, json.clone()).unwrap();
    drop(db);
    assert!(dir.path().join(".relate/schema/schema.cypher").exists());
    assert!(
        !dir.path().join(".cypherlite").exists(),
        "the legacy spelling is gone after the writable adoption"
    );
}

#[test]
fn a_writable_vfs_open_adopts_a_legacy_markdown_sidecar() {
    use std::sync::Arc;

    use relate_storage::{NativeVfs, Vfs};

    // The writable twin: `open_vfs` takes ADR-0623's writable pass
    // before the store is constructed, so a legacy store on a
    // caller-supplied VFS opens instead of being permanently refused.
    let dir = tempfile::tempdir().unwrap();
    let vfs: Arc<dyn Vfs> = Arc::new(NativeVfs::new(dir.path()));
    relate_storage::atomic_write(
        &*vfs,
        ".cypherlite/manifest.json",
        br#"{"codec":"markdown"}"#,
    )
    .unwrap();
    let db = Database::open_vfs(
        vfs,
        OpenOptions {
            snapshot_codec: Some(SnapshotCodecKind::Markdown),
            ..OpenOptions::with_mode(OpenMode::Writable)
        },
    )
    .unwrap();
    assert!(
        dir.path().join(".relate/manifest.json").exists(),
        "the writable VFS open adopted the sidecar"
    );
    assert!(
        !dir.path().join(".cypherlite").exists(),
        "the legacy spelling is gone after the adoption"
    );
    drop(db);
}

#[test]
fn a_config_refused_writable_vfs_open_leaves_the_legacy_tree_untouched() {
    use std::sync::Arc;

    use relate_storage::{NativeVfs, Vfs};

    // The VFS leg shares the native leg's fail-before-mutation order:
    // the codec-configuration gate runs inside the construction, before
    // the pre-read step's adoption rename, so a config-refused open
    // over a legacy-spelling store never renames the caller's tree.
    let dir = tempfile::tempdir().unwrap();
    let vfs: Arc<dyn Vfs> = Arc::new(NativeVfs::new(dir.path()));
    std::fs::create_dir_all(dir.path().join(".cypherlite")).unwrap();
    std::fs::write(
        dir.path().join(".cypherlite/manifest.json"),
        br#"{"codec":"markdown"}"#,
    )
    .unwrap();
    // `.err().expect` — not `expect_err` — because Database does not
    // implement Debug.
    let err = Database::open_vfs(
        Arc::clone(&vfs),
        OpenOptions {
            snapshot_codec: Some(SnapshotCodecKind::Json),
            markdown_body_properties: Some(
                relate_storage::MarkdownBodyProperties::parse("summary").unwrap(),
            ),
            ..no_timer()
        },
    )
    .err()
    .expect("the bodies configuration must refuse a json open");
    assert!(
        err.to_string().contains("markdown body properties"),
        "{err}"
    );
    assert!(
        dir.path().join(".cypherlite/manifest.json").exists(),
        "the refused open renamed nothing"
    );
    assert!(!dir.path().join(".relate").exists());
}
