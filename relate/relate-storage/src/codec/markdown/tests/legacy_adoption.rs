//! The ADR-0623 legacy-engine-directory adoption suite: the writable
//! adoption moves the whole tree and the sidecar manifest names the
//! codec; a read-only open never renames; a store carrying both
//! spellings refuses closed; a live writer refuses the open untouched;
//! the reset guard refuses an un-adopted legacy vault; and a
//! caller-supplied VFS adopts at its construction — the writable pass —
//! while its read-only construction never reaches the adoption and its
//! flat backends fail closed through the rename. Split from `tests.rs`
//! under that file's own organizing rule.

use crate::codec::markdown::layout::{LEGACY_MANIFEST, MANIFEST};
use crate::codec::markdown::{ENGINE_DIR, LEGACY_ENGINE_DIR};
use crate::codec::SnapshotCodecKind;

/// The legacy sidecar as a real store leaves it: the manifest the
/// sidecar probe classifies from (a WAL the lease would have marked
/// rides along inside the renamed tree — the marker files kept their
/// pre-rename names, so their role bytes survive the move).
fn legacy_sidecar(root: &std::path::Path) {
    std::fs::create_dir_all(root.join(LEGACY_ENGINE_DIR)).unwrap();
    std::fs::write(root.join(LEGACY_MANIFEST), r#"{"codec":"markdown"}"#).unwrap();
}

#[test]
fn a_legacy_engine_directory_is_adopted_at_a_writable_open() {
    let dir = tempfile::tempdir().unwrap();
    legacy_sidecar(dir.path());
    let target = crate::parse_uri_opening(dir.path().to_str().unwrap(), None, None, false)
        .expect("the adopted sidecar's manifest names the markdown codec");
    assert_eq!(target.codec, SnapshotCodecKind::Markdown);
    drop(target);
    assert!(
        dir.path().join(MANIFEST).exists(),
        "the whole tree moved to the .relate/ spelling"
    );
    assert!(
        !dir.path().join(LEGACY_ENGINE_DIR).exists(),
        "the legacy spelling is gone after the adoption"
    );
}

#[test]
fn a_read_only_open_leaves_a_legacy_engine_directory_alone() {
    let dir = tempfile::tempdir().unwrap();
    legacy_sidecar(dir.path());
    // A read-only open cannot adopt, and a legacy markdown sidecar
    // must not resolve as a fresh classic store (its WAL would be
    // probed at the wrong spelling): the typed refusal names the
    // one writable open that adopts it.
    let err = crate::parse_uri_opening(dir.path().to_str().unwrap(), None, None, true)
        .expect_err("a read-only open of an un-adopted legacy markdown store refuses closed");
    assert!(
        err.to_string().contains(&format!(
            "open it writable once to adopt the `{ENGINE_DIR}` spelling"
        )),
        "{err}"
    );
    assert!(
        dir.path().join(LEGACY_MANIFEST).exists(),
        "a read-only open never adopts"
    );
    assert!(!dir.path().join(ENGINE_DIR).exists());
}

/// The legacy sidecar planted through a caller-supplied VFS — the
/// fixture the VFS-leg tests share: the manifest bytes a real store
/// leaves at the pre-rename spelling.
fn legacy_sidecar_vfs(vfs: &dyn crate::vfs::Vfs) {
    crate::vfs::atomic_write(vfs, LEGACY_MANIFEST, br#"{"codec":"markdown"}"#).unwrap();
}

#[test]
fn a_caller_supplied_vfs_legacy_sidecar_names_the_callers_own_repair() {
    // ADR-0623's migration is one folder rename, and a cold read never
    // renames: a legacy sidecar reached without an adoption refuses
    // closed, naming the caller's own repair (`from_vfs` cannot fail,
    // so the cold reads keep theirs — `from_vfs_with_codec` is the
    // constructor that adopts, pinned below).
    let target = crate::memory_store_with_codec(SnapshotCodecKind::Json).unwrap();
    let vfs = target.vfs();
    legacy_sidecar_vfs(&*vfs);
    let err = crate::codec::markdown::read_manifest(&target)
        .expect_err("a legacy sidecar on a caller-supplied VFS refuses the cold read");
    assert!(
        err.to_string()
            .contains(&format!("rename it to `{ENGINE_DIR}` in the VFS")),
        "{err}"
    );
    assert!(
        vfs.exists(LEGACY_MANIFEST).unwrap(),
        "the refusal must not move anything"
    );
}

#[test]
fn a_legacy_store_with_a_live_writer_refuses_the_open_untouched() {
    // The adoption runs under the root's exclusive writer lease, so
    // a live writable opener never has its engine directory moved
    // underneath it: a concurrent open refuses here, with the store
    // untouched, instead of mutating first and failing at the lease.
    let dir = tempfile::tempdir().unwrap();
    legacy_sidecar(dir.path());
    let lease = crate::native_ownership::take_writer_lease(dir.path(), dir.path()).unwrap();
    let err = crate::parse_uri_opening(dir.path().to_str().unwrap(), None, None, false)
        .expect_err("a live writer must refuse the open before the adoption");
    assert!(matches!(err, crate::Error::Locked(_)), "{err}");
    assert!(
        dir.path().join(LEGACY_MANIFEST).exists(),
        "the refused open must not adopt"
    );
    drop(lease);
}

#[test]
fn a_reset_of_an_un_adopted_legacy_vault_refuses() {
    // The reset guard's second spelling: the vault has not been
    // adopted, so the probe at the new spelling finds nothing — the
    // legacy sidecar must refuse the reset all the same, or a reset
    // would delete hand-authored vault content (ADR-0406 §6).
    let dir = tempfile::tempdir().unwrap();
    legacy_sidecar(dir.path());
    let err = crate::reset_uri(dir.path().to_str().unwrap(), None)
        .expect_err("an un-adopted legacy vault must refuse the reset");
    assert!(
        err.to_string()
            .contains("refusing to reset a markdown vault store"),
        "{err}"
    );
    assert!(
        dir.path().join(LEGACY_MANIFEST).exists(),
        "the refused reset must not touch the vault"
    );
}

#[test]
fn a_store_with_both_engine_directories_refuses_the_open() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(LEGACY_ENGINE_DIR)).unwrap();
    std::fs::create_dir_all(dir.path().join(ENGINE_DIR)).unwrap();
    let err = crate::parse_uri_opening(dir.path().to_str().unwrap(), None, None, false)
        .expect_err("both engine directories must refuse the open");
    assert!(err.to_string().contains("refusing to open"), "{err}");
    assert!(
        err.to_string()
            .contains(&format!("delete the stale `{LEGACY_ENGINE_DIR}`")),
        "{err}"
    );
}

#[test]
fn a_caller_supplied_vfs_adopts_a_legacy_engine_directory_at_construction() {
    // ADR-0623's caller-supplied-VFS leg: `from_vfs_with_codec` is the
    // writable pass, so a legacy store on such a VFS is no longer
    // permanently unopenable — the whole tree moves through one
    // `Vfs::rename` before the sidecar is first read, and the read then
    // finds it at the adopted spelling instead of refusing.
    let dir = tempfile::tempdir().unwrap();
    let vfs: std::sync::Arc<dyn crate::vfs::Vfs> =
        std::sync::Arc::new(crate::vfs::NativeVfs::new(dir.path()));
    legacy_sidecar_vfs(&*vfs);
    // A second engine object under the legacy spelling, so the assert
    // proves the whole tree moved — not one renamed key.
    let legacy_wal = format!("{LEGACY_ENGINE_DIR}/wal/00000001.log");
    crate::vfs::atomic_write(&*vfs, &legacy_wal, b"segment").unwrap();
    let target = crate::StoreTarget::from_vfs_with_codec(vfs, SnapshotCodecKind::Markdown).unwrap();
    assert!(
        crate::codec::markdown::read_manifest(&target)
            .expect("the adopted sidecar reads at the new spelling")
            .is_some(),
        "the sidecar manifest survived the move"
    );
    assert!(
        dir.path().join(MANIFEST).exists(),
        "the manifest moved to the .relate/ spelling"
    );
    assert!(
        dir.path()
            .join(ENGINE_DIR)
            .join("wal/00000001.log")
            .exists(),
        "the whole tree moved with it"
    );
    assert!(
        !dir.path().join(LEGACY_ENGINE_DIR).exists(),
        "the legacy spelling is gone after the adoption"
    );
}

#[test]
fn a_caller_supplied_vfs_with_both_engine_directories_refuses_the_adoption() {
    // The same shared refusal wording the native open speaks: the
    // classification is the same, only the substrate differs.
    let dir = tempfile::tempdir().unwrap();
    let vfs: std::sync::Arc<dyn crate::vfs::Vfs> =
        std::sync::Arc::new(crate::vfs::NativeVfs::new(dir.path()));
    legacy_sidecar_vfs(&*vfs);
    vfs.create_dir_all(ENGINE_DIR).unwrap();
    let err = crate::StoreTarget::from_vfs_with_codec(vfs, SnapshotCodecKind::Markdown)
        .expect_err("both spellings must refuse the VFS adoption");
    assert!(
        err.to_string()
            .contains(&format!("delete the stale `{LEGACY_ENGINE_DIR}`")),
        "{err}"
    );
    assert!(
        dir.path().join(LEGACY_MANIFEST).exists(),
        "the refused adoption moved nothing"
    );
}

#[test]
fn a_flat_vfs_backend_refuses_the_adoption_through_the_rename() {
    // A caller-supplied VFS whose `rename` moves single entries (the
    // flat in-process backend) cannot move the tree: the adoption
    // fails closed through the rename's own error — the store keeps its
    // legacy spelling, never a silent half-move — and the cold read's
    // refusal still names the caller's repair.
    let store = crate::memory_store_with_codec(SnapshotCodecKind::Json).unwrap();
    let vfs = store.vfs();
    legacy_sidecar_vfs(&*vfs);
    let err = crate::StoreTarget::from_vfs_with_codec(vfs, SnapshotCodecKind::Markdown)
        .expect_err("a flat backend cannot move the legacy tree");
    assert!(
        err.to_string()
            .contains("adopting the legacy engine directory"),
        "{err}"
    );
    assert!(
        store.vfs().exists(LEGACY_MANIFEST).unwrap(),
        "the failed adoption moved nothing"
    );
    assert!(
        !store.vfs().exists(MANIFEST).unwrap(),
        "nothing was half-moved to the new spelling"
    );
}

#[test]
fn a_read_only_vfs_construction_never_reaches_the_adoption() {
    // The open's writability threads past the refusal into the
    // construction: a read-only build never runs the adoption, so the
    // mirror-only legacy shape (a classic store's schema mirror, no
    // markdown manifest — the inert leg) constructs untouched — on a
    // flat backend too, whose single-entry `rename` the writable pass
    // would fail through.
    let store = crate::memory_store_with_codec(SnapshotCodecKind::Json).unwrap();
    let vfs = store.vfs();
    let mirror = format!("{LEGACY_ENGINE_DIR}/schema/schema.cypher");
    crate::vfs::atomic_write(&*vfs, &mirror, b"").unwrap();
    let target = crate::StoreTarget::from_vfs_with_codec_opening(
        vfs,
        SnapshotCodecKind::Json,
        crate::OpenMode::ReadOnly,
        None,
    )
    .unwrap();
    assert_eq!(target.snapshot_codec(), SnapshotCodecKind::Json);
    assert!(
        store
            .vfs()
            .exists(&format!("{LEGACY_ENGINE_DIR}/schema/schema.cypher"))
            .unwrap(),
        "the read-only construction renamed nothing"
    );
    assert!(
        !store.vfs().exists(ENGINE_DIR).unwrap(),
        "the read-only construction must not adopt the inert legacy shape"
    );
}

#[test]
fn a_config_refused_writable_open_leaves_the_legacy_tree_untouched() {
    // The native leg's fail-before-mutation order (round 3's reorder),
    // pinned: the codec-configuration gate refuses BEFORE the adoption
    // rename, so a config-refused open over a legacy-spelling store
    // leaves the caller's tree exactly as it was — the round-3 review
    // note this closes. (A store whose sidecar already proves markdown
    // matches the bodies configuration at the gate and is refused
    // later, by `resolve_codec`'s requested-vs-evidence coherence —
    // ADR-0623 explicitly leaves that store adopted, so the pin's
    // fixture is the legacy classic store the gate itself refuses.)
    let dir = tempfile::tempdir().unwrap();
    let mirror = format!("{LEGACY_ENGINE_DIR}/schema/schema.cypher");
    std::fs::create_dir_all(dir.path().join(&mirror).parent().unwrap()).unwrap();
    std::fs::write(dir.path().join(&mirror), b"").unwrap();
    let config = crate::CodecOpenConfig::MarkdownBodies(
        crate::MarkdownBodyProperties::parse("summary").unwrap(),
    );
    let err = crate::parse_uri_opening_with_config(
        dir.path().to_str().unwrap(),
        None,
        Some(SnapshotCodecKind::Json),
        false,
        Some(config),
    )
    .expect_err("the bodies configuration must refuse a json open");
    assert!(
        err.to_string().contains("markdown body properties"),
        "{err}"
    );
    assert!(
        dir.path()
            .join(format!("{LEGACY_ENGINE_DIR}/schema/schema.cypher"))
            .exists(),
        "the refused open renamed nothing"
    );
    assert!(!dir.path().join(ENGINE_DIR).exists());
}
/// A real store's engine tree at the legacy spelling — the honest
/// legacy fixture: open a markdown store, checkpoint it, close it, and
/// rename the whole `.relate/` tree back to `.cypherlite/`, exactly the
/// bytes a pre-rename store carries (sidecar, WAL with its typed role
/// marker, schema mirror).
fn legacy_store_at(dir: &std::path::Path) {
    let target = crate::parse_uri_with_options(
        dir.to_str().unwrap(),
        None,
        Some(SnapshotCodecKind::Markdown),
    )
    .unwrap();
    let (graph, wal) = crate::open_store::<relate_executor::Graph>(&target).unwrap();
    crate::checkpoint_with_lsn(&target, &graph, wal.lsn, None).unwrap();
    drop((graph, wal));
    drop(target);
    std::fs::rename(dir.join(ENGINE_DIR), dir.join(LEGACY_ENGINE_DIR)).unwrap();
}

#[test]
fn a_legacy_spelled_wal_dir_on_a_fresh_vault_lands_under_the_adopted_spelling() {
    // The caller's pre-rename WAL spelling meant "the engine's WAL
    // directory": on a fresh vault it follows the adopted spelling, so
    // the store is born carrying one spelling — taken verbatim instead,
    // the lease would create `.cypherlite/wal` beside the bootstrap's
    // `.relate/`, and the next open would refuse the both-spellings
    // guard.
    let dir = tempfile::tempdir().unwrap();
    let legacy_wal = dir.path().join(LEGACY_ENGINE_DIR).join("wal");
    let target = crate::parse_uri_with_options(
        dir.path().to_str().unwrap(),
        Some(legacy_wal),
        Some(SnapshotCodecKind::Markdown),
    )
    .unwrap();
    // Complete the open: the bootstrap publishes the sidecar, so the
    // next open resolves the store as markdown.
    let (graph, wal) = crate::open_store::<relate_executor::Graph>(&target).unwrap();
    drop((graph, wal));
    drop(target);
    assert!(
        dir.path().join(ENGINE_DIR).join("wal").exists(),
        "the WAL was leased at the adopted spelling"
    );
    assert!(
        !dir.path().join(LEGACY_ENGINE_DIR).exists(),
        "the fresh vault carries one spelling"
    );
    crate::parse_uri(dir.path().to_str().unwrap())
        .expect("the next open is clean — no both-spellings state");
}

#[test]
fn a_legacy_spelled_wal_dir_follows_the_adoption_and_leases_at_the_adopted_path() {
    // The legacy-vault ending: the caller's WAL sits inside the tree
    // the adoption renames, so the path must follow it — taken
    // verbatim instead, the rename would strand the WAL away from the
    // lease pointing at it, and a fresh empty `.cypherlite/wal` beside
    // the renamed tree would leave the store refused by the
    // both-spellings guard and needing a hand delete.
    let dir = tempfile::tempdir().unwrap();
    legacy_store_at(dir.path());
    let legacy_wal = dir.path().join(LEGACY_ENGINE_DIR).join("wal");
    let target = crate::parse_uri_with_options(
        dir.path().to_str().unwrap(),
        Some(legacy_wal),
        Some(SnapshotCodecKind::Markdown),
    )
    .unwrap();
    assert!(
        dir.path().join(ENGINE_DIR).join("wal").exists(),
        "the caller's WAL moved with the adoption and was leased where it now lives"
    );
    assert!(
        !dir.path().join(LEGACY_ENGINE_DIR).exists(),
        "no both-spellings state, nothing to hand-delete"
    );
    drop(target);
    crate::parse_uri(dir.path().to_str().unwrap()).expect("the next open is clean");
}

#[test]
fn an_external_wal_dir_is_untouched_by_the_adoption_rebase() {
    // A WAL path outside the engine directory is a genuinely external
    // location: the rebase never redirects it, and the adoption still
    // runs — the store adopts around it. The fixture is the legacy
    // sidecar without an internal WAL: an external `wal_dir` is a
    // from-birth arrangement, and a store whose engine tree already
    // carries its own typed WAL is refused by the nested-identity guard
    // with the WAL inside or outside — a pre-existing policy nothing
    // here changes.
    let dir = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    legacy_sidecar(dir.path());
    let target = crate::parse_uri_with_options(
        dir.path().to_str().unwrap(),
        Some(external.path().join("wal")),
        Some(SnapshotCodecKind::Markdown),
    )
    .unwrap();
    assert!(
        external.path().join("wal").exists(),
        "the external WAL location was leased verbatim"
    );
    assert!(
        dir.path().join(ENGINE_DIR).exists(),
        "the adoption ran around the external WAL"
    );
    assert!(
        !dir.path().join(LEGACY_ENGINE_DIR).exists(),
        "the legacy spelling is gone"
    );
    assert!(
        !dir.path().join(ENGINE_DIR).join("wal").exists(),
        "the WAL was not silently moved back under the engine directory"
    );
    drop(target);
}
