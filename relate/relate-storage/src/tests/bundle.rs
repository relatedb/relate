//! Single-file bundles: a `.rl` path (any case, also under `file://`) opens
//! a bundle that holds the snapshot and the WAL in one file, never a
//! directory. The legacy v0.16 `.cl` spelling selects the same bundle VFS
//! (ADR-0517 §5). A bundle-shaped directory, a `wal_dir` and a file that is
//! not a bundle are refused; any other path stays a native directory.

use super::*;

#[test]
fn rl_suffix_opens_bundle_not_directory() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.rl");
    let uri = path.to_str().unwrap();
    let (g, mut wal) = person_ada();
    let target = parse_uri(uri).unwrap();
    assert!(target.filesystem_dir().is_none());
    checkpoint_with_lsn(&target, &g, wal.lsn, None).unwrap();
    let cut = wal.rotate().unwrap();
    wal.recycle(cut).unwrap();
    drop(wal);
    drop(target);
    assert!(path.is_file());
    assert!(!dir.path().join("wal").exists());
    let target = parse_uri(uri).unwrap();
    let loaded = load::<relate_executor::Graph>(&target).unwrap();
    assert_eq!(loaded.live_node_count(), 1);
    drop(target);
    reset_uri(uri, None).unwrap();
    assert!(!path.exists());
}

/// A v0.16-and-earlier bundle kept its `.cl` name (ADR-0517 §5): `.rl` is
/// the extension this engine writes, and the legacy suffix still selects the
/// bundle VFS, so every existing single-file store opens unchanged.
#[test]
fn legacy_cl_suffix_opens_bundle_not_directory() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.cl");
    let uri = path.to_str().unwrap();
    let (g, mut wal) = person_ada();
    let target = parse_uri(uri).unwrap();
    assert!(target.filesystem_dir().is_none());
    checkpoint_with_lsn(&target, &g, wal.lsn, None).unwrap();
    let cut = wal.rotate().unwrap();
    wal.recycle(cut).unwrap();
    drop(wal);
    drop(target);
    assert!(path.is_file());
    assert!(!dir.path().join("wal").exists());
    let target = parse_uri(uri).unwrap();
    let loaded = load::<relate_executor::Graph>(&target).unwrap();
    assert_eq!(loaded.live_node_count(), 1);
    drop(target);
    reset_uri(uri, None).unwrap();
    assert!(!path.exists());
}

#[test]
fn bundle_checkpoint_with_wal_on_same_vfs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.rl");
    let uri = path.to_str().unwrap();
    let (g, mem_wal) = person_ada();
    let target = parse_uri(uri).unwrap();
    let (wv, prefix) = target.wal_vfs();
    let mut wal = Wal::open(wv, prefix, 0).unwrap();
    for rec in mem_wal.replay(0).unwrap() {
        wal.append_commit(rec.commit_ts, &rec.ops).unwrap();
    }
    let cut = wal.rotate().unwrap();
    checkpoint_with_lsn(&target, &g, wal.lsn, None).unwrap();
    wal.recycle(cut).unwrap();
    wal.append_commit(
        2,
        &[
            relate_executor::RedoOp::CreateNode {
                id: 1,
                labels: vec!["Person".into()].into(),
                properties: Default::default(),
            },
            relate_executor::RedoOp::PullIdentityPut {
                source: "upstream".into(),
                kind: relate_executor::PullKind::Node,
                remote_id: "bob".into(),
                local_id: 1,
            },
        ],
    )
    .unwrap();
    drop(wal);
    drop(target);
    let target = parse_uri(uri).unwrap();
    let (loaded, reopened) = open_store::<relate_executor::Graph>(&target).unwrap();
    assert_eq!(reopened.lsn, 2);
    assert_eq!(loaded.live_node_count(), 2);
    assert_eq!(
        loaded.pull_identity_get("upstream", relate_executor::PullKind::Node, "bob"),
        Some(1)
    );
}

#[test]
fn file_scheme_cl_is_bundle() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("g.CL");
    let uri = format!("file://{}", path.display());
    let target = parse_uri(&uri).unwrap();
    assert!(target.filesystem_dir().is_none());
    assert!(target.vfs().durable());
}

#[test]
fn rl_directory_is_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.rl");
    std::fs::create_dir(&path).unwrap();
    let err = parse_uri(path.to_str().unwrap())
        .err()
        .expect("directory named .rl");
    assert!(err.to_string().contains("directory"), "{err}");
}

#[test]
fn rl_rejects_wal_dir() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.rl");
    let err = parse_uri_with_wal(path.to_str().unwrap(), Some(dir.path().join("wal")))
        .err()
        .expect("wal_dir rejected");
    assert!(err.to_string().contains("wal_dir"), "{err}");
}

#[test]
fn non_rl_path_stays_native_directory() {
    let dir = tempfile::tempdir().unwrap();
    let uri = dir.path().to_str().unwrap();
    let target = parse_uri(uri).unwrap();
    assert_eq!(
        target.filesystem_dir().unwrap(),
        std::fs::canonicalize(dir.path()).unwrap()
    );
}

#[test]
fn garbage_rl_file_is_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.rl");
    std::fs::write(&path, b"not a bundle").unwrap();
    let err = parse_uri(path.to_str().unwrap())
        .err()
        .expect("garbage .rl");
    assert!(err.to_string().contains("MDBCLB01"), "{err}");
}
