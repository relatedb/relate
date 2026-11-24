//! The retained version-chain object family (ADR-0610): the `chains/`
//! directory of lsn-windowed JSON parts a checkpoint publishes while the
//! `value-provenance` preview feature is on and an open loads —
//! codec-agnostic, like the `transactions/` family beside it: every
//! snapshot codec's checkpoint crosses [`crate::checkpoint_at`], which
//! runs [`publish_checkpoint`] after the record family and before the
//! codec's own image, and every codec's load crosses
//! [`crate::open::load_with_checkpoint_lsn`], which runs
//! [`load_retained`] after the codec's own decode.
//!
//! One part per checkpoint interval, named `chains/<lo>-<hi>.json` for
//! the lsn window it covers (`lo` the family's covered tip, `hi` the
//! checkpoint's pin). The body is the interval's dirty delta — every
//! element touched since the last checkpoint with its *complete* version
//! chain, every version's `xmin`/`xmax` beside the row it installed —
//! so the parts are append-only and never rewritten, and the load folds
//! them ascending with the latest part's chain winning per element: no
//! per-version patching, and a tombstone set by an off-period
//! crash-window commit is already in the touched element's next part.
//! A part past the published manifest's checkpoint LSN is a checkpoint
//! that crashed before its manifest landed: the load skips it — the WAL
//! still holds its commits and replays them — so no version is applied
//! twice, and the next successful checkpoint's manifest adopts the part
//! (its window is covered, its content is the true delta of its
//! interval).
//!
//! The family exists only while retention is on: an open without the
//! opt-in publishes no part and drops the family at its checkpoint —
//! the durable half of the collapse (ADR-0610: provenance is bounded by
//! the last provenance-enabled open). A store that never opts in writes
//! no part, ever.

use std::collections::BTreeMap;

use relate_core::{Error, NodeId, RelId, Result};
use relate_engine_api::{GraphImport, RetainedNodeChain, RetainedRelChain};
use serde::Deserialize;

use crate::family::{FlushSink, Flushed, PartFamily, PartKind, RowBatch, PART_CLOSE};
use crate::snapshot::{check_bounded_len, get_object_opt};
use crate::vfs::{atomic_write_stream, VfsFile};
use crate::StoreTarget;

/// The `chains/` retained version-chain family (ADR-0610): the shared
/// part-family plumbing bound to this family — the one spelling of its
/// identity every path consults (never re-derived beside it).
pub(crate) const FAMILY: PartFamily = PartFamily::new(PartKind::Chains);

/// One part's decoded body: the interval's dirty delta — the touched
/// elements' complete chains, nodes and rels each sorted by id. The
/// publish no longer serializes this struct — it streams the same
/// bytes row by row through [`PartWriter`]'s envelope — so the struct
/// is the load's decode shape, and the envelope constants below are
/// pinned to its field spelling by the checkpoint/reopen roundtrips.
#[derive(Deserialize)]
struct PartBody {
    nodes: Vec<RetainedNodeChain>,
    rels: Vec<RetainedRelChain>,
}

/// The part envelope's node-array opening — exactly what a one-shot
/// `serde_json::to_vec` of [`PartBody`] writes before the first node
/// row, so a streamed part decodes through the load's unchanged
/// `from_slice::<PartBody>`.
const NODES_OPEN: &[u8] = b"{\"nodes\":[";

/// The envelope's transition from the node array to the rel array.
const RELS_TRANSITION: &[u8] = b"],\"rels\":[";

/// The one checkpoint step (ADR-0610): while retention is on, publish
/// this interval's dirty delta as one new part covering the lsn window
/// past the family's tip — or write nothing when the interval touched
/// no element or landed no commit. While retention is off, drop the
/// family: the durable half of the collapse an open without the opt-in
/// performs at its checkpoint. Runs before the codec's own image inside
/// [`crate::checkpoint_at`], so a crash between the two leaves at most a
/// part past the still-old manifest — the load skips it and the WAL
/// still holds the same commits.
///
/// The publish streams (ADR-0555): the part's rows flow through the
/// folds one chain at a time — no whole-delta Vec, no one-shot
/// whole-part encode — into a [`PartWriter`] holding one batch, so a
/// publish's peak memory is one batch whatever the interval's dirty
/// delta.
pub(crate) fn publish_checkpoint(
    target: &StoreTarget,
    graph: &dyn GraphImport,
    checkpoint_lsn: u64,
) -> Result<()> {
    if !graph.value_provenance_retention() {
        FAMILY.drop_family(target, &FAMILY.parts(target)?)?;
        return Ok(());
    }
    // The covered tip — the family's one covered-tip helper, shared
    // with the `transactions/` family's publish. A zero-or-negative
    // window means no commit landed since the family's last part — the
    // dirty delta can only be an aborted write's leftover mark, which
    // changed nothing — and is refused before any fold runs.
    let covered_hi = FAMILY.covered_hi(target)?;
    if checkpoint_lsn <= covered_hi {
        return Ok(());
    }
    // The interval's dirty delta, decided from the dirty-id sets the
    // write pass's own folds are driven by — the cheap probe (no
    // element's chain built, no fold run): the publish writes no part
    // when the interval touched no element.
    if graph.dirty_node_ids().is_empty() && graph.dirty_rel_ids().is_empty() {
        return Ok(());
    }
    let name = FAMILY.part_name(covered_hi, checkpoint_lsn);
    // The bounded append: the batches stream into the store's
    // atomic-write temp, and the part's running byte total is proven
    // against [`crate::FAMILY_PART_BYTES`] at every flush through
    // [`check_bounded_len`] — the same class a one-shot encode's
    // `put_object` refusal enforced, from the same policy home — so an
    // interval whose serialized chains exceed the family bound still
    // fails the checkpoint loudly, before the part is ever published.
    atomic_write_stream(&*target.vfs, &name, |file| {
        let mut writer = PartWriter::new(file, &name);
        graph.retained_node_chains_into(&mut |chain| writer.node(chain))?;
        writer.nodes_done()?;
        graph.retained_rel_chains_into(&mut |chain| writer.rel(chain))?;
        writer.finish()
    })
}

/// The `chains/` writer's flush half — [`RowBatch`]'s sink: one filled
/// batch is the next chunk of the interval's single part, written to
/// the part's open temp file with the running total proven against
/// [`crate::FAMILY_PART_BYTES`] through [`check_bounded_len`] at every
/// flush — the loud family ceiling the one-shot encode enforced, from
/// the same policy home.
struct ChunkSink<'a> {
    /// The part's open temp file — [`atomic_write_stream`]'s handle,
    /// which syncs and renames the part once the writer finishes.
    file: &'a mut dyn VfsFile,
    /// The final part name — the running bound check's subject, so a
    /// refusal words the part the reader's own check would refuse.
    name: String,
    /// The part's running total, proven at every flush.
    written: u64,
}

impl FlushSink for ChunkSink<'_> {
    fn flush_batch(&mut self, bytes: &mut Vec<u8>) -> Result<Flushed> {
        self.file
            .write_all(bytes)
            .map_err(|e| Error::storage(e.to_string()))?;
        self.written += bytes.len() as u64;
        check_bounded_len(&self.name, self.written)?;
        Ok(Flushed::Chunk)
    }
}

/// The `chains/` part's streaming writer (ADR-0555): serialized chain
/// rows append through the shared [`RowBatch`] — at most
/// [`crate::FAMILY_PUBLISH_BATCH_BYTES`] plus the row being appended —
/// whose flushes write the next chunk of the part's open temp file, so
/// the publish holds one batch, never the interval's whole delta. The
/// part's running byte total is proven against
/// [`crate::FAMILY_PART_BYTES`] through [`check_bounded_len`] at every
/// flush: the loud family ceiling the one-shot encode enforced, from
/// the same policy home.
struct PartWriter<'a> {
    /// The family's flush half: the open temp file, the part name and
    /// the running bound total.
    chunks: ChunkSink<'a>,
    /// The shared row appender: the batch's bytes and the comma and
    /// budget discipline.
    rows: RowBatch,
    /// Whether the envelope's node array has been opened — the
    /// envelope opens once per part, across the batch flushes, so the
    /// node array's opening rides this flag rather than the batch's
    /// emptiness.
    nodes_open: bool,
}

impl<'a> PartWriter<'a> {
    fn new(file: &'a mut dyn VfsFile, name: &str) -> Self {
        Self {
            chunks: ChunkSink {
                file,
                name: name.to_string(),
                written: 0,
            },
            rows: RowBatch::new(),
            nodes_open: false,
        }
    }

    /// Append one node chain's serialized row, opening the envelope's
    /// node array first.
    fn node(&mut self, chain: &RetainedNodeChain) -> Result<()> {
        if !self.nodes_open {
            self.rows.fragment(NODES_OPEN);
            self.nodes_open = true;
        }
        self.rows.row(b"", chain, &mut self.chunks)?;
        Ok(())
    }

    /// Close the node array and open the rel array — the envelope's
    /// one section transition, written even when the interval touched
    /// no node, so the part decodes with both fields.
    fn nodes_done(&mut self) -> Result<()> {
        if !self.nodes_open {
            self.rows.fragment(NODES_OPEN);
        }
        self.rows.fragment(RELS_TRANSITION);
        Ok(())
    }

    /// Append one relationship chain's serialized row.
    fn rel(&mut self, chain: &RetainedRelChain) -> Result<()> {
        self.rows.row(b"", chain, &mut self.chunks)?;
        Ok(())
    }

    /// Close the envelope and flush the final batch — the last bound
    /// check, so a part exactly at the family bound is refused here
    /// with the class's own error.
    fn finish(mut self) -> Result<()> {
        self.rows.fragment(PART_CLOSE);
        self.rows.flush(&mut self.chunks)
    }
}

/// The one load step (ADR-0610): read every part the published manifest
/// covers, fold them ascending with the latest part's chain winning per
/// element, and hand each element's chain to the graph's restore. A part
/// past the manifest's checkpoint LSN is a checkpoint that crashed
/// before its manifest landed and is skipped (the WAL replays its
/// commits); a covered part that does not decode — or whose chain the
/// restore refuses — fails the open with a typed error naming the
/// object, the same fail-closed posture as a WAL record, never a silent
/// skip. The fold carries each chain's part name beside it, so the
/// restore's refusal words the part the chain came from exactly as the
/// parse's does.
pub(crate) fn load_retained(
    graph: &dyn GraphImport,
    target: &StoreTarget,
    checkpoint_lsn: u64,
) -> Result<()> {
    let mut nodes: BTreeMap<NodeId, (String, RetainedNodeChain)> = BTreeMap::new();
    let mut rels: BTreeMap<RelId, (String, RetainedRelChain)> = BTreeMap::new();
    for part in FAMILY.parts(target)? {
        if part.hi > checkpoint_lsn {
            continue;
        }
        let bytes = get_object_opt(target, &part.name)?
            .ok_or_else(|| Error::storage(format!("missing retained-chain part {}", part.name)))?;
        let body: PartBody = serde_json::from_slice(&bytes).map_err(|e| {
            Error::storage(format!(
                "failed to parse retained-chain part {}: {e}",
                part.name
            ))
        })?;
        for chain in body.nodes {
            nodes.insert(chain.id, (part.name.clone(), chain));
        }
        for chain in body.rels {
            rels.insert(chain.id, (part.name.clone(), chain));
        }
    }
    for (name, chain) in nodes.into_values() {
        graph.restore_retained_node_chain(chain).map_err(|e| {
            Error::storage(format!("failed to restore retained-chain part {name}: {e}"))
        })?;
    }
    for (name, chain) in rels.into_values() {
        graph.restore_retained_rel_chain(chain).map_err(|e| {
            Error::storage(format!("failed to restore retained-chain part {name}: {e}"))
        })?;
    }
    Ok(())
}
