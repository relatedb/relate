//! EXPERIMENTAL fjall snapshot codec — measurement only, behind the
//! `fjall-codec` feature (default off). Not a product codec until an ADR
//! promotes it; no store compatibility promise.
//!
//! The store's snapshot is a `fjall/` keyspace directory beside the
//! manifest: two LSM partitions (`nodes`, `rels`) keyed by big-endian id,
//! plus a `meta` partition for the counters, the catalog wire and the
//! ontology. Row values are the same cypher-json rows the JSON codec
//! writes (serde_json), so a measurement isolates the *container*: fjall
//! adds LSM segment/journal overhead and lz4, and offers no columnar
//! encoding.
//!
//! fjall needs a real directory, so this codec fails closed on targets
//! without one (`:memory:`, object stores) — a limitation worth recording
//! for any future adoption.

use relate_core::{Error, Node, PropertyMap, Relationship, Result};
use std::sync::Arc;

use fjall::{Config, Keyspace, PartitionCreateOptions, PartitionHandle, PersistMode};

use super::catalog;
use super::convert::{props_from_cypher_json, props_to_cypher_json};
use super::model::{SnapshotContents, SnapshotRows};
use super::SnapshotCodecKind;
use crate::snapshot::LoadedSnapshot;
use crate::vfs::native::{native_entry_kind, NativeEntryKind};
use crate::{storage_context, StoreTarget};

/// The store-relative directory holding the keyspace.
pub const FJALL_DIR: &str = "fjall";
const NODES: &str = "nodes";
const RELS: &str = "rels";
const META: &str = "meta";
const META_KEY: &[u8] = b"snapshot";

#[derive(serde::Serialize, serde::Deserialize)]
struct MetaPayload {
    value_encoding: u64,
    next_node_id: i64,
    next_rel_id: i64,
    /// The checkpoint LSN this keyspace holds. Required: every writer sets
    /// it, and the load refuses a keyspace whose LSN disagrees with the
    /// manifest's (the two halves publish separately, so a mismatch is a
    /// torn publication). An older payload without the field fails the
    /// decode — this codec promises no store compatibility.
    checkpoint_lsn: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ontology: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct NodeRow {
    labels: Vec<String>,
    props: serde_json::Map<String, serde_json::Value>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct RelRow {
    #[serde(rename = "type")]
    rel_type: String,
    start: i64,
    end: i64,
    props: serde_json::Map<String, serde_json::Value>,
}

/// The codec's store layout, derived once: the native root the codec
/// requires, the live keyspace directory beside the manifest, and the
/// staging and backup siblings publication uses. The dump, the load and
/// the containment guard all consume this model — no call site re-proves
/// the local-filesystem precondition.
struct StorePaths {
    root: std::path::PathBuf,
    live: std::path::PathBuf,
    staging: std::path::PathBuf,
    backup: std::path::PathBuf,
}

fn store_paths(target: &StoreTarget) -> Result<StorePaths> {
    let root = target
        .native_root
        .as_ref()
        .ok_or_else(|| Error::storage("fjall snapshot codec requires a local filesystem store"))?
        .to_path_buf();
    let sibling = |suffix: &str| {
        let mut dir = root.join(FJALL_DIR);
        dir.set_file_name(format!("{FJALL_DIR}.{suffix}"));
        dir
    };
    Ok(StorePaths {
        live: root.join(FJALL_DIR),
        staging: sibling("new"),
        backup: sibling("old"),
        root,
    })
}

impl StorePaths {
    /// The layout entries the containment guard checks — the model's own
    /// enumeration, so a path added to the struct is guarded by being
    /// added here, beside the fields it belongs to.
    fn guarded(&self) -> [(&'static str, &std::path::Path); 4] {
        [
            ("store root", &self.root),
            ("keyspace", &self.live),
            ("staging keyspace", &self.staging),
            ("backup keyspace", &self.backup),
        ]
    }
}

/// Replace `dir` with `staging`, keeping `backup` until the caller says the
/// publication succeeded: a failure here puts the previous keyspace back.
fn swap_in(
    staging: &std::path::Path,
    dir: &std::path::Path,
    backup: &std::path::Path,
) -> Result<()> {
    discard_dir(backup)?;
    move_keyspace(dir, backup)?;
    match std::fs::rename(staging, dir) {
        Ok(()) => Ok(()),
        Err(err) => Err(super::publication_failure(
            Error::storage(format!("fjall keyspace publication failed: {err}")),
            restore_backup(dir, backup),
        )),
    }
}

/// Undo a failed publication: discard whatever landed at `dir` and move
/// the backup back. Its own failure is folded into the caller's error —
/// the load refuses a damaged store rather than serving one.
fn restore_backup(dir: &std::path::Path, backup: &std::path::Path) -> Result<()> {
    discard_dir(dir)?;
    move_keyspace(backup, dir)
}

/// Write every row of `contents` into the partitions — the dump's encode
/// phase, one pass per row type.
fn write_rows(
    contents: &SnapshotContents,
    nodes: &PartitionHandle,
    rels: &PartitionHandle,
) -> Result<()> {
    for n in &contents.nodes {
        let row = NodeRow {
            labels: n.labels.as_ref().clone(),
            props: props_to_json(&n.properties, n.id)?,
        };
        let bytes = serde_json::to_vec(&row).map_err(|e| Error::storage(e.to_string()))?;
        nodes
            .insert(id_key(n.id), bytes)
            .map_err(|e| Error::storage(format!("fjall node insert failed: {e}")))?;
    }
    for r in &contents.relationships {
        let row = RelRow {
            rel_type: r.rel_type.clone(),
            start: r.start,
            end: r.end,
            props: props_to_json(&r.properties, r.id)?,
        };
        let bytes = serde_json::to_vec(&row).map_err(|e| Error::storage(e.to_string()))?;
        rels.insert(id_key(r.id), bytes)
            .map_err(|e| Error::storage(format!("fjall relationship insert failed: {e}")))?;
    }
    Ok(())
}

/// Whether the keyspace directory is present on disk — the one probe both
/// the evidence count and the load's empty-store check share. Fail-closed
/// like every sibling probe: a benign absence is a definite "no", any
/// other I/O error is an unreadable-but-present store and propagates
/// (a bare `Path::exists` would fold it into "absent").
fn keyspace_present(dir: &std::path::Path) -> Result<bool> {
    match std::fs::metadata(dir) {
        Ok(_) => Ok(true),
        Err(err) if super::benign_absence(&err) => Ok(false),
        Err(err) => Err(Error::storage(format!(
            "fjall keyspace probe failed: {err}"
        ))),
    }
}

/// The keyspace must be a real directory inside the store root: fjall
/// writes through `std::fs` (it owns its own file layout), so the VFS's
/// below-root symlink refusal does not cover it, and a crafted store
/// directory could otherwise redirect keyspace creation outside the store.
/// The store root, the keyspace and the two publication siblings are all
/// checked — the root itself is never followed, matching the VFS's
/// policy, and the entry shapes are classified through the crate's one
/// classifier ([`crate::vfs::native::native_entry_kind`]).
fn ensure_keyspace_is_inside_the_store(paths: &StorePaths) -> Result<()> {
    for (what, path) in paths.guarded() {
        match std::fs::symlink_metadata(path) {
            Ok(meta) => {
                if native_entry_kind(&meta) == NativeEntryKind::Alias {
                    return Err(Error::storage(format!(
                        "fjall {what} {} is a symlink; the codec refuses to follow it",
                        path.display()
                    )));
                }
            }
            // A missing path is fine: the dump creates it, and a missing
            // keyspace is the damaged-store / fresh-store case the caller
            // already handles.
            Err(err) if super::benign_absence(&err) => {}
            Err(err) => {
                return Err(Error::storage(format!("fjall {what} probe failed: {err}")));
            }
        }
    }
    Ok(())
}

/// Refuse any alias inside the keyspace: the top-level guard covers the
/// keyspace entry itself, but fjall creates and recovers partition files
/// throughout the directory it opens, so a crafted partition directory or
/// segment that is a symlink would redirect those writes outside the
/// store. One walk, classified through the same
/// [`crate::vfs::native::native_entry_kind`], covers everything the
/// keyspace contains.
fn ensure_no_aliases_inside(dir: &std::path::Path) -> Result<()> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .map_err(|e| Error::storage(format!("fjall keyspace scan failed: {e}")))?;
        for entry in entries {
            let entry =
                entry.map_err(|e| Error::storage(format!("fjall keyspace scan failed: {e}")))?;
            // `DirEntry::metadata` does not follow the entry's own symlink,
            // so the shape classified is the entry itself.
            let meta = entry
                .metadata()
                .map_err(|e| Error::storage(format!("fjall keyspace scan failed: {e}")))?;
            match native_entry_kind(&meta) {
                NativeEntryKind::Dir => stack.push(entry.path()),
                NativeEntryKind::File => {}
                _ => {
                    return Err(Error::storage(format!(
                        "fjall keyspace entry {} is not a plain file or directory; the codec refuses to follow it",
                        entry.path().display()
                    )));
                }
            }
        }
    }
    Ok(())
}

/// Discard a keyspace directory if it is present — the one
/// presence-guarded removal the staging cleanup, the swap and the rollback
/// all need.
fn discard_dir(dir: &std::path::Path) -> Result<()> {
    if keyspace_present(dir)? {
        std::fs::remove_dir_all(dir).map_err(|e| Error::storage(e.to_string()))?;
    }
    Ok(())
}

/// Move a keyspace directory into place if it is present — the one
/// presence-guarded rename the swap and the rollback share.
fn move_keyspace(from: &std::path::Path, to: &std::path::Path) -> Result<()> {
    if keyspace_present(from)? {
        std::fs::rename(from, to).map_err(|e| Error::storage(e.to_string()))?;
    }
    Ok(())
}

/// Whether the store carries this codec's keyspace directory — the
/// evidence `resolve_codec` counts. A target without a native root is no
/// evidence (the codec is never its writer).
pub(crate) fn has_store_files(target: &StoreTarget) -> Result<bool> {
    let Some(root) = target.native_root.as_ref() else {
        return Ok(false);
    };
    keyspace_present(&root.join(FJALL_DIR))
}

fn open_keyspace(dir: &std::path::Path) -> Result<Keyspace> {
    Config::new(dir)
        .open()
        .map_err(|e| Error::storage(format!("fjall keyspace open failed: {e}")))
}

fn open_partitions(
    keyspace: &Keyspace,
) -> Result<(PartitionHandle, PartitionHandle, PartitionHandle)> {
    let open = |name: &str| {
        keyspace
            .open_partition(name, PartitionCreateOptions::default())
            .map_err(|e| Error::storage(format!("fjall partition '{name}' open failed: {e}")))
    };
    Ok((open(NODES)?, open(RELS)?, open(META)?))
}

/// Big-endian keys keep iteration in id order (the graph rebuild order).
fn id_key(id: i64) -> [u8; 8] {
    id.to_be_bytes()
}

fn props_to_json(
    props: &PropertyMap,
    id: i64,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    props_to_cypher_json(props)
        .map_err(|e| storage_context(e, format_args!("node/relationship {id}")))
}

fn props_from_json(
    props: &serde_json::Map<String, serde_json::Value>,
    id: i64,
    value_encoding: u64,
) -> Result<Arc<PropertyMap>> {
    let decoded = props_from_cypher_json(props, value_encoding)
        .map_err(|e| storage_context(e, format_args!("node/relationship {id}")))?;
    Ok(PropertyMap::share(decoded))
}

pub fn dump(
    target: &StoreTarget,
    graph: &dyn relate_engine_api::GraphImport,
    checkpoint_lsn: u64,
    snapshot_ts: u64,
    ontology: Option<&str>,
) -> Result<()> {
    let contents = SnapshotContents::from_graph(graph, snapshot_ts)?;
    let theater = graph.checkpoint_theater_for_write()?;
    let wire = catalog::to_json(&contents, &theater);
    let paths = store_paths(target)?;
    ensure_keyspace_is_inside_the_store(&paths)?;
    // The dump is a replacement, not a delta: the new keyspace is built in
    // a staging directory and only then swapped in, so rows that vanished
    // since the last checkpoint cannot resurrect on the next load and a
    // failure anywhere in the write leaves the previous keyspace — and the
    // manifest that names it — untouched.
    let staging = &paths.staging;
    discard_dir(staging)?;
    std::fs::create_dir_all(staging).map_err(|e| Error::storage(e.to_string()))?;
    let keyspace = open_keyspace(staging)?;
    let (nodes, rels, meta) = open_partitions(&keyspace)?;

    write_rows(&contents, &nodes, &rels)?;
    let payload = MetaPayload {
        value_encoding: crate::version::value_encoding::version_for_write(ontology.is_some()),
        next_node_id: contents.next_node_id,
        next_rel_id: contents.next_rel_id,
        checkpoint_lsn,
        ontology: ontology.map(str::to_owned),
    };
    let meta_bytes = serde_json::to_vec(&payload).map_err(|e| Error::storage(e.to_string()))?;
    meta.insert(META_KEY, meta_bytes)
        .map_err(|e| Error::storage(format!("fjall meta insert failed: {e}")))?;
    // Everything must be durable before the envelope that names it.
    keyspace
        .persist(PersistMode::SyncAll)
        .map_err(|e| Error::storage(format!("fjall persist failed: {e}")))?;
    // The partition handles keep the keyspace alive after the `Keyspace`
    // value drops, so all four go before the swap: the staged directory
    // must be closed before it is renamed (Windows refuses to rename an
    // open directory).
    drop((nodes, rels, meta));
    drop(keyspace);

    // Publish: swap the staged keyspace in (keeping the previous one
    // aside), write the manifest that names it, then drop the backup. A
    // failure before the manifest is written restores the previous
    // keyspace, so the live checkpoint survives any failed publication;
    // a crash after the swap but before the manifest leaves a damaged
    // store, which the load refuses rather than serving empty.
    let backup = &paths.backup;
    // The manifest this store had before the swap: a failed publication
    // must put back *both* halves of the previous checkpoint, or the
    // manifest would name rows the restored keyspace does not hold (and
    // the load, having no checkpoint identity to compare, would skip
    // already-checkpointed WAL records).
    let previous_manifest =
        crate::snapshot::get_object_opt(target, crate::snapshot::MANIFEST_JSON)?;
    swap_in(staging, &paths.live, backup)?;
    let published = super::write_codec_manifest(
        target,
        SnapshotCodecKind::Fjall,
        checkpoint_lsn,
        snapshot_ts,
        Some(theater),
        wire,
        ontology,
    );
    if let Err(error) = published {
        // Both halves of the previous checkpoint are restored, and both
        // restore outcomes are reported: an incomplete rollback leaves a
        // state the load must refuse, so it is never swallowed.
        let rollback = restore_backup(&paths.live, backup).and_then(|()| {
            super::restore_object(target, crate::snapshot::MANIFEST_JSON, previous_manifest)
        });
        return Err(super::publication_failure(error, rollback));
    }
    // The schema mirror publishes after the checkpoint it reflects, and the
    // retired backup is dropped only after that: a cleanup failure must not
    // report a committed checkpoint as failed.
    crate::schema_files::finish_checkpoint(target, graph, ontology)?;
    let _ = discard_dir(backup);
    Ok(())
}

pub(crate) fn load_with_checkpoint_lsn<G: relate_engine_api::SnapshotGraph>(
    target: &StoreTarget,
) -> Result<LoadedSnapshot<G>> {
    let Some((manifest, marker)) = super::read_codec_manifest(target)? else {
        return Ok(LoadedSnapshot::empty(0));
    };
    let paths = store_paths(target)?;
    ensure_keyspace_is_inside_the_store(&paths)?;
    if !keyspace_present(&paths.live)? {
        // The manifest names a published checkpoint, so the keyspace it
        // describes must exist (the shared manifest-implies-payload
        // policy): serving an empty graph would let the next checkpoint
        // publish that emptiness over the real data.
        return Err(super::experimental::missing_payload(
            &manifest,
            "keyspace directory",
        ));
    }
    // The keyspace is opened read-write by fjall itself, so everything it
    // contains must be a plain file or directory before the open — the
    // top-level guard above covers only the store-root entries.
    ensure_no_aliases_inside(&paths.live)?;
    let keyspace = open_keyspace(&paths.live)?;
    let (nodes, rels, meta) = open_partitions(&keyspace)?;
    let meta_bytes = meta
        .get(META_KEY)
        .map_err(|e| Error::storage(format!("fjall meta read failed: {e}")))?
        .ok_or_else(|| Error::storage("fjall store has no meta row"))?;
    let payload: MetaPayload = serde_json::from_slice(&meta_bytes)
        .map_err(|e| Error::storage(format!("fjall meta decode failed: {e}")))?;
    // The payload's own checkpoint identity: a keyspace left beside a
    // manifest that names a different checkpoint is a torn publication.
    if payload.checkpoint_lsn != manifest.envelope.checkpoint_lsn {
        return Err(super::checkpoint_identity_mismatch(
            &manifest,
            "keyspace",
            payload.checkpoint_lsn,
        ));
    }
    // The content gate: the payload's own value-encoding marker (the
    // shared experimental policy).
    let encoding = super::content_encoding(payload.value_encoding, payload.ontology.is_some())?;

    // One pass per row type: the decode never touches the keyspace, so
    // the models are built inside the iteration loops.
    let mut node_models = Vec::new();
    for kv in nodes.iter() {
        let (key, value) =
            kv.map_err(|e| Error::storage(format!("fjall node iteration failed: {e}")))?;
        let id = i64::from_be_bytes(
            key.as_ref()
                .try_into()
                .map_err(|_| Error::storage("fjall node key is not an 8-byte id"))?,
        );
        let row: NodeRow = serde_json::from_slice(&value)
            .map_err(|e| Error::storage(format!("fjall node decode failed: {e}")))?;
        node_models.push(Node {
            id,
            labels: Arc::new(row.labels),
            properties: props_from_json(&row.props, id, encoding)?,
        });
    }
    let mut rel_models = Vec::new();
    for kv in rels.iter() {
        let (key, value) =
            kv.map_err(|e| Error::storage(format!("fjall relationship iteration failed: {e}")))?;
        let id = i64::from_be_bytes(
            key.as_ref()
                .try_into()
                .map_err(|_| Error::storage("fjall relationship key is not an 8-byte id"))?,
        );
        let row: RelRow = serde_json::from_slice(&value)
            .map_err(|e| Error::storage(format!("fjall relationship decode failed: {e}")))?;
        rel_models.push(Relationship {
            id,
            start: row.start,
            end: row.end,
            rel_type: row.rel_type,
            properties: props_from_json(&row.props, id, encoding)?,
        });
    }
    drop((nodes, rels, meta));
    drop(keyspace);
    let rows = SnapshotRows {
        nodes: node_models,
        relationships: rel_models,
        next_node_id: payload.next_node_id,
        next_rel_id: payload.next_rel_id,
    };
    super::assemble_loaded(rows, &manifest, payload.ontology, marker)
}

pub fn snapshot_data_bytes(target: &StoreTarget) -> Result<u64> {
    // The one size-accounting policy over the manifest (see
    // `snapshot::sum_object_bytes`), plus the keyspace directory.
    let sum = crate::snapshot::sum_object_bytes(target, &[crate::snapshot::MANIFEST_JSON])?;
    let dir = store_paths(target)?.live;
    if keyspace_present(&dir)? {
        return sum
            .checked_add(crate::snapshot::dir_bytes(&dir)?)
            .ok_or_else(|| Error::storage("snapshot size overflows u64"));
    }
    Ok(sum)
}
