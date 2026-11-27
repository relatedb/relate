mod catalog;
mod convert;
pub(crate) mod cut;
// The experimental codec follows the Parquet codec's optional-module shape
// (see `snapshot`'s `native`/`unavailable` pair): the implementation file
// compiles only with its feature, and a same-named stub takes its place
// otherwise, failing closed through `codec_unavailable` — so the dispatch
// matches in `lib.rs` and the ops table in `open.rs` stay cfg-free, one
// unconditional arm per codec.
/// The experimental codecs' shared machinery — the checkpoint manifest
/// and the load tail their decoders hand off to. Always compiled: the
/// Vortex codec's storage half compiles unconditionally and hands its
/// rows through it.
mod experimental;
pub(crate) use experimental::{
    assemble_loaded, checkpoint_identity_mismatch, content_encoding, publish_staged_payload,
    read_codec_manifest, StagedNames,
};
pub mod json;
mod kind;

pub(crate) mod markdown;
mod model;
pub(crate) use model::ensure_dump_ids;
#[cfg(parquet_codec)]
pub(crate) use model::ensure_dump_shape;
pub(crate) mod open_config;
#[cfg(parquet_codec)]
pub(crate) mod parquet;
#[cfg(rkyv_codec)]
pub(crate) mod rkyv;
#[cfg(not(rkyv_codec))]
pub(crate) mod rkyv {
    use super::{codec_unavailable, SnapshotCodecKind};
    use crate::snapshot::LoadedSnapshot;
    use crate::{Result, StoreTarget};

    pub(crate) fn dump(
        _: &StoreTarget,
        _: &super::cut::CheckpointCut<'_>,
        _: super::cut::Rewrite,
    ) -> Result<()> {
        Err(codec_unavailable(SnapshotCodecKind::Rkyv))
    }

    pub(crate) fn load_with_checkpoint_lsn<G: relate_engine_api::SnapshotGraph>(
        _: &StoreTarget,
    ) -> Result<LoadedSnapshot<G>> {
        Err(codec_unavailable(SnapshotCodecKind::Rkyv))
    }

    pub(crate) fn snapshot_data_bytes(_: &StoreTarget) -> Result<u64> {
        Err(codec_unavailable(SnapshotCodecKind::Rkyv))
    }

    /// No evidence in a build without this codec (an uncompiled codec's
    /// store files cannot be its evidence).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn has_store_files(_: &StoreTarget) -> Result<bool> {
        Ok(false)
    }
}

mod shard_codec;
/// The experimental Vortex codec's storage-general half (ADR-0665): the
/// payload manifest, the row-range shard file names and the orphan
/// retirement. The vortex-typed byte serialization arrives through the
/// process-wide [`shard_codec`] registry the experiment crate installs, so
/// this module compiles without the Vortex dependency.
pub(crate) mod vortex;

/// The Vortex store's payload manifest — the JSON object naming the row
/// shards beside the shared envelope.
pub(crate) const GRAPH_VORTEX_META: &str = "graph.vortex.json";

/// The Vortex payload manifest's staging and backup object names — the
/// manifest publishes through the staged swap (the same policy as the
/// rkyv archive's [`GRAPH_RKYV_STAGING`]/[`GRAPH_RKYV_BACKUP`]), never by
/// an in-place overwrite.
pub(crate) const GRAPH_VORTEX_META_STAGING: &str = "graph.vortex.json.new";
pub(crate) const GRAPH_VORTEX_META_BACKUP: &str = "graph.vortex.json.old";

/// The single archive object of an rkyv store — declared once for the
/// real module and the evidence probe alike. Dead in a build without the
/// codec (only the codec module and the codec-gated tests name it).
#[cfg_attr(not(rkyv_codec), allow(dead_code))]
pub(crate) const GRAPH_RKYV: &str = "graph.rkyv";

/// The rkyv archive's staging and backup object names — publication
/// swaps the live archive through them by rename, never by a byte copy.
/// Dead in a build without the codec, like [`GRAPH_RKYV`] above.
#[cfg_attr(not(rkyv_codec), allow(dead_code))]
pub(crate) const GRAPH_RKYV_STAGING: &str = "graph.rkyv.new";
#[cfg_attr(not(rkyv_codec), allow(dead_code))]
pub(crate) const GRAPH_RKYV_BACKUP: &str = "graph.rkyv.old";

pub use kind::SnapshotCodecKind;
/// The shard-codec install hook the experiment crate calls (ADR-0665),
/// with the encode/decode types its signature names.
#[doc(hidden)]
pub use shard_codec::{install as install_shard_codec, ShardDecode, ShardEncode};

use relate_core::{Error, Result};

use crate::snapshot;
use crate::StoreTarget;

/// One codec's checkpoint writer. The cut is the export surface, so the
/// pointer does not name a concrete engine: it carries the frozen image,
/// the live graph the rows are read from, the ontology document the codecs
/// publish (ADR-0120), and the LSN and snapshot timestamp the dump
/// records. The rewrite policy is the one fact that is not the cut's —
/// it is the trigger's classification of this checkpoint.
pub(crate) type CheckpointDump =
    fn(&StoreTarget, &cut::CheckpointCut<'_>, cut::Rewrite) -> Result<()>;

/// The replay window an open hands the codec's open tail: where the WAL
/// stands (`wal_lsn`) and the checkpoint it replays from
/// (`checkpoint_lsn`) — the pair [`CodecOps::finish_open`] admits or
/// defers replay against. Named at the boundary, so the two LSNs cannot
/// be swapped or blurred by a bare `u64` pair.
#[derive(Clone, Copy)]
pub(crate) struct ReplayWindow {
    pub(crate) wal_lsn: u64,
    pub(crate) checkpoint_lsn: u64,
}

/// The codec vocabulary's one behavioral surface: what the codec does at
/// the open tail — the dirty-WAL guard plus the load's deferred open-time
/// action — whether it sweeps orphaned store objects after open, how it
/// writes a checkpoint, how it sizes its snapshot objects, and whether it
/// has already published a checkpoint image (the no-op checkpoint skip's
/// presence probe, ADR-0646). One fn
/// table per codec ([`codec_ops`]), so a codec's behavior is spelled once
/// instead of one arm bolted onto every dispatch the open walk, the write
/// path and the sizing path pass. The loader stays a generic match in
/// `open::load_with_checkpoint_lsn`: the graph type is a type parameter, and
/// a fn pointer in this table cannot carry it. A new codec lands in this
/// table and in that match.
pub(crate) struct CodecOps {
    pub(crate) finish_open:
        fn(&StoreTarget, ReplayWindow, Option<snapshot::DeferredOpenAction>) -> Result<()>,
    pub(crate) sweep_after_open: fn(&StoreTarget) -> Result<()>,
    pub(crate) dump: CheckpointDump,
    pub(crate) snapshot_data_bytes: fn(&StoreTarget) -> Result<u64>,
    /// Whether the store has already published a checkpoint image —
    /// the presence probe the facade's no-op checkpoint skip consults
    /// (a store that has never checkpointed still needs its first
    /// dump to pin its codec and materialize its files). One row per
    /// codec: the classic codecs' root `manifest.json`, the markdown
    /// codec's `.relate/` sidecar — each the evidence probe that
    /// codec's own resolution already speaks.
    pub(crate) has_published_snapshot: fn(&StoreTarget) -> Result<bool>,
}

/// The classic codecs' open-tail default: no dirty-WAL window (their
/// durable form cannot change underneath them) and no deferred action —
/// every replay is admitted, nothing is deferred.
fn admit_every_replay(
    _target: &StoreTarget,
    _window: ReplayWindow,
    _deferred: Option<snapshot::DeferredOpenAction>,
) -> Result<()> {
    Ok(())
}

/// The file-per-checkpoint codecs' sweep default: the checkpoint image is
/// rewritten wholesale, so no object is orphaned and there is nothing to
/// retire.
fn nothing_to_sweep(_target: &StoreTarget) -> Result<()> {
    Ok(())
}

/// The one codec → ops mapping: Parquet overrides the sweep (it retires
/// objects its newest manifest orphaned, ADR-0089), markdown overrides the
/// open tail (the dirty-WAL guard and the deferred re-baseline, ADR-0406
/// §5), and every codec names its own writer and sizer. The loader is the
/// generic match in `open::load_with_checkpoint_lsn`. An uncompiled codec's
/// stub module fails closed, so neither dispatch needs a cfg arm.
pub(crate) fn codec_ops(codec: SnapshotCodecKind) -> CodecOps {
    match codec {
        SnapshotCodecKind::Parquet => CodecOps {
            finish_open: admit_every_replay,
            sweep_after_open: snapshot::sweep_orphans,
            dump: snapshot::checkpoint_at,
            snapshot_data_bytes: snapshot::snapshot_data_bytes,
            has_published_snapshot: classic_manifest_present,
        },
        SnapshotCodecKind::Json => CodecOps {
            finish_open: admit_every_replay,
            sweep_after_open: nothing_to_sweep,
            dump: json::dump,
            snapshot_data_bytes: json::snapshot_data_bytes,
            has_published_snapshot: classic_manifest_present,
        },
        SnapshotCodecKind::Markdown => CodecOps {
            finish_open: markdown::finish_open,
            sweep_after_open: nothing_to_sweep,
            dump: markdown::checkpoint_at,
            snapshot_data_bytes: markdown::snapshot_data_bytes,
            has_published_snapshot: markdown::has_store_files,
        },
        SnapshotCodecKind::Rkyv => CodecOps {
            finish_open: admit_every_replay,
            sweep_after_open: nothing_to_sweep,
            dump: rkyv::dump,
            snapshot_data_bytes: rkyv::snapshot_data_bytes,
            has_published_snapshot: classic_manifest_present,
        },
        SnapshotCodecKind::Vortex => CodecOps {
            finish_open: admit_every_replay,
            sweep_after_open: nothing_to_sweep,
            dump: vortex::dump,
            snapshot_data_bytes: vortex::snapshot_data_bytes,
            has_published_snapshot: classic_manifest_present,
        },
    }
}

/// The classic codecs' published-snapshot row: the shared root
/// envelope's presence, the same object every classic checkpoint
/// rewrites.
fn classic_manifest_present(target: &StoreTarget) -> Result<bool> {
    snapshot::object_exists(target, snapshot::MANIFEST_JSON)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct Envelope {
    #[serde(default = "SnapshotCodecKind::absent")]
    pub(crate) codec: SnapshotCodecKind,
    #[serde(default)]
    pub(crate) checkpoint_lsn: u64,
    #[serde(default)]
    pub(crate) commit_ts: u64,
    #[serde(default = "default_query_engine")]
    pub(crate) query_engine: String,
    #[serde(default = "default_write_engine")]
    pub(crate) write_engine: String,
}

fn default_query_engine() -> String {
    relate_engine_api::MEMORY_QUERY_ENGINE.to_owned()
}

fn default_write_engine() -> String {
    relate_engine_api::LOCAL_WRITE_ENGINE.to_owned()
}

impl Envelope {
    /// The envelope's commit-timestamp floor and checkpoint LSN — the
    /// shared policy ([`envelope_commit_ts`]), on the type that owns the
    /// fields.
    pub(crate) fn envelope_meta(&self) -> Result<(Option<u64>, u64)> {
        Ok((envelope_commit_ts(self.commit_ts)?, self.checkpoint_lsn))
    }
}

/// A codec resolved from disk (or requested by the caller) that this
/// build does not compile in (ADR-0105). Fails closed; never falls back.
pub(crate) fn codec_unavailable(codec: SnapshotCodecKind) -> Error {
    Error::storage(format!(
        "{} snapshot codec is not compiled into this build ({})",
        codec.as_str(),
        codec.unavailable_hint(),
    ))
}

/// The never-written state a root `manifest.json` naming the markdown
/// codec is (ADR-0406 §3): the markdown store's manifest home is the
/// `.relate/` sidecar, so a root envelope naming it — with or
/// without a sidecar — is invalid evidence everywhere it is classified.
/// The refusal lives in [`read_envelope`], so every envelope reader
/// (open's codec resolution and the cold `store_info` read alike)
/// inherits the one typed error instead of re-spelling the branch.
pub(crate) fn root_envelope_names_markdown() -> Error {
    Error::storage(
        "mixed snapshot codecs: a root manifest.json names the markdown codec; the \
         markdown store's manifest is the .relate/ sidecar",
    )
}

/// Whether an I/O error proves a probed engine file cannot be present —
/// plain absence, or a path component that is a regular file (a `.rl`
/// bundle's store root is the file itself, so a path under it can never
/// resolve). Every other error is a present-but-unreadable file and
/// fails closed. Codec-neutral I/O classification, so it lives here —
/// the lowest module both path-level probes share — and the markdown
/// sidecar probe imports it downward; a markdown policy it is not.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn benign_absence(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}

/// The root envelope as the classic-evidence probe reads it: the codec
/// member alone, **required** — the wire envelope defaults every field,
/// so any JSON object would otherwise parse as one, and a vault
/// folder's own unrelated `manifest.json` (no `codec` member, or one
/// outside the vocabulary) is not engine evidence.
#[cfg(not(target_arch = "wasm32"))]
#[derive(serde::Deserialize)]
struct ClassicProbe {
    codec: SnapshotCodecKind,
}

/// The path-level half of the envelope evidence `resolve_codec` counts:
/// a root `manifest.json` that parses as an engine envelope naming a
/// classic codec (an envelope-only classic store — its graph objects
/// removed — still carries its envelope). A markdown store never
/// writes a root manifest.json (its manifest is the `.relate/`
/// sidecar), and a vault folder's own unrelated `manifest.json` does
/// not parse as the probe above, so this never false-refuses a
/// legitimate markdown open.
///
/// Read errors are never folded into "no classic evidence" (the
/// no-silent-fallbacks rule): a benign absence — [`benign_absence`]'s
/// vocabulary — is a definite "no", and every other read error is
/// treated as classic evidence, fail-closed in
/// [`probe_path`](crate::codec::markdown::probe_path)'s direction, so
/// the pre-lease guard refuses to create WAL engine state beside a
/// manifest.json it could not read. The read is bounded like every
/// sibling metadata probe: the length is proven from `metadata` against
/// [`snapshot::MAX_JSON_OBJECT`] before any byte is fetched, and an
/// oversized manifest fails closed exactly like an unreadable one.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn root_envelope_names_classic(path: &std::path::Path) -> bool {
    let manifest = path.join(snapshot::MANIFEST_JSON);
    match std::fs::metadata(&manifest) {
        Err(err) if benign_absence(&err) => return false,
        Err(_) => return true,
        Ok(meta) if meta.len() > snapshot::MAX_JSON_OBJECT => return true,
        Ok(_) => {}
    }
    let bytes = match std::fs::read(&manifest) {
        Ok(bytes) => bytes,
        // A benign absence here is a removal race with the metadata
        // above: the file is gone, so there is no evidence.
        Err(err) if benign_absence(&err) => return false,
        Err(_) => return true,
    };
    let Ok(probe) = serde_json::from_slice::<ClassicProbe>(&bytes) else {
        return false;
    };
    // The typed layout fact decides (engine_dir): a codec whose engine
    // state lives at the store root writes a root manifest.json, and the
    // markdown codec — the one that does not — is already refused by
    // `read_envelope`. A new codec needs no edit here.
    probe.codec.engine_dir().is_none()
}

/// One root-layout codec's path-level store evidence — the row probe
/// [`has_classic_store_evidence_at`] derives from the codec
/// vocabulary. The exhaustive match is compile-forced over the
/// vocabulary's rows, so a codec added to [`SnapshotCodecKind::ALL`]
/// fails to compile until its evidence row exists — none can be
/// silently absent again.
#[cfg(not(target_arch = "wasm32"))]
fn classic_evidence_at(path: &std::path::Path, kind: SnapshotCodecKind) -> bool {
    match kind {
        // Parquet's evidence is its object families — the shard
        // directories and the retired monolith an envelope-less store
        // still carries.
        SnapshotCodecKind::Parquet => snapshot::has_parquet_store_files_at(path),
        // An envelope-only codec's evidence is its payload checkpoint
        // object beside the shared envelope (graph.json, graph.rkyv,
        // graph.vortex.json): presence at the root. The shared envelope
        // object itself is the parse-classified row in
        // `has_classic_store_evidence_at` — a vault folder's own
        // unrelated manifest.json is not engine evidence.
        SnapshotCodecKind::Json | SnapshotCodecKind::Rkyv | SnapshotCodecKind::Vortex => kind
            .checkpoint_objects()
            .iter()
            .filter(|object| **object != snapshot::MANIFEST_JSON)
            .any(|object| path.join(object).exists()),
        // The sidecar family never carries root-layout evidence; the
        // guard's vocabulary filter never asks this row.
        SnapshotCodecKind::Markdown => false,
    }
}

/// The path-level shape of the classic-codec evidence checks — the
/// same evidence vocabulary `resolve_codec` consults, probed before any
/// VFS or lease exists, so an explicit markdown request cannot create
/// WAL engine state inside a snapshot-file store before the codec
/// resolver refuses it. The rows come from the one vocabulary: every
/// root-layout codec (`ALL` minus the sidecar family) contributes its
/// evidence row through [`classic_evidence_at`], and the shared root
/// envelope carries its parse-classified probe — so no codec can be
/// silently absent from the guard.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn has_classic_store_evidence_at(path: &std::path::Path) -> bool {
    root_envelope_names_classic(path)
        || SnapshotCodecKind::ALL
            .into_iter()
            .filter(|kind| kind.engine_dir().is_none())
            .any(|kind| classic_evidence_at(path, kind))
}

/// The envelope's commit-timestamp policy, the one spelling every codec's
/// load shares: `u64::MAX` is the exhaustion the writer refuses (a forged
/// or corrupted value must not become the graph's commit ts), `0` means
/// absent (no floor), anything else floors the loaded graph.
pub(crate) fn envelope_commit_ts(commit_ts: u64) -> Result<Option<u64>> {
    if commit_ts == u64::MAX {
        return Err(Error::storage("manifest timestamp exhausted"));
    }
    Ok(Some(commit_ts).filter(|ts| *ts > 0))
}

pub(crate) fn read_envelope(target: &StoreTarget) -> Result<Option<Envelope>> {
    if !snapshot::object_exists(target, snapshot::MANIFEST_JSON)? {
        return Ok(None);
    }
    let bytes = snapshot::get_object(target, snapshot::MANIFEST_JSON)?;
    let env: Envelope =
        serde_json::from_slice(&bytes).map_err(|e| Error::storage(e.to_string()))?;
    // A root envelope naming the markdown codec is a never-written state
    // (ADR-0406 §3) and invalid evidence everywhere: refused at the one
    // typed reader, so every caller — codec resolution, the cold
    // `store_info` read, the LSN reader — inherits the same error
    // instead of each re-spelling the branch.
    if env.codec == SnapshotCodecKind::Markdown {
        return Err(root_envelope_names_markdown());
    }
    crate::manifest::reject_foreign_engine(&env.query_engine, &env.write_engine)?;
    Ok(Some(env))
}

// Callers (`parse_filesystem_uri`) are native-only, so this scanner and its
// `snapshot::has_parquet_store_files` dependency don't need to compile for
// wasm32.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn resolve_codec(
    target: &StoreTarget,
    requested: Option<SnapshotCodecKind>,
) -> Result<SnapshotCodecKind> {
    let has_json = snapshot::object_exists(target, json::GRAPH_JSON)?;
    let has_parquet = snapshot::has_parquet_store_files(target)?;
    // The experimental codec's evidence: its snapshot object, through the
    // codec's own named probe (the stub module answers "no evidence" for a
    // build without the codec).
    let has_rkyv = rkyv::has_store_files(target)?;
    // The Vortex codec's evidence is its payload manifest, through the
    // codec's own named probe (the stub answers "no evidence" without it).
    let has_vortex = vortex::has_store_files(target)?;
    // The markdown store's evidence is its sidecar manifest at either
    // spelling (ADR-0406 §5, ADR-0623): the codec's side-effect-free
    // presence probe — the classification and its refusals stay the
    // manifest reader's, so an un-adopted legacy store counts as
    // markdown evidence here and meets its refusal where it reads.
    let has_markdown = markdown::has_store_files(target)?;
    // A root envelope naming the markdown codec never reaches this far:
    // `read_envelope` refuses it at the one typed reader, so every
    // caller inherits that error (ADR-0406 §3 — the markdown store's
    // manifest home is the `.relate/` sidecar).
    let envelope = read_envelope(target)?;
    // The evidence set, modeled once: every piece of evidence the
    // directory carries — the markdown sidecar, the root envelope's
    // codec, `graph.json`, the Parquet store files — must name one
    // codec. The set check is exhaustive: any incoherent pair fails
    // here, including combinations no hand-enumerated branch used to
    // spell.
    let evidenced: Vec<SnapshotCodecKind> = [
        has_markdown.then_some(SnapshotCodecKind::Markdown),
        envelope.map(|env| env.codec),
        has_json.then_some(SnapshotCodecKind::Json),
        has_parquet.then_some(SnapshotCodecKind::Parquet),
        has_rkyv.then_some(SnapshotCodecKind::Rkyv),
        has_vortex.then_some(SnapshotCodecKind::Vortex),
    ]
    .into_iter()
    .flatten()
    .collect();
    if let Some((&first, rest)) = evidenced.split_first() {
        if let Some(&other) = rest.iter().find(|codec| **codec != first) {
            return Err(Error::storage(format!(
                "mixed snapshot codecs: directory has both {first} and {other} evidence"
            )));
        }
    }
    // After the set check every surviving element names the one
    // codec, so the first is the inference — no hand-enumerated
    // priority order to drift from the evidence model.
    let inferred = evidenced.first().copied();
    let codec = match (requested, inferred) {
        (Some(want), Some(have)) if want != have => {
            return Err(Error::storage(format!(
                "snapshot codec mismatch: directory is {have}, requested {want}"
            )))
        }
        (Some(want), _) => want,
        (None, Some(have)) => have,
        (None, None) => SnapshotCodecKind::default(),
    };
    check_available(codec)?;
    Ok(codec)
}

pub(crate) fn check_available(codec: SnapshotCodecKind) -> Result<()> {
    if codec.is_compiled() {
        Ok(())
    } else {
        Err(codec_unavailable(codec))
    }
}

#[cfg(all(test, rkyv_codec))]
mod experimental_tests;
#[cfg(test)]
mod tests;

/// Join a scoped worker, resuming its panic in the caller — the one
/// spelling of "a worker's panic is the parent's" the codec tree's
/// parallel passes share (the Parquet shard load, the markdown vault
/// walk, and the striped read).
pub(crate) fn join_or_resume<T>(handle: std::thread::ScopedJoinHandle<'_, T>) -> T {
    match handle.join() {
        Ok(outcome) => outcome,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

/// Run `f` with the thread-local test override `cell` set to `value`,
/// and restore the override it replaced once `f` returns or panics, so
/// overrides of one cell nest — the one override mechanics every codec
/// test seam (a forced worker count) wraps.
#[cfg(test)]
pub(in crate::codec) fn with_test_override<V: 'static, T>(
    cell: &'static std::thread::LocalKey<std::cell::Cell<Option<V>>>,
    value: V,
    f: impl FnOnce() -> T,
) -> T {
    struct Restore<V: 'static> {
        cell: &'static std::thread::LocalKey<std::cell::Cell<Option<V>>>,
        previous: Option<V>,
    }
    impl<V: 'static> Drop for Restore<V> {
        fn drop(&mut self) {
            self.cell.set(self.previous.take());
        }
    }
    let _restore = Restore {
        cell,
        previous: cell.replace(Some(value)),
    };
    f()
}
