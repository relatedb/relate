//! The open/replay cluster (ADR-0014): load the snapshot, open the WAL at
//! its checkpoint LSN, stream the validated records after it, and fold the
//! store-level ontology state the walk passes (ADR-0120). The write side —
//! checkpoint publication — stays with the facade in `lib.rs`.

use relate_core::{Error, Result};
use relate_engine_api::{OntologyOp, SnapshotGraph};

use crate::snapshot::{self, LoadedSnapshot};
use crate::wal::Wal;
use crate::{codec, version, SnapshotCodecKind, StoreTarget};

/// One codec's checkpoint writer. The graph is the export surface, so the
/// pointer does not name a concrete engine.
type CheckpointDump =
    fn(&StoreTarget, &dyn relate_engine_api::GraphImport, u64, u64, Option<&str>) -> Result<()>;

/// The codec vocabulary's one behavioral surface: what the codec does at
/// the open tail — the dirty-WAL guard plus the load's deferred open-time
/// action — whether it sweeps orphaned store objects after open, how it
/// writes a checkpoint, and how it sizes its snapshot objects. One fn
/// table per codec ([`codec_ops`]), so a codec's behavior is spelled once
/// instead of one arm bolted onto every dispatch the open walk, the write
/// path and the sizing path pass. The loader stays a generic match in
/// [`load_with_checkpoint_lsn`]: the graph type is a type parameter, and a
/// fn pointer in this table cannot carry it. A new codec lands in this
/// table and in that match.
pub(crate) struct CodecOps {
    pub(crate) finish_open:
        fn(&StoreTarget, u64, u64, Option<snapshot::DeferredOpenAction>) -> Result<()>,
    pub(crate) sweep_after_open: fn(&StoreTarget) -> Result<()>,
    pub(crate) dump: CheckpointDump,
    pub(crate) snapshot_data_bytes: fn(&StoreTarget) -> Result<u64>,
}

/// The classic codecs' open-tail default: no dirty-WAL window (their
/// durable form cannot change underneath them) and no deferred action —
/// every replay is admitted, nothing is deferred.
fn admit_every_replay(
    _target: &StoreTarget,
    _wal_lsn: u64,
    _checkpoint_lsn: u64,
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
/// generic match in [`load_with_checkpoint_lsn`]. An uncompiled codec's
/// stub module fails closed, so neither dispatch needs a cfg arm.
pub(crate) fn codec_ops(codec: SnapshotCodecKind) -> CodecOps {
    match codec {
        SnapshotCodecKind::Parquet => CodecOps {
            finish_open: admit_every_replay,
            sweep_after_open: snapshot::sweep_orphans,
            dump: snapshot::checkpoint_at,
            snapshot_data_bytes: snapshot::snapshot_data_bytes,
        },
        SnapshotCodecKind::Json => CodecOps {
            finish_open: admit_every_replay,
            sweep_after_open: nothing_to_sweep,
            dump: codec::json::dump,
            snapshot_data_bytes: codec::json::snapshot_data_bytes,
        },
        SnapshotCodecKind::Markdown => CodecOps {
            finish_open: codec::markdown::finish_open,
            sweep_after_open: nothing_to_sweep,
            dump: codec::markdown::checkpoint_at,
            snapshot_data_bytes: codec::markdown::snapshot_data_bytes,
        },
        SnapshotCodecKind::Rkyv => CodecOps {
            finish_open: admit_every_replay,
            sweep_after_open: nothing_to_sweep,
            dump: codec::rkyv::dump,
            snapshot_data_bytes: codec::rkyv::snapshot_data_bytes,
        },
    }
}

/// A fresh snapshot load of a store — the guarded preamble
/// ([`load_snapshot_with_schema_guard`]) included, like every other
/// fresh load: a drifted `.cypherlite/schema/` mirror refuses here too.
///
/// The snapshot-only materialization boundary: no WAL is replayed here, so
/// this path completes its own indexes (the no-replay half of the split
/// [`replay_after_ontology`] owns for opens that do replay). One
/// completion per path — `finish_restore` in the codec tail builds
/// adjacency only.
pub fn load<G: SnapshotGraph>(target: &StoreTarget) -> Result<G> {
    let graph: G = load_snapshot_with_schema_guard(target)?.graph;
    graph.rebuild_indexes();
    Ok(graph)
}

/// Load the snapshot and its checkpoint LSN from one manifest read
/// (0 when no manifest exists yet), plus the version marker the codec
/// loader decoded (ADR-0127 Decision 2: detection costs no extra I/O) and
/// the ontology document the snapshot carries (ADR-0120; `None` when the
/// store holds none).
fn load_with_checkpoint_lsn<G: SnapshotGraph>(target: &StoreTarget) -> Result<LoadedSnapshot<G>> {
    match target.codec {
        SnapshotCodecKind::Parquet => snapshot::load_with_checkpoint_lsn::<G>(target),
        SnapshotCodecKind::Json => codec::json::load_with_checkpoint_lsn::<G>(target),
        SnapshotCodecKind::Markdown => codec::markdown::load_with_checkpoint_lsn::<G>(target),
        SnapshotCodecKind::Rkyv => codec::rkyv::load_with_checkpoint_lsn::<G>(target),
    }
}

/// The guarded open preamble every fresh snapshot load shares — the two
/// open flows and [`load`]: the snapshot load, then the schema text
/// mirror's drift guard (ADR-0481 §4) against the state that load
/// restored — before the WAL is opened or replayed, so a refusal leaves
/// every durable byte untouched.
/// [`crate::schema_files::verify_open_schema_files`]'s own doc carries
/// the guard's rationale. [`load_with_wal`] deliberately skips this
/// preamble: it re-loads a base for a WAL the caller already holds, not
/// a fresh open.
fn load_snapshot_with_schema_guard<G: SnapshotGraph>(
    target: &StoreTarget,
) -> Result<LoadedSnapshot<G>> {
    let loaded = load_with_checkpoint_lsn::<G>(target)?;
    crate::schema_files::verify_open_schema_files(
        target,
        &loaded.graph,
        loaded.ontology.as_deref(),
    )?;
    Ok(loaded)
}

/// The codec-owned open tail, through the codec's one ops table
/// ([`codec_ops`]): prove the loaded base safe for replay (the dirty-WAL
/// guard, ADR-0406 §5 — with WAL records past the checkpoint LSN, a
/// codec whose durable form can change underneath it refuses the open
/// instead of corrupting the base), then run the load's deferred
/// open-time action — the markdown re-baseline over externally-edited
/// vaults, the only deferred action today. The classic codecs have no
/// such window, admit every replay, and defer nothing. The read-only
/// open calls this with `None` — the guard alone, no deferred action;
/// the writable open passes the action the load deferred.
fn finish_open(
    target: &StoreTarget,
    wal: &Wal,
    checkpoint_lsn: u64,
    deferred: Option<snapshot::DeferredOpenAction>,
) -> Result<()> {
    (codec_ops(target.codec).finish_open)(target, wal.lsn, checkpoint_lsn, deferred)
}

/// The codec-owned post-open sweep, through the codec's one ops table
/// ([`codec_ops`]): the Parquet codec retires store objects its newest
/// manifest orphaned (ADR-0089); the file-per-checkpoint codecs rewrite
/// their image wholesale and have nothing to sweep.
fn sweep_after_open(target: &StoreTarget) -> Result<()> {
    (codec_ops(target.codec).sweep_after_open)(target)
}

/// Open a store: load snapshot, then stream validated WAL records (ADR-0014).
///
/// The snapshot is loaded and validated *before* [`Wal::open`] may repair a
/// torn tail: a store written by a newer Relate (unsupported snapshot
/// `format_version`, JSON `value_encoding`, or read-only marker) fails open
/// with every WAL segment byte untouched (ADR-0114 Decision 2).
///
/// A supported-older store is migrated to the current format before the
/// first query is served (ADR-0127: migrate-on-open, hands-off,
/// forward-only); see [`open_store_with_report`].
pub fn open_store<G: SnapshotGraph>(target: &StoreTarget) -> Result<(G, Wal)> {
    let (graph, wal, _, _) = open_store_with_report(target)?;
    Ok((graph, wal))
}

/// [`open_store`] for a read-only handle (ADR-0136).
///
/// Skips migrate-on-open, orphan sweep, and torn-tail trim. Retries when the
/// manifest changes during the load.
pub fn open_store_readonly<G: SnapshotGraph>(target: &StoreTarget) -> Result<(G, Wal)> {
    let (graph, wal, _) = open_store_readonly_with_ontology::<G>(target)?;
    Ok((graph, wal))
}

/// [`open_store_readonly`] plus the store's current ontology document
/// (ADR-0120), folded exactly as in [`open_store_with_report`].
#[doc(hidden)]
pub fn open_store_readonly_with_ontology<G: SnapshotGraph>(
    target: &StoreTarget,
) -> Result<(G, Wal, Option<String>)> {
    const ATTEMPTS: u32 = 8;
    let mut last_err = None;
    for attempt in 0..ATTEMPTS {
        let before = manifest_stamp(target)?;
        match open_store_readonly_once(target) {
            Ok((graph, wal, ontology)) => {
                let after = manifest_stamp(target)?;
                if before == after {
                    return Ok((graph, wal, ontology));
                }
            }
            Err(e) => last_err = Some(e),
        }
        if attempt + 1 < ATTEMPTS {
            #[cfg(not(target_arch = "wasm32"))]
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    Err(last_err
        .unwrap_or_else(|| Error::storage("read-only open could not observe a stable snapshot")))
}

/// Open the WAL read-only at the published checkpoint, without loading the
/// snapshot.
///
/// [`Wal::poll_follow`] refuses an append handle. A writer that has just
/// checkpointed already holds the graph that snapshot describes, so it can
/// swap in this WAL and keep the graph.
pub fn open_follower_wal(target: &StoreTarget) -> Result<Wal> {
    let checkpoint_lsn = crate::published_checkpoint_lsn(target)?;
    let (vfs, prefix) = target.wal_vfs();
    Wal::open_readonly(vfs, prefix, checkpoint_lsn)
}

/// Open the WAL for append at the published checkpoint, without loading
/// the snapshot.
///
/// [`Wal::open`] rescans `bytes` and `commits` and trims a torn tail. A
/// follower that has applied through the tip swaps this handle in and
/// keeps the loaded graph (ADR-0440).
pub fn open_writer_wal(target: &StoreTarget) -> Result<Wal> {
    Ok(open_writer_wal_with_report(target)?.0)
}

/// [`open_writer_wal`] plus the torn-tail report (ADR-0114): the tails
/// this open trimmed, for the promoting caller to report — empty when
/// recovery found none. The doc-hidden reporting twin, mirroring the
/// [`Wal::open`] / [`Wal::open_with_report`] split so the published
/// signature keeps its shape.
#[doc(hidden)]
pub fn open_writer_wal_with_report(target: &StoreTarget) -> Result<(Wal, Vec<crate::TrimmedTail>)> {
    let checkpoint_lsn = crate::published_checkpoint_lsn(target)?;
    let (vfs, prefix) = target.wal_vfs();
    Wal::open_with_report(vfs, prefix, checkpoint_lsn)
}

fn manifest_stamp(target: &StoreTarget) -> Result<Option<Vec<u8>>> {
    // The one Option-returning checkpoint-image read, stamped through
    // the codec's own `checkpoint_image` — the file that codec's writer
    // actually rewrites — so the before/after comparison observes the
    // image a concurrent checkpoint really touches (`None` when the
    // store has no image yet).
    snapshot::get_object_opt(target, target.codec.checkpoint_image())
}

fn open_store_readonly_once<G: SnapshotGraph>(
    target: &StoreTarget,
) -> Result<(G, Wal, Option<String>)> {
    let LoadedSnapshot::<G> {
        graph,
        checkpoint_lsn,
        ontology,
        ..
    } = load_snapshot_with_schema_guard::<G>(target)?;
    let (wal_vfs, wal_prefix) = target.wal_vfs();
    let wal = Wal::open_readonly(wal_vfs, wal_prefix, checkpoint_lsn)?;
    // The read-only open tail: the one call with no deferred action —
    // for the markdown codec that is exactly the dirty-WAL guard
    // (`markdown::finish_open` runs its guard before any deferred
    // action); the classic codecs admit every replay.
    finish_open(target, &wal, checkpoint_lsn, None)?;
    let ontology = replay_after_ontology(&graph, &wal, checkpoint_lsn, ontology)?;
    Ok((graph, wal, ontology))
}

/// The open path's report: what a writable open did beyond loading — the
/// version migrations it ran (ADR-0127) and the torn WAL tails recovery
/// trimmed (ADR-0114). Open-time output, never handle state: the opener
/// reports it once and drops it.
#[doc(hidden)]
#[derive(Default)]
pub struct OpenReport {
    /// ADR-0127's migration trace — internal to `relate-storage` (the
    /// crate has no logging facility, so it surfaces only through this
    /// channel; its `Display` impl is the trace line a future logging
    /// facility would emit on open).
    pub(crate) migrations: version::MigrationReport,
    /// The torn tails this open trimmed (ADR-0114), for the opener to
    /// report — empty when recovery found none.
    pub trimmed_tails: Vec<crate::TrimmedTail>,
}

/// [`open_store`] plus the store's current ontology document (ADR-0120)
/// and the open report — the one report-returning name in the family: the
/// opener reads `report.trimmed_tails` (ADR-0114) off the named outcome,
/// and the migration trace stays crate-internal (ADR-0127 Decision 8 —
/// the crate has no logging facility, so its `Display` impl is the trace
/// line a future logging facility would emit on open).
///
/// ADR-0127 Decisions 2–4: one open flow. The codec loaders report the
/// version marker they decoded, so detection costs no extra I/O; a legacy
/// marker on a durable VFS is then migrated on the handles already open —
/// checkpoint at the current format, rotate to a fresh segment with the
/// current magic, recycle the covered segments. There is no second load,
/// `Wal::open` or replay. Checkpoint publication follows the manifest-last
/// tmp/rename rules (ADR-0089/ADR-0092), so a crash mid-migration leaves
/// the old store intact and the next open re-migrates from scratch.
/// `:memory:` never migrates (Decision 7). A store with an unreadable
/// marker (newer binary, unknown magic) fails at the ADR-0114 gate inside
/// the load, before any byte mutates.
#[doc(hidden)]
pub fn open_store_with_report<G: SnapshotGraph>(
    target: &StoreTarget,
) -> Result<(G, Wal, Option<String>, OpenReport)> {
    let LoadedSnapshot::<G> {
        graph,
        checkpoint_lsn,
        ontology,
        marker,
        deferred,
    } = load_snapshot_with_schema_guard::<G>(target)?;
    let (wal_vfs, wal_prefix) = target.wal_vfs();
    let (mut wal, trimmed_tails) = Wal::open_with_report(wal_vfs, wal_prefix, checkpoint_lsn)?;
    // The writable-open tail (ADR-0406 §5), codec-owned through the ops
    // table: the dirty-WAL guard first, then — no WAL deltas pending
    // over the loaded base — the load's deferred action runs.
    finish_open(target, &wal, checkpoint_lsn, deferred)?;
    let ontology = replay_after_ontology(&graph, &wal, checkpoint_lsn, ontology)?;
    let mut report = OpenReport {
        migrations: version::MigrationReport::default(),
        trimmed_tails,
    };
    if let version::SnapshotMarker::Legacy(subsystem, from) = marker {
        if target.is_fully_durable() {
            // The published version from the same codec/ontology facts
            // the checkpoint below hands its codec — each subsystem's
            // canonical derivation, never a per-subsystem constant.
            let to = match subsystem {
                #[cfg(manifest_codec)]
                version::Subsystem::Manifest => u64::from(version::manifest::published_with(
                    target.codec,
                    ontology.is_some(),
                )),
                version::Subsystem::ValueEncoding => {
                    version::value_encoding::version_for_write(ontology.is_some())
                }
            };
            report.migrations.record(subsystem, from, to);
            // The migration checkpoint carries the loaded ontology document
            // so a content-mismatched marker (ADR-0120 §8) is normalized,
            // never dropped. It publishes at the graph's own commit
            // timestamp, like every checkpoint.
            crate::checkpoint_at(
                target,
                &graph,
                wal.lsn,
                graph.commit_ts(),
                ontology.as_deref(),
            )?;
            // Rotate to a fresh segment with the current magic and recycle
            // every segment the checkpoint covers. A fully checkpointed
            // tail leaves no commits on the open segment, so `rotate` is a
            // no-op and the open segment — whose magic `Wal::open` already
            // verified as readable — stays active.
            let cut = wal.rotate()?;
            wal.recycle(cut)?;
        }
    }
    sweep_after_open(target)?;
    Ok((graph, wal, ontology, report))
}

/// Materialize the live graph: snapshot + streamed WAL records after `checkpoint_lsn`.
pub fn load_with_wal<G: SnapshotGraph>(target: &StoreTarget, wal: &Wal) -> Result<G> {
    let LoadedSnapshot::<G> {
        graph,
        checkpoint_lsn,
        ..
    } = load_with_checkpoint_lsn::<G>(target)?;
    replay_after(&graph, wal, checkpoint_lsn)?;
    Ok(graph)
}

fn replay_after(
    graph: &dyn relate_engine_api::GraphImport,
    wal: &Wal,
    checkpoint_lsn: u64,
) -> Result<()> {
    replay_after_ontology(graph, wal, checkpoint_lsn, None).map(|_| ())
}

/// [`replay_after`] plus the store-level ontology state (ADR-0120 §1/§3):
/// the snapshot's document folded with every ontology op the replay walks
/// (the last op wins — a set no checkpoint covered yet, or a drop after
/// one). The ops themselves are graph no-ops; the document lives with the
/// store layer.
fn replay_after_ontology(
    graph: &dyn relate_engine_api::GraphImport,
    wal: &Wal,
    checkpoint_lsn: u64,
    snapshot: Option<String>,
) -> Result<Option<String>> {
    let mut ontology = snapshot;
    wal.for_each_after(checkpoint_lsn, |rec| {
        for op in &rec.ops {
            // The executor's typed classifier: a future ontology-affecting
            // redo variant forces a compile-time decision at its enum site,
            // not a silent miss in this fold.
            match op.ontology_op() {
                Some(OntologyOp::Set(document)) => ontology = Some(document.to_owned()),
                Some(OntologyOp::Drop) => ontology = None,
                None => {}
            }
        }
        graph.apply_replayed_redo(rec.ops, rec.commit_ts)
    })?;
    // The one wholesale index rebuild of a replaying open, after the graph
    // is final: `finish_restore` builds only adjacency, so this site owns
    // the indexes (LABS-1112 — the old flow paid it twice on a snapshot+tail
    // reopen, and once even on a clean one). Unconditional, deliberately:
    // any skip condition has to be right about every shape (a checkpoint can
    // carry rows at lsn 0, and a store can carry constraints with no live
    // nodes — constraint keys must build), and the cheapest honest probe
    // (`live_node_count`) materializes and sorts the node-id list this call
    // walks anyway. The rebuild over an empty graph is a clear plus an empty
    // refill.
    graph.rebuild_indexes();
    Ok(ontology)
}
