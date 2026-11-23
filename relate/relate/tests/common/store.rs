//! WAL segment readers, the WAL frame encoder, and the crash-seeded store
//! fixtures, plus the deterministic test-store open policy.

use super::run;

/// `(segment name, magic)` of every complete WAL segment under `dir`,
/// sorted by name — the last entry is the active segment (a rotate in
/// flight's just-created, still-empty segment is skipped). The per-codec WAL
/// directory comes from [`relate_storage::SnapshotCodecKind::wal_dir`]
/// (ADR-0406 §3 for the markdown store).
pub fn segment_magics(
    dir: &std::path::Path,
    codec: relate_storage::SnapshotCodecKind,
) -> Vec<(String, [u8; 8])> {
    let mut segments: Vec<(String, [u8; 8])> = std::fs::read_dir(dir.join(codec.wal_dir()))
        .unwrap()
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("log") {
                return None;
            }
            // Skip only a rotate in flight's just-created, still-empty segment
            // (or one recycled away), like the read-only WAL open. Any other
            // unreadable or truncated segment is a real failure, not a skip.
            let bytes = std::fs::read(&path)
                .unwrap_or_else(|error| panic!("read WAL segment {}: {error}", path.display()));
            if bytes.is_empty() {
                return None;
            }
            let magic: [u8; 8] = bytes
                .get(..8)
                .expect("a non-empty WAL segment carries its 8-byte magic")
                .try_into()
                .unwrap();
            Some((
                path.file_name().unwrap().to_string_lossy().into_owned(),
                magic,
            ))
        })
        .collect();
    segments.sort();
    segments
}

/// Just the magics of [`segment_magics`], in segment-name order.
pub fn magics(dir: &std::path::Path, codec: relate_storage::SnapshotCodecKind) -> Vec<[u8; 8]> {
    segment_magics(dir, codec)
        .into_iter()
        .map(|(_, magic)| magic)
        .collect()
}

/// Open a store's VFS by the storage crate's own path rule: the bundle VFS
/// for a `.rl` file, the Native VFS for a directory. A `BundleVfs` holds
/// the host file's exclusive flock, so the handle must be dropped before
/// `Database::open` on the same store.
pub fn store_vfs(root: &std::path::Path) -> Box<dyn relate_storage::Vfs> {
    if relate_storage::is_bundle_path(root) {
        Box::new(relate_storage::BundleVfs::open_or_create(root).unwrap())
    } else {
        Box::new(relate_storage::NativeVfs::new(root))
    }
}

/// The store-relative name of the active (highest-numbered) WAL segment,
/// e.g. `wal/00000001.log`.
pub fn active_segment(vfs: &dyn relate_storage::Vfs) -> String {
    let mut segments: Vec<String> = vfs
        .list_dir("wal")
        .unwrap()
        .into_iter()
        .filter(|name| name.ends_with(".log"))
        .collect();
    segments.sort();
    format!("wal/{}", segments.last().expect("a WAL segment must exist"))
}

/// The deterministic-test-store open policy: zero checkpoint interval
/// (every durability boundary is an explicit `db.checkpoint()`, the timer
/// never fires on its own) plus the chosen snapshot codec — the shared
/// fixture the integration tests open stores with, so no test re-spells
/// the `OpenOptions` record.
pub fn opts(codec: relate_storage::SnapshotCodecKind) -> relate::OpenOptions {
    relate::OpenOptions {
        checkpoint_interval: Some(std::time::Duration::ZERO),
        snapshot_codec: Some(codec),
        ..relate::OpenOptions::default()
    }
}

/// Create a store through the public `Database` API under `codec`, commit
/// once, checkpoint, commit again, then close with
/// `leak_without_checkpoint` (a crash after the WAL fsync) so the last
/// commit lives only in the active WAL segment.
pub fn seed_store(root: &std::path::Path, codec: relate_storage::SnapshotCodecKind) {
    seed_store_with(root, codec, &[]);
}

/// [`seed_store`] plus `pre_checkpoint` statements applied before the
/// checkpoint — the parametrized fixture the constraint-replay seeding
/// shares, where the DDL must live in the checkpoint and the appended WAL
/// tail carries only the duplicate writes.
pub fn seed_store_with(
    root: &std::path::Path,
    codec: relate_storage::SnapshotCodecKind,
    pre_checkpoint: &[&str],
) {
    let db = super::env_seam::open(relate_test_support::uri(root), opts(codec));
    let mut conn = db.connect().unwrap();
    conn.query("CREATE (:T {name: 'checkpointed'})").unwrap();
    for statement in pre_checkpoint {
        run(&mut conn, statement);
    }
    db.checkpoint().unwrap();
    conn.query("CREATE (:T {name: 'tailed'})").unwrap();
    drop(conn);
    db.leak_without_checkpoint();
}

/// The one WAL frame encoder the crash-planting fixtures share: CRC-32 of
/// the payload, then the payload length as a little-endian `u32`, then the
/// payload verbatim (ADR-0120 §3). Each fixture builds its own payload.
pub fn wal_frame(payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::new();
    frame.extend_from_slice(&crc32fast::hash(payload).to_le_bytes());
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(payload);
    frame
}

/// A complete, CRC-valid WAL frame carrying one `OntologySet` op with `spec`
/// verbatim (ADR-0120 §3/§4) — the raw path a pre-bump binary's store carries
/// a document the public `set` would now refuse. `spec` is JSON-escaped by
/// the serializer, so any document syntax can be planted. The receiving
/// segment must carry the `MDBWAL02` ontology magic (ADR-0120 §4): the
/// scanner rejects an ontology op in an `MDBWAL01` segment.
pub fn ontology_set_frame(lsn: u64, commit_ts: u64, spec: &str) -> Vec<u8> {
    let payload = serde_json::json!({
        "lsn": lsn,
        "commit_ts": commit_ts,
        "ops": [{"op": "OntologySet", "database": "neo4j", "spec": spec}],
    })
    .to_string();
    wal_frame(payload.as_bytes())
}
