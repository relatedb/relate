//! Parquet shard writer/reader (ADR-0022) — the Parquet-only part of
//! [`crate::snapshot`], split out of `snapshot.rs` as a directory module. It
//! is compiled only when the Parquet codec is (`parquet` feature on a
//! non-wasm32 target, ADR-0105); the parent gates the whole module.

use super::*;

#[cfg(test)]
use std::ops::Range;

#[cfg(test)]
use relate_executor::Graph;

use crate::codec::parquet::{load_nodes, load_rels, Loader, Origin, MAX_FILE};
// Test modules reach the preflighting wrappers through `use super::*`.
#[cfg(test)]
use crate::codec::parquet::{nodes_parquet_ids, rels_parquet_ids};
use crate::vfs::native;
use crate::vfs::Vfs;
#[cfg(test)]
use crate::SnapshotCodecKind;
use crate::{storage_context, Manifest};

mod coalesce;
mod layout;
mod load;
mod ontology;
mod shard_name;
mod write;

use crate::codec::cut::{CheckpointCut, Rewrite};
#[cfg(test)]
pub use load::load;
pub(crate) use load::load_with_checkpoint_lsn;
pub(crate) use shard_name::ShardName;
pub(crate) use write::checkpoint_at;
#[cfg(test)]
pub(crate) use write::checkpoint_at_with_bytes;
#[cfg(test)]
use write::ewma_density;
use write::ShardWrite;

/// Per-kind manifest/graph pairing: the four choices `rewrite_kind` and
/// the manifest recheck must keep consistent, stated once here.
impl Kind {
    fn parts(self, m: &Manifest) -> &[ShardPart] {
        match self {
            Self::Node => &m.node_parts,
            Self::Rel => &m.rel_parts,
        }
    }

    #[cfg(test)]
    fn parts_mut(self, m: &mut Manifest) -> &mut Vec<ShardPart> {
        match self {
            Self::Node => &mut m.node_parts,
            Self::Rel => &mut m.rel_parts,
        }
    }

    fn bytes_per_row(self, m: &Manifest) -> f64 {
        match self {
            Self::Node => m.bytes_per_node,
            Self::Rel => m.bytes_per_rel,
        }
    }

    /// Ids the previous checkpoint did not cover, sorted. Takes the marks the
    /// caller already fetched — one fetch per checkpoint covers both kinds,
    /// so neither pays for a clone of the other's set.
    fn dirty_ids(self, marks: &relate_engine_api::CheckpointDirty) -> Vec<i64> {
        let mut ids: Vec<i64> = match self {
            Self::Node => marks.nodes.iter().copied().collect(),
            Self::Rel => marks.rels.iter().copied().collect(),
        };
        ids.sort_unstable();
        ids
    }

    /// The live ids at `ts`, strictly ascending: the planner, the layout and
    /// every `partition_point` over them rely on that order, which the
    /// engine's enumerators promise and this boundary states once.
    fn all_ids(self, graph: &dyn relate_engine_api::GraphExport, ts: u64) -> Vec<i64> {
        let ids = match self {
            Self::Node => graph.all_node_ids_at(ts),
            Self::Rel => graph.all_rel_ids_at(ts),
        };
        debug_assert!(ids.is_sorted_by(|a, b| a < b), "live ids ascend strictly");
        ids
    }

    fn loader(self) -> Loader {
        match self {
            Self::Node => load_nodes,
            Self::Rel => load_rels,
        }
    }
}

// The version marker lives in the store-version registry (ADR-0127);
// `FORMAT_VERSION` is re-exported for the existing checkpoint/load sites.
pub(crate) use crate::version::manifest::CURRENT_VERSION as FORMAT_VERSION;

const PARQUET_SHARD: BoundedRead = BoundedRead {
    max_len: MAX_FILE,
    too_large: |name| {
        Error::storage(format!(
            "Parquet shard {name} exceeds 1 GiB ({MAX_FILE} bytes)"
        ))
    },
    changed: |name| Error::storage(format!("Parquet shard {name} changed length during read")),
};

/// Default shard byte budget (override with `RELATE_SHARD_TARGET_BYTES`).
pub const DEFAULT_SHARD_TARGET_BYTES: u64 = 256 * 1024 * 1024;

const MIN_WINDOW: i64 = 4096;
const MAX_WINDOW: i64 = 1_000_000;
const FLOOR_BYTES_PER_ROW: f64 = 256.0;
const ASSUMED_BYTES_PER_ROW: f64 = 1024.0;

pub fn shard_target_bytes() -> Result<u64> {
    match std::env::var("RELATE_SHARD_TARGET_BYTES") {
        Err(std::env::VarError::NotPresent) => Ok(DEFAULT_SHARD_TARGET_BYTES),
        Ok(value) => parse_shard_target(&value),
        Err(_) => Err(Error::storage(format!(
            "RELATE_SHARD_TARGET_BYTES must be valid UTF-8 in 1..={MAX_FILE}"
        ))),
    }
}

fn parse_shard_target(value: &str) -> Result<u64> {
    value
        .parse::<u64>()
        .ok()
        .filter(|&n| (1..=MAX_FILE).contains(&n))
        .ok_or_else(|| {
            Error::storage(format!(
                "RELATE_SHARD_TARGET_BYTES must be an integer in 1..={MAX_FILE}"
            ))
        })
}

pub fn window_width(target_bytes: u64, bytes_per_row: f64) -> i64 {
    let bpr = if bytes_per_row.is_finite() && bytes_per_row > 0.0 {
        bytes_per_row.max(FLOOR_BYTES_PER_ROW)
    } else {
        ASSUMED_BYTES_PER_ROW
    };
    let w = (target_bytes as f64 / bpr).floor() as i64;
    w.clamp(MIN_WINDOW, MAX_WINDOW)
}

/// Sum of live snapshot objects (manifest + Parquet parts). Excludes WAL
/// and `.tmp`.
pub fn snapshot_data_bytes(target: &StoreTarget) -> Result<u64> {
    let Some((manifest, _)) = read_manifest(target)? else {
        return Ok(0);
    };
    let mut names = vec![(MANIFEST_JSON, false)];
    for kind in Kind::ALL {
        if manifest.format_version < FORMAT_VERSION && kind.parts(&manifest).is_empty() {
            names.push((kind.legacy_names()[0], false));
        }
    }
    for p in manifest.node_parts.iter().chain(manifest.rel_parts.iter()) {
        names.push((&p.path, true));
    }
    // The selected ontology object is a live snapshot object too
    // (ADR-0120 §5).
    if let Some(path) = &manifest.ontology_path {
        names.push((path.as_str(), true));
    }
    let mut sum = 0u64;
    for (name, required) in names {
        if !target.vfs.exists(name)? {
            if required {
                return Err(Error::storage(format!("missing manifest shard {name}")));
            }
            continue;
        }
        let meta = target.vfs.metadata(name)?;
        check_bounded_len(name, meta.len)?;
        sum = sum
            .checked_add(meta.len)
            .ok_or_else(|| Error::storage("snapshot byte accounting overflow"))?;
    }
    Ok(sum)
}

#[cfg(test)]
pub(crate) fn read_manifest_for_test(target: &StoreTarget) -> Result<Option<Manifest>> {
    Ok(read_manifest(target)?.map(|(m, _)| m))
}

/// How [`validate_dir`] treats an absent directory — the two admission
/// policies its callers select by name (mirroring `OpenAdmit` and
/// `HostAdmit`), so a validate/create/re-validate sandwich reads by
/// policy instead of by boolean position.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DirAdmit {
    /// The directory must already exist: absence is a refusal.
    Required,
    /// An absent entry is acceptable, for callers about to create it.
    Optional,
}

/// Refuse a `root`-relative directory that is missing, is not a directory, or
/// is reachable only through a symlink/junction/reparse point.
/// [`DirAdmit::Optional`] treats an absent entry as acceptable, for callers
/// about to create it.
fn validate_dir(root: &std::path::Path, rel: &str, what: &str, admit: DirAdmit) -> Result<()> {
    let path = root.join(rel);
    match std::fs::symlink_metadata(&path) {
        Ok(meta)
            if matches!(
                native::native_entry_kind(&meta),
                native::NativeEntryKind::Dir
            ) =>
        {
            Ok(())
        }
        Ok(_) => Err(Error::storage(format!(
            "{what} is not a regular directory (symlinks refused): {}",
            path.display()
        ))),
        // Absence is its own refusal — only `DirAdmit::Optional` promotes
        // it to Ok — and every other inspection failure propagates instead
        // of reading as a shape refusal.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => match admit {
            DirAdmit::Optional => Ok(()),
            DirAdmit::Required => Err(Error::storage(format!(
                "missing {what}: {}",
                path.display()
            ))),
        },
        Err(e) => Err(e.into()),
    }
}

fn get_parquet(target: &StoreTarget, name: &str) -> Result<Vec<u8>> {
    get_parquet_opt(target, name)?.ok_or_else(|| Error::storage(format!("missing {name}")))
}

fn get_parquet_opt(target: &StoreTarget, name: &str) -> Result<Option<Vec<u8>>> {
    read_bounded_object(target, name, &PARQUET_SHARD)
}
pub(crate) fn read_manifest(
    target: &StoreTarget,
) -> Result<Option<(Manifest, crate::version::SnapshotMarker)>> {
    let Some(bytes) = get_object_opt(target, MANIFEST_JSON)? else {
        return Ok(None);
    };
    let m: Manifest = serde_json::from_slice(&bytes).map_err(|e| Error::storage(e.to_string()))?;
    // The format marker gates everything else: a manifest written by a newer
    // Relate must fail before any selector, sentinel or shard check runs
    // (ADR-0114 Decision 2). The registry's per-subsystem gate is the one
    // gate (ADR-0127 Decision 2); the marker is classified here, once,
    // against the content the manifest carries (ADR-0120 §8), and handed to
    // the caller with the manifest.
    let marker = crate::version::manifest::gate(m.format_version, m.ontology_path.is_some())?;
    crate::manifest::reject_foreign_engine(&m.query_engine, &m.write_engine)?;
    if let Some(path) = &m.ontology_path {
        if !is_ontology_object_name(path) {
            return Err(Error::storage(format!(
                "invalid manifest ontology path: {path}"
            )));
        }
    }
    if m.commit_ts == u64::MAX || m.next_node_id < 0 || m.next_rel_id < 0 {
        return Err(Error::storage(
            "manifest has unsupported timestamp or next ID",
        ));
    }
    validate_manifest_parts(target, &m)?;
    Ok(Some((m, marker)))
}

/// Validate every part a manifest names — the established-part check both
/// the open's `read_manifest` and a rewrite's recheck (every proposed
/// reference after writing, not just new leaves) consult: one spelling of
/// the walk over the `Kind::ALL` vocabulary.
fn validate_manifest_parts(target: &StoreTarget, m: &Manifest) -> Result<()> {
    for kind in Kind::ALL {
        for part in kind.parts(m) {
            validate_part(target, kind, part)?;
        }
    }
    Ok(())
}

fn validate_part(target: &StoreTarget, kind: Kind, part: &ShardPart) -> Result<()> {
    let invalid = || Error::storage(format!("invalid manifest shard path: {}", part.path));
    let name = ShardName::parse(&part.path).ok_or_else(invalid)?;
    if name.kind != kind || name.lo != part.id_lo || name.hi != part.id_hi {
        return Err(invalid());
    }
    validate_shard_name(target, name).map_err(|_| invalid())?;
    if part.rows == 0
        || part.rows > MAX_WINDOW as u64
        || part.rows > name.width()
        || part.bytes == 0
        || part.bytes > MAX_FILE
    {
        return Err(Error::storage(format!(
            "invalid manifest shard metrics: {} (rows {}, bytes {})",
            part.path, part.rows, part.bytes
        )));
    }
    Ok(())
}

/// Whether the `root`-relative `rel` resolves to a real directory, never
/// through a symlink/junction/reparse point. Missing or not-a-directory is
/// `false`: the orphan sweeps skip rather than fail.
fn native_plain_dir(root: &std::path::Path, rel: &str) -> bool {
    std::fs::symlink_metadata(root.join(rel))
        .map(|m| m.file_type().is_dir())
        .unwrap_or(false)
}

/// Whether the `root`-relative `rel` resolves to a plain file, never an
/// alias: the orphan sweeps' delete condition.
fn native_plain_file(root: &std::path::Path, rel: &str) -> bool {
    std::fs::symlink_metadata(root.join(rel))
        .map(|m| m.file_type().is_file())
        .unwrap_or(false)
}

/// The native-side proof that a valid-looking object name does not alias
/// the lock or another object through a directory junction, reparse point
/// or leaf symlink: `rel` under `root` must metadata as a plain file
/// (ADR-0089, ADR-0120 §5). `what` names the object family in the errors.
fn validate_native_plain_file(root: &std::path::Path, rel: &str, what: &str) -> Result<()> {
    let file = root.join(rel);
    let meta = std::fs::symlink_metadata(&file)
        .map_err(|e| Error::storage(format!("invalid {what} {}: {e}", file.display())))?;
    if native::native_entry_kind(&meta) != native::NativeEntryKind::File {
        return Err(Error::storage(format!("invalid {what} path: {rel}")));
    }
    Ok(())
}

fn referenced(m: &Manifest, path: &str) -> bool {
    m.ontology_path.as_deref() == Some(path)
        || m.node_parts
            .iter()
            .chain(&m.rel_parts)
            .any(|p| p.path == path)
}

/// Whether this store retains superseded and orphaned shard objects instead
/// of deleting them: Windows cannot prove namespace persistence of a renamed
/// manifest after power loss, so a native directory there keeps older
/// generations for a fallback manifest. The one home of the rule — the
/// superseded-object retirement and [`sweep_orphans`] both consult it, and
/// so do the suites that assert which objects a store keeps.
fn keeps_superseded(target: &StoreTarget) -> bool {
    cfg!(windows) && target.native_root.is_some()
}

/// Run only after a complete snapshot and WAL replay have established the
/// authoritative manifest. Fixed-name legacy shards are never crash orphans.
pub(crate) fn sweep_orphans(target: &StoreTarget) -> Result<()> {
    let Some((manifest, _)) = read_manifest(target)? else {
        return Ok(());
    };
    if keeps_superseded(target) {
        return Ok(());
    }
    for dir in Kind::ALL.map(Kind::dir) {
        sweep_dir_orphans(
            target,
            dir,
            |path| matches!(object_name(path), Some(ObjectName::Shard(shard)) if shard.generation.is_some()),
            |path| referenced(&manifest, path),
        );
    }
    ontology::sweep_ontology_orphans(target, &manifest);
    Ok(())
}

/// The orphan-sweep protocol for one immutable object family directory
/// (ADR-0089, ADR-0120 §5): an entry `is_candidate` accepts (the family's
/// generated-name grammar) that `keep` rejects (the published manifest
/// does not reference it) is a crash-window orphan — the object publishes
/// before the manifest that selects it, so a crash between the two leaves
/// exactly this state. Housekeeping is retryable: a missing or unreadable
/// directory skips the family, a failed delete skips the entry, and
/// neither fails the open. A symlink presented as a family object is never
/// followed.
fn sweep_dir_orphans(
    target: &StoreTarget,
    dir: &str,
    is_candidate: impl Fn(&str) -> bool,
    keep: impl Fn(&str) -> bool,
) {
    if !matches!(target.vfs.exists(dir), Ok(true)) {
        return;
    }
    if let Some(root) = &target.native_root {
        if !native_plain_dir(root, dir) {
            return;
        }
    }
    let Ok(names) = target.vfs.list_dir(dir) else {
        return;
    };
    for name in names {
        let path = format!("{dir}/{name}");
        if !is_candidate(&path) || keep(&path) {
            continue;
        }
        if let Some(root) = &target.native_root {
            if !native_plain_file(root, &path) {
                continue;
            }
        }
        let _ = delete_object(target, &path);
    }
}

pub(crate) fn delete_object(target: &StoreTarget, name: &str) -> Result<()> {
    validate_deletable(target, name)?;
    target.vfs.remove_file(name)
}

/// Best-effort removal of a batch of snapshot objects. Every path is validated
/// exactly as [`delete_object`]; invalid paths are skipped. One
/// [`Vfs::remove_files`] call lets the Bundle retire the whole batch with a
/// single catalog generation instead of one persist per shard.
pub(crate) fn delete_objects(target: &StoreTarget, names: &[String]) {
    let batch: Vec<String> = names
        .iter()
        .filter(|name| validate_deletable(target, name).is_ok())
        .cloned()
        .collect();
    if !batch.is_empty() {
        let _ = target.vfs.remove_files(&batch);
    }
}

fn validate_deletable(target: &StoreTarget, name: &str) -> Result<()> {
    match object_name(name) {
        Some(ObjectName::LegacyShard) => Ok(()),
        Some(ObjectName::Ontology) => ontology::validate_ontology_object(target, name),
        Some(ObjectName::Shard(shard)) => validate_shard_name(target, shard),
        _ => Err(Error::storage(format!(
            "invalid manifest shard path: {name}"
        ))),
    }
}

/// The native alias proof of one shard part: its family directory must be a
/// real directory and the part a plain file, never a symlink/junction.
fn validate_shard_name(target: &StoreTarget, name: ShardName) -> Result<()> {
    let path = name.path();
    let invalid = || Error::storage(format!("invalid manifest shard path: {path}"));
    if let Some(root) = &target.native_root {
        validate_dir(root, name.kind.dir(), "shard directory", DirAdmit::Required)
            .map_err(|_| invalid())?;
        validate_native_plain_file(root, &path, "shard")?;
    }
    Ok(())
}

/// The validate/create/re-validate directory sandwich every immutable
/// object publish performs on its family directory before the first
/// reservation attempt: refuse an alias or non-directory both before and
/// after creating the directory, so a race into the create window is
/// caught (ADR-0089, ADR-0120 §5).
fn prepare_object_dir(target: &StoreTarget, dir: &str, what: &str) -> Result<()> {
    if let Some(root) = &target.native_root {
        validate_dir(root, dir, what, DirAdmit::Optional)?;
    }
    target.vfs.create_dir_all_for_atomic(dir)?;
    if let Some(root) = &target.native_root {
        validate_dir(root, dir, what, DirAdmit::Required)?;
    }
    Ok(())
}

/// The exclusive-reservation policy every generated immutable object
/// runs: pick a fresh name, create it without overwriting, record it in
/// `owned` for cleanup — up to eight collisions, then a refusal. The
/// caller owns directory preparation and what it writes through the
/// handle.
fn reserve_exclusive(
    vfs: &dyn Vfs,
    mut make_name: impl FnMut() -> Result<String>,
    owned: &mut Vec<String>,
    what: &str,
) -> Result<(String, Box<dyn crate::vfs::VfsFile>)> {
    for _ in 0..8 {
        let path = make_name()?;
        if let Some(file) = vfs.open_exclusive(&path)? {
            owned.push(path.clone());
            return Ok((path, file));
        }
    }
    Err(Error::storage(format!(
        "could not reserve a unique {what} path"
    )))
}

/// Exclusive-reservation loop shared by generated immutable objects: pick a
/// fresh name, create it without overwriting, record it in `owned` for
/// cleanup, write and fsync its bytes, then persist the containing directory.
/// A collision retries with a new name; all other failures abort. Callers own
/// any directory preparation before the first attempt.
fn reserve_owned_object(
    vfs: &dyn Vfs,
    dir: &str,
    make_name: impl FnMut() -> Result<String>,
    bytes: &[u8],
    owned: &mut Vec<String>,
    what: &str,
) -> Result<String> {
    let (path, mut file) = reserve_exclusive(vfs, make_name, owned, what)?;
    let result = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    result?;
    vfs.sync_dir(dir)?;
    Ok(path)
}

#[cfg(test)]
mod tests;
