//! The sidecar manifest (ADR-0406 §5): one home for the wire types,
//! the bounded read/write, and the typed probe every "is this a
//! markdown store" question consults.
//!
//! Extracted from `mod.rs` (the no-1k-line rule, LABS-311).

use std::collections::{BTreeMap, BTreeSet};

use relate_core::{Error, Result};
use relate_engine_api::CheckpointTheater;
use relate_markdown as md;
use serde::{Deserialize, Serialize};

#[cfg(not(target_arch = "wasm32"))]
use super::super::benign_absence;
use super::super::catalog;
use super::layout::{
    engine_dir, engine_dir_probe_error, legacy_sidecar_error, LegacySidecarRepair, SidecarSpelling,
    ENGINE_DIR, LEGACY_ENGINE_DIR, LEGACY_MANIFEST, MANIFEST,
};
use super::vault::crc;
use super::MarkdownBodyProperties;
use crate::codec::SnapshotCodecKind;
use crate::snapshot::MAX_JSON_OBJECT;
use crate::vfs::{atomic_write, read_to_vec, Vfs};
use crate::StoreTarget;

/// The copy-on-write journal directory (ADR-0406 §5): its own macro so
/// the three journal paths build from one spelling of the segment.
macro_rules! journal_dir {
    () => {
        concat!(engine_dir!(), "/checkpoint-journal")
    };
}
pub(super) const JOURNAL_DIR: &str = journal_dir!();
pub(super) const JOURNAL_INTENT: &str = concat!(journal_dir!(), "/intent.json");
/// The pre-image of a journaled checkpoint: the paths that existed
/// before it began. Rollback deletes only intent files absent from
/// this set — a file that predates the checkpoint is restored, never
/// deleted (ADR-0406 §5).
pub(super) const JOURNAL_PREIMAGE: &str = concat!(journal_dir!(), "/preimage.json");

// The sidecar's metadata bound is the ADR-0086 per-metadata-object
// ceiling, spelled through the one crate-visible home
// (`snapshot::MAX_JSON_OBJECT`) at every check site below — never a
// mirrored second constant.
/// The generated/learned schema (ADR-0406 §2): descriptive, never
/// prescriptive — the vault stays authoritative and drift regenerates
/// it at the next checkpoint.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
pub(super) struct LearnedSchema {
    /// Label → its key property name (the filename source).
    #[serde(default)]
    pub(super) keys: BTreeMap<String, String>,
    /// The relationship-type vocabulary the frontmatter classification
    /// consults.
    #[serde(default)]
    pub(super) rel_types: BTreeSet<String>,
    /// Files whose frontmatter does not parse (real Obsidian vaults
    /// carry `@handle` values, Templater placeholders, malformed
    /// mappings). They load body-only — Obsidian's own behavior — and
    /// are preserved verbatim at checkpoint: the raw text is the user's,
    /// never silently rewritten (ADR-0406 §6 warnings).
    #[serde(default)]
    pub(super) raw_files: BTreeSet<String>,
    /// Files whose vault key was a case-only duplicate and folded to
    /// the first spelling (ADR-0406 §2) — the load's warning record;
    /// the loser file is deleted at the next checkpoint.
    #[serde(default)]
    pub(super) folded_keys: BTreeSet<String>,
}

/// Equality is the publication gate's comparison basis (ADR-0406 §5):
/// the load's re-baselined expectation compares against the sidecar it
/// read as one whole value, so every field the load overwrites
/// participates by construction — `FileStat`'s content-only equality
/// (mtime excluded) carries through the derive.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
pub(crate) struct VaultManifest {
    pub(super) codec: Option<SnapshotCodecKind>,
    #[serde(default)]
    pub(crate) checkpoint_lsn: u64,
    #[serde(default)]
    pub(crate) commit_ts: u64,
    #[serde(default)]
    pub(super) value_encoding: Option<u64>,
    #[serde(default)]
    pub(super) next_node_id: i64,
    #[serde(default)]
    pub(super) next_rel_id: i64,
    /// The catalog and graph type, carried by the one wire carrier —
    /// flattened inline, so the sidecar's shape is unchanged.
    #[serde(flatten)]
    pub(super) catalog: catalog::CatalogWire,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::manifest::deserialize_checkpoint_theater"
    )]
    pub(super) theater: Option<CheckpointTheater>,
    #[serde(default)]
    pub(super) read_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) ontology: Option<String>,
    #[serde(default)]
    pub(super) learned: LearnedSchema,
    /// The body-property configuration (ADR-0460): the ordered default
    /// list plus per-label overrides. Store configuration, not learned
    /// schema — recorded at every checkpoint, replaced only by an
    /// explicit open-time config; an omitted field is the legacy
    /// single-property `body` form.
    #[serde(default, skip_serializing_if = "MarkdownBodyProperties::is_default")]
    pub(super) bodies: MarkdownBodyProperties,
    /// Embeddings (vector-indexed properties), keyed `"<vault path>#<property>"`.
    #[serde(default)]
    pub(super) embeddings: BTreeMap<String, serde_json::Value>,
    /// Every guarded node file at the last checkpoint with its length
    /// and crc32 — the dirty-WAL and external-write guard's expectation.
    #[serde(default)]
    pub(super) files: BTreeMap<String, FileStat>,
    /// The stable node ids (ADR-0406 §5's WAL-recovery contract), keyed
    /// by vault key: restored at load so id-based WAL replay after a
    /// checkpoint addresses the same nodes the writes named.
    #[serde(default)]
    pub(super) node_ids: BTreeMap<String, i64>,
    /// The stable relationship ids, keyed by the canonical relationship
    /// identity (source key, type, target key, sorted properties).
    #[serde(default)]
    pub(super) rel_ids: BTreeMap<String, i64>,
}

/// The stat-first digest of one guarded file: length (cheap, consulted
/// first) and crc32 of content (consulted on length match).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub(super) struct FileStat {
    pub(super) len: u64,
    pub(super) crc: u32,
    /// The entry's modification time when the VFS reports one — the
    /// stat-first half of the ADR-0406 §5 guard (absent on virtual
    /// backends; the crc then proves the file).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) mtime: Option<u64>,
}

/// A file's identity is its content digest (length + crc); `mtime` is
/// advisory guard metadata — a fresh write can carry a different mtime
/// with identical content, so it never participates in equality
/// (vault_matches) — only the stat-first guard consults it.
impl PartialEq for FileStat {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len && self.crc == other.crc
    }
}
impl Eq for FileStat {}

impl FileStat {
    /// The stat of `bytes` as the guards record it; the caller fills
    /// `mtime` from the VFS when it reports one.
    pub(super) fn of(bytes: &[u8]) -> Self {
        Self {
            len: bytes.len() as u64,
            crc: crc(bytes),
            mtime: None,
        }
    }

    /// Does `bytes` differ from this recorded stat (length first, then
    /// the crc)? The one differ predicate the journal's touched check
    /// and the crc fallback of the guards consult.
    pub(super) fn differs_from(&self, bytes: &[u8]) -> bool {
        self.len != bytes.len() as u64 || self.crc != crc(bytes)
    }
}

/// What the sidecar manifest says about a directory (ADR-0406 §5) —
/// the one typed classification every "is this a markdown store"
/// question consults, at either spelling: codec resolution, the
/// pre-lease default-WAL routing, the reset refusal, the manifest
/// readers, and the legacy presence probes below.
#[derive(Clone, Debug)]
pub(crate) enum ManifestProbe {
    /// No sidecar: not a markdown store (yet).
    Absent,
    /// A sidecar naming the markdown codec, parsed.
    Markdown(Box<VaultManifest>),
    /// A sidecar naming another codec.
    Other(SnapshotCodecKind),
    /// A sidecar that is present but oversized, unreadable,
    /// unparseable, or names no codec: every fail-closed decision
    /// treats the directory as a markdown store anyway (the reset
    /// refusal and `.relate/` WAL routing must not be bypassable
    /// by a corrupt manifest).
    Unparseable,
}

/// The one bounded read + classification of the sidecar at a spelling
/// (either, ADR-0623): the metadata length bounds the read before any
/// byte is fetched, the bytes classify through the one bytes-level
/// classifier, and every probe failure — `exists`, `metadata`, the
/// read itself — speaks the one probe-context sentence naming the
/// spelling's engine directory, the directory actually probed: the
/// [`SidecarSpelling`] value pairs the path with the name, so neither
/// spelling's failures can ever name the other's.
pub(super) fn probe_sidecar(vfs: &dyn Vfs, spelling: SidecarSpelling) -> Result<ManifestProbe> {
    let name = spelling.manifest();
    let fail = |err: Error| engine_dir_probe_error(spelling.engine_dir(), &err);
    if !vfs.exists(name).map_err(fail)? {
        return Ok(ManifestProbe::Absent);
    }
    let len = vfs.metadata(name).map_err(fail)?.len;
    if len > MAX_JSON_OBJECT {
        return Ok(ManifestProbe::Unparseable);
    }
    let bytes = read_to_vec(vfs, name).map_err(fail)?;
    Ok(classify_manifest_bytes(&bytes))
}

/// The markdown codec's evidence probe — the family's
/// `has_store_files`, the shape every other codec's evidence row
/// speaks: the sidecar manifest's presence at either spelling, through
/// the canonical existence probe the other rows use. Side-effect-free:
/// no parse, no classification, no refusal — the classification stays
/// the manifest reader's, and the evidence table only counts the
/// store's presence (ADR-0623: an un-adopted legacy store is still
/// markdown evidence).
pub(crate) fn has_store_files(target: &StoreTarget) -> Result<bool> {
    Ok(crate::snapshot::object_exists(target, MANIFEST)?
        || crate::snapshot::object_exists(target, LEGACY_MANIFEST)?)
}

/// The host-side presence collapse of the one typed classification
/// ([`probe_manifest_at`]) at one spelling: a markdown or unparseable
/// sidecar is a markdown store, and so is a probe that failed — the
/// reset refusal must not be bypassable by an unreadable sidecar.
/// Another codec's sidecar is not.
#[cfg(not(target_arch = "wasm32"))]
fn sidecar_presence_at(dir: &std::path::Path, spelling: SidecarSpelling) -> bool {
    matches!(
        probe_manifest_at(dir, spelling),
        Ok(ManifestProbe::Markdown(_)) | Ok(ManifestProbe::Unparseable) | Err(_)
    )
}

/// The sidecar's one typed presence answer at a host path (ADR-0623):
/// the same metadata-bounded, fail-closed classification as the VFS
/// probe ([`probe_sidecar`]), at the given spelling — a benign absence
/// is `Absent`, an oversized or unparseable sidecar is `Unparseable`,
/// a parsed one classifies by its codec, and a present-but-unreadable
/// sidecar classifies fail-closed as `Unparseable` (never bypassable
/// as "not a store"). A probe that could not even ask — a stat error
/// that is not a benign absence — is `Err`: the consumers that surface
/// probe failures (the read-only refusals) speak it through the
/// adoption module's probe error, and the ones that must not be
/// bypassed (the reset guard) collapse it to a markdown-store verdict
/// above.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn probe_manifest_at(
    dir: &std::path::Path,
    spelling: SidecarSpelling,
) -> Result<ManifestProbe> {
    let sidecar = dir.join(spelling.manifest());
    // The same metadata-first bound as the VFS probe.
    match std::fs::metadata(&sidecar) {
        Err(err) if benign_absence(&err) => return Ok(ManifestProbe::Absent),
        Err(err) => return Err(err.into()),
        Ok(meta) if meta.len() > MAX_JSON_OBJECT => return Ok(ManifestProbe::Unparseable),
        Ok(_) => {}
    }
    match std::fs::read(&sidecar) {
        // A present sidecar that cannot be read classifies fail-closed
        // as unparseable: never "not a store".
        Err(err) if !benign_absence(&err) => Ok(ManifestProbe::Unparseable),
        // A benign read absence is a removal race with the metadata
        // above: the sidecar is gone.
        Err(_) => Ok(ManifestProbe::Absent),
        Ok(bytes) => Ok(classify_manifest_bytes(&bytes)),
    }
}

/// Whether a markdown sidecar exists at either spelling (ADR-0623) —
/// the host-side presence collapse the reset guard and the pre-lease
/// WAL hint both consult: a vault that has not been adopted yet is
/// still a markdown store for both, so neither check can depend on the
/// adoption having happened.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn sidecar_at_either(dir: &std::path::Path) -> bool {
    sidecar_presence_at(dir, SidecarSpelling::Adopted)
        || sidecar_presence_at(dir, SidecarSpelling::Legacy)
}

/// Refuse [`crate::reset`](super::super::super::reset) on a markdown
/// vault store (ADR-0406 §6): the vault's markdown files are
/// hand-authored content — deleting them is the user's call, by hand.
/// The codec owns the policy; the reset entry stays codec-neutral.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn refuse_reset(dir: &std::path::Path) -> Result<()> {
    if sidecar_at_either(dir) {
        return Err(md::vault_error(format!(
            "refusing to reset a markdown vault store; delete the vault's {ENGINE_DIR}/ \
             directory (or the legacy {LEGACY_ENGINE_DIR}/ one) and the markdown files \
             by hand"
        )));
    }
    Ok(())
}

/// The bytes-level classification shared by the VFS probe and the
/// pre-lease path probe, so the policy has exactly one home. `Absent`
/// is never returned: only the probes' existence checks produce it.
fn classify_manifest_bytes(bytes: &[u8]) -> ManifestProbe {
    if bytes.len() as u64 > MAX_JSON_OBJECT {
        return ManifestProbe::Unparseable;
    }
    match serde_json::from_slice::<VaultManifest>(bytes) {
        Ok(manifest) => match manifest.codec {
            Some(SnapshotCodecKind::Markdown) => ManifestProbe::Markdown(Box::new(manifest)),
            Some(other) => ManifestProbe::Other(other),
            None => ManifestProbe::Unparseable,
        },
        Err(_) => ManifestProbe::Unparseable,
    }
}

/// A sidecar that parses but names another codec — the one refusal,
/// shared by both spellings' readers.
fn not_markdown_sidecar(other: SnapshotCodecKind) -> Error {
    md::vault_error(format!(
        "sidecar manifest names codec {other}, not markdown"
    ))
}

/// A sidecar that is present but does not parse, or names no codec —
/// the one refusal, shared by both spellings' readers.
fn unparseable_sidecar() -> Error {
    md::vault_error("sidecar manifest names no codec or does not parse, not markdown")
}

/// The reachable legacy-sidecar repair for this store (ADR-0623): a
/// native root adopts at its next writable open; a caller-supplied VFS
/// has no folder rename every backend can perform, so its owner does
/// it — or constructs the store writable once, which adopts through
/// the backend's own `Vfs::rename`.
fn legacy_repair_for(target: &StoreTarget) -> LegacySidecarRepair {
    if target.native_root.is_some() {
        LegacySidecarRepair::AdoptAtNextWritableOpen
    } else {
        LegacySidecarRepair::CallerRenames
    }
}

/// How the manifest reader answers a legacy-spelling markdown sidecar
/// (ADR-0623): the load refuses — resolving it would open a
/// split-brain store, reads at `.cypherlite/` and writes at
/// `.relate/` — while the cold read accepts: a cold read never adopts,
/// but an un-adopted store still deserves its envelope. Every other
/// legacy shape (unparseable, another codec) refuses identically
/// either way.
enum LegacyRead {
    Refuse,
    Accept,
}

/// The one manifest reader (ADR-0406 §5, ADR-0623): the bounded probe
/// and classification at the adopted spelling first, the legacy
/// spelling second — [`LegacyRead`] supplies the one difference between
/// its two callers, the legacy markdown outcome; everything else is
/// spelled once here.
fn read_manifest_with(target: &StoreTarget, legacy: LegacyRead) -> Result<Option<VaultManifest>> {
    match probe_sidecar(&*target.vfs(), SidecarSpelling::Adopted)? {
        ManifestProbe::Markdown(manifest) => Ok(Some(*manifest)),
        ManifestProbe::Other(other) => Err(not_markdown_sidecar(other)),
        ManifestProbe::Unparseable => Err(unparseable_sidecar()),
        ManifestProbe::Absent => match probe_sidecar(&*target.vfs(), SidecarSpelling::Legacy)? {
            ManifestProbe::Absent => Ok(None),
            ManifestProbe::Markdown(manifest) => match legacy {
                LegacyRead::Refuse => Err(legacy_sidecar_error(legacy_repair_for(target))),
                LegacyRead::Accept => Ok(Some(*manifest)),
            },
            ManifestProbe::Unparseable => Err(legacy_sidecar_error(legacy_repair_for(target))),
            ManifestProbe::Other(other) => Err(not_markdown_sidecar(other)),
        },
    }
}

/// The load's manifest reader: the legacy markdown sidecar refuses
/// with the reachable repair (ADR-0623).
pub(crate) fn read_manifest(target: &StoreTarget) -> Result<Option<VaultManifest>> {
    read_manifest_with(target, LegacyRead::Refuse)
}

/// The cold read's sidecar projection (ADR-0165, ADR-0623): the same
/// reader at either spelling, accepting the legacy markdown sidecar
/// instead of refusing it — a cold read never adopts and never
/// renames, but a pre-rename vault still projects its envelope instead
/// of reporting a fresh store.
pub(crate) fn read_manifest_cold(target: &StoreTarget) -> Result<Option<VaultManifest>> {
    read_manifest_with(target, LegacyRead::Accept)
}

pub(super) fn write_manifest(vfs: &dyn Vfs, manifest: &VaultManifest) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|e| md::vault_error(format!("serializing sidecar manifest: {e}")))?;
    if bytes.len() as u64 > MAX_JSON_OBJECT {
        return Err(md::vault_error(format!(
            "{MANIFEST} exceeds {MAX_JSON_OBJECT}-byte metadata limit"
        )));
    }
    atomic_write(vfs, MANIFEST, &bytes)
}
