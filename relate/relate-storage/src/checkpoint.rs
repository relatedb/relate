//! Checkpoint publication: the entry points that freeze a cut's image and
//! dump it through the codec, plus the WAL/snapshot metadata the write path
//! and the open read back. Keeps publication out of the crate's largest
//! module (`snapshot`); the sharded writer a cut steers is reached solely
//! through `codec_ops`'s Parquet row.

use relate_core::Result;
use relate_engine_api::CheckpointDirtyMarks;

use crate::codec::cut::{CheckpointCut, Rewrite};
use crate::wal::Wal;
use crate::{codec, StoreTarget};

pub fn checkpoint(
    target: &StoreTarget,
    graph: &dyn CheckpointDirtyMarks,
    ontology: Option<&str>,
) -> Result<()> {
    checkpoint_at(target, graph, 0, graph.commit_ts(), ontology)
}

pub fn checkpoint_with_lsn(
    target: &StoreTarget,
    graph: &dyn CheckpointDirtyMarks,
    checkpoint_lsn: u64,
    ontology: Option<&str>,
) -> Result<()> {
    checkpoint_at(target, graph, checkpoint_lsn, graph.commit_ts(), ontology)
}

/// Snapshot of rows visible at `snapshot_ts` (ADR-0020 / ADR-0022 / ADR-0050),
/// for a caller that holds no concurrent writer: the checkpoint takes its
/// own image.
///
/// `ontology` is the store's current ontology document (ADR-0120), opaque
/// to storage: the Parquet codec publishes it as an immutable
/// `ontology/<128-bit-hex>.json` object selected by a v4 manifest, the JSON
/// codec inlines it in `graph.json` at `value_encoding` 3; `None` keeps
/// both codecs at the previous format. It is frozen into the cut with the
/// image, so the dump serves the document of its own cut.
pub fn checkpoint_at(
    target: &StoreTarget,
    graph: &dyn CheckpointDirtyMarks,
    checkpoint_lsn: u64,
    snapshot_ts: u64,
    ontology: Option<&str>,
) -> Result<()> {
    let cut = CheckpointCut::take(graph, checkpoint_lsn, snapshot_ts, ontology)?;
    checkpoint_at_image(target, &cut, Rewrite::Coalesce)
}

/// The same dump at a cut the caller already froze (ADR-0643):
/// [`CheckpointCut::take`] took the image — and the ontology document —
/// from the graph under the checkpoint's exclusive pin, and the graph is
/// read at the cut's snapshot timestamp under a snapshot pin. A failed dump
/// hands the image's dirty ids back, so the next checkpoint still covers
/// them.
///
/// `rewrite` picks the shard writer's policy: [`Rewrite::Coalesce`] absorbs
/// clean neighbours into each rewritten unit (ADR-0183 Decision 2), while
/// [`Rewrite::Incremental`] rewrites only the parts a dirty id seeds and
/// leaves every clean part untouched (ADR-0022 Decision 2), so a rotation
/// mid-load re-reads no clean shard. A whole-image codec has one image to
/// rewrite and ignores it.
pub fn checkpoint_at_image(
    target: &StoreTarget,
    cut: &CheckpointCut<'_>,
    rewrite: Rewrite,
) -> Result<()> {
    // The codec vocabulary's one behavioral surface (see `codec::codec_ops`):
    // a codec's writer is one table entry, not an arm here. The failed-dump
    // marks hand-back is the cut's own protocol.
    cut.dump_with_marks(|| (codec::codec_ops(target.codec).dump)(target, cut, rewrite))
}

/// Create an empty WAL positioned at `checkpoint_lsn` (ADR-0156).
///
/// A later open replays nothing and the next commit is `checkpoint_lsn + 1`.
/// The caller has already published a snapshot at that LSN.
pub fn init_empty_wal(target: &StoreTarget, checkpoint_lsn: u64) -> Result<()> {
    let (vfs, prefix) = target.wal_vfs();
    let _wal = Wal::open(vfs, prefix, checkpoint_lsn)?;
    Ok(())
}

pub fn snapshot_data_bytes(target: &StoreTarget) -> Result<u64> {
    // The codec vocabulary's one behavioral surface (see `codec::codec_ops`):
    // a codec's sizer is one table entry, not an arm here.
    (codec::codec_ops(target.codec).snapshot_data_bytes)(target)
}

/// Whether the store has already published a checkpoint image — the
/// codec's own presence probe, one table entry per codec (the classic
/// codecs' root `manifest.json`, the markdown codec's `.relate/`
/// sidecar). The no-op checkpoint skip consults it (ADR-0646): a
/// checkpoint whose WAL holds nothing uncheckpointed over a published
/// snapshot has nothing to write.
pub fn has_published_snapshot(target: &StoreTarget) -> Result<bool> {
    // The codec vocabulary's one behavioral surface (see `codec::codec_ops`):
    // the published-image probe is one table entry, not an arm here.
    (codec::codec_ops(target.codec).has_published_snapshot)(target)
}

/// Published checkpoint LSN, or 0 when the store has no manifest yet.
///
/// Reads the codec envelope (`codec`, `checkpoint_lsn`, `commit_ts`). A
/// Parquet manifest carries extra fields and a JSON manifest is only the
/// envelope, so both builds report the same LSN.
pub fn published_checkpoint_lsn(target: &StoreTarget) -> Result<u64> {
    Ok(codec::read_envelope(target)?
        .map(|env| env.checkpoint_lsn)
        .unwrap_or(0))
}
