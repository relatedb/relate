//! The payload's reading half: the open-time install, its node-cut windows,
//! the row decode and the rel-row-group digest verification.

use std::ops::Range;
use std::sync::Arc;

use arrow::array::{BinaryArray, BooleanArray, Int64Array, ListArray, StringArray, UInt32Array};
use bytes::Bytes;
use parquet::arrow::arrow_reader::{
    ArrowReaderMetadata, ArrowReaderOptions, ParquetRecordBatchReaderBuilder,
};
use parquet::arrow::ProjectionMask;
use parquet::file::metadata::{KeyValue, ParquetMetaData};
use relate_core::{Error, NodeId, RelId, Result};
use relate_engine_api::{AdjacencyChunkRef, AdjacencySide, ChunkTombstone, GraphImport};

use super::super::get_parquet_opt;
use super::write::rel_digest_entries;
use super::{column, column_of, frame_header, named, row_crc, PayloadHeader, RowKey};
use crate::codec::parquet::{
    join_or_resume, load_workers, preflight, SpanReaders, DECODE_BATCH_ROWS,
};
use crate::{AdjacencyPayload, Manifest};

/// What installing the manifest's adjacency payload did (ADR-0551).
pub(crate) enum Installed {
    /// The manifest selects no payload.
    Absent,
    /// Every chunk installed; the load tail keeps the installed adjacency.
    Complete,
    /// The payload cannot be installed; the warning names why and the load
    /// re-folds adjacency from the rel rows, so the store opens as it would
    /// without a payload. The payload is a cache tier: every failure is a
    /// degrade, never a refusal.
    Degraded(crate::DegradedPayload),
}

/// Install the manifest's adjacency payload into the loading graph.
pub(crate) fn install(
    target: &crate::StoreTarget,
    graph: &dyn GraphImport,
    manifest: &Manifest,
) -> Installed {
    let Some(payload) = &manifest.adjacency_payload else {
        return Installed::Absent;
    };
    match install_payload(target, graph, manifest, payload) {
        Ok(()) => Installed::Complete,
        Err(error) => Installed::Degraded(crate::DegradedPayload {
            path: payload.path.clone(),
            cause: error.to_string(),
        }),
    }
}

/// The install's own failure: the payload's identity, integrity and rows
/// all have to hold, or the caller degrades to the re-fold.
fn install_payload(
    target: &crate::StoreTarget,
    graph: &dyn GraphImport,
    manifest: &Manifest,
    payload: &AdjacencyPayload,
) -> Result<()> {
    let Some(bytes) = get_parquet_opt(target, &payload.path)? else {
        return Err(Error::storage(
            "the manifest names it but the object is missing",
        ));
    };
    let bytes = Bytes::from(bytes);
    // The ADR-0086 preflight every Parquet reader holds an untrusted object
    // to: the footer walk ahead of Parquet's own metadata reader.
    let builder = preflight(Bytes::clone(&bytes))
        .map_err(|e| Error::storage(format!("unreadable parquet metadata: {e}")))?;
    let metadata = Arc::clone(builder.metadata());
    let key_values = metadata
        .file_metadata()
        .key_value_metadata()
        .map_or(&[][..], Vec::as_slice);
    // The payload's one identity check: its own metadata names the
    // checkpoint the manifest names.
    let header = PayloadHeader::parse(key_values)?;
    if header.checkpoint_lsn != manifest.checkpoint_lsn {
        return Err(crate::manifest::payload_checkpoint_stale(
            header.checkpoint_lsn,
            manifest.checkpoint_lsn,
        ));
    }
    let windows = node_windows(&bytes, &metadata, load_workers())?;
    if windows.is_empty() {
        // A payload with no rows holds no adjacency: fine for a graph
        // without relationships, a payload built against other rows when
        // there are some.
        if graph.live_rel_count(graph.commit_ts()) == 0 {
            return Ok(());
        }
        return Err(Error::storage(
            "it holds no rows, but the checkpoint holds relationships",
        ));
    }
    let readers = SpanReaders::open_groups(Bytes::clone(&bytes), &metadata, windows)?;
    std::thread::scope(|scope| {
        // The rel-row-group digests verify on their own worker while the
        // windows decode: both read the store, and either failing degrades.
        let verify = scope.spawn(|| verify_rel_digests(target, manifest, key_values));
        let readers = &readers;
        let handles: Vec<_> = (0..readers.span_count())
            .map(|span| scope.spawn(move || install_window(graph, readers, span)))
            .collect();
        let mut outcome = Ok(());
        for handle in handles {
            let result = join_or_resume(handle);
            if outcome.is_ok() {
                outcome = result;
            }
        }
        outcome?;
        join_or_resume(verify)
    })
}

/// The payload's install windows: row-group ranges cut at node-id
/// boundaries, so a node's chunks never straddle two workers and install in
/// payload order. The rows are node-ordered, so a row group starts a new
/// node exactly when its first node id differs from the previous group's
/// last; only the node-id column is read to find the boundaries.
fn node_windows(
    file: &Bytes,
    metadata: &Arc<ParquetMetaData>,
    spans: usize,
) -> Result<Vec<Range<usize>>> {
    let groups = metadata.num_row_groups();
    if groups == 0 {
        return Ok(Vec::new());
    }
    let starts = node_starts(file, metadata)?;
    let boundaries: Vec<usize> = (1..groups).filter(|&group| starts[group]).collect();
    let span_count = spans.max(1).min(boundaries.len() + 1);
    let cuts: Vec<usize> = (1..span_count)
        .map(|span| boundaries[span * boundaries.len() / span_count])
        .collect();
    let mut windows = Vec::with_capacity(cuts.len() + 1);
    let mut from = 0;
    for cut in cuts.into_iter().chain(std::iter::once(groups)) {
        windows.push(from..cut);
        from = cut;
    }
    Ok(windows)
}

/// Per row group, whether it starts a new node id.
fn node_starts(file: &Bytes, metadata: &Arc<ParquetMetaData>) -> Result<Vec<bool>> {
    let groups = metadata.num_row_groups();
    let mut group_start = Vec::with_capacity(groups);
    let mut row = 0usize;
    for group in 0..groups {
        group_start.push(row);
        row += metadata.row_group(group).num_rows() as usize;
    }
    let reader = ParquetRecordBatchReaderBuilder::new_with_metadata(
        Bytes::clone(file),
        ArrowReaderMetadata::try_new(Arc::clone(metadata), ArrowReaderOptions::default())
            .map_err(|e| Error::storage(e.to_string()))?,
    )
    .with_projection(ProjectionMask::roots(
        metadata.file_metadata().schema_descr(),
        [0],
    ))
    .with_batch_size(DECODE_BATCH_ROWS)
    .build()
    .map_err(|e| Error::storage(format!("unreadable payload node ids: {e}")))?;
    let mut starts = vec![false; groups];
    let mut next = 0usize;
    let mut previous: Option<NodeId> = None;
    let mut row = 0usize;
    for batch in reader {
        let batch =
            batch.map_err(|e| Error::storage(format!("unreadable payload node ids: {e}")))?;
        let ids = column::<Int64Array>(&batch, "node_id")?;
        for i in 0..batch.num_rows() {
            let id = ids.value(i);
            while next < groups && row == group_start[next] {
                starts[next] = next > 0 && previous != Some(id);
                next += 1;
            }
            previous = Some(id);
            row += 1;
        }
    }
    Ok(starts)
}

/// The rel-row-group digest check: every rel part the manifest lists must
/// still hash to the digests the payload recorded for it. A part whose
/// bytes moved after publication is a payload built against other rows.
fn verify_rel_digests(
    target: &crate::StoreTarget,
    manifest: &Manifest,
    key_values: &[KeyValue],
) -> Result<()> {
    for (key, expected) in rel_digest_entries(target, &manifest.rel_parts)? {
        if named(key_values, &key) != Some(expected.as_str()) {
            return Err(Error::storage(format!(
                "row group {key} of the rel parts hashes to {expected}, the payload names {}",
                named(key_values, &key).unwrap_or("no digest")
            )));
        }
    }
    Ok(())
}

/// Decode one window's rows and install them. The delta hop ids and the
/// tombstones land in one scratch vector per window and each chunk borrows
/// its own slice of them, so a chunk costs no intermediate allocation; the
/// sealed stream is borrowed straight from the decoded row.
fn install_window(graph: &dyn GraphImport, readers: &SpanReaders, window: usize) -> Result<()> {
    let mut delta: Vec<RelId> = Vec::new();
    let mut tombstones: Vec<ChunkTombstone> = Vec::new();
    for batch in readers.reader(window)? {
        let batch = batch.map_err(|e| Error::storage(format!("unreadable payload rows: {e}")))?;
        let node = column::<Int64Array>(&batch, "node_id")?;
        let types = column::<StringArray>(&batch, "type_name")?;
        let incoming = column::<BooleanArray>(&batch, "incoming")?;
        let hops = column::<Int64Array>(&batch, "hops")?;
        let stream = column::<BinaryArray>(&batch, "stream_bytes")?;
        // The tombstone ids and their kinds stay in the lists' own child
        // arrays; each row copies its range into the window's scratch.
        let ids = column::<ListArray>(&batch, "tombstones")?;
        let kinds = column::<ListArray>(&batch, "tombstone_deleted")?;
        let crc32 = column::<UInt32Array>(&batch, "crc32")?;
        let id_values = column_of::<Int64Array>(ids.values().as_ref(), "tombstones")?;
        let kind_values = column_of::<BooleanArray>(kinds.values().as_ref(), "tombstone_deleted")?;
        let offsets = ids.value_offsets();
        if kinds.value_offsets() != offsets {
            return Err(Error::storage(
                "payload tombstone columns disagree in length",
            ));
        }
        delta.clear();
        tombstones.clear();
        let mut rows: Vec<Row<'_>> = Vec::with_capacity(batch.num_rows());
        for i in 0..batch.num_rows() {
            let key = RowKey {
                node_id: node.value(i),
                type_name: types.value(i),
                side: AdjacencySide::from_is_in(incoming.value(i)),
            };
            let from = tombstones.len();
            for j in offsets[i] as usize..offsets[i + 1] as usize {
                tombstones.push(ChunkTombstone {
                    id: id_values.value(j),
                    deleted: kind_values.value(j),
                });
            }
            let total_hops = u64::try_from(hops.value(i))
                .map_err(|_| Error::storage("payload hop count is negative"))?;
            let digest = row_crc(key, total_hops, stream.value(i), &tombstones[from..]);
            if digest != crc32.value(i) {
                return Err(Error::storage(format!(
                    "payload row for node {} hashes to {digest:08x}, its crc32 column names {:08x}",
                    key.node_id,
                    crc32.value(i)
                )));
            }
            rows.push(Row::decode(
                key,
                total_hops,
                stream.value(i),
                from..tombstones.len(),
                &mut delta,
            )?);
        }
        let chunks: Vec<AdjacencyChunkRef<'_>> = rows
            .iter()
            .map(|row| AdjacencyChunkRef {
                node_id: row.key.node_id,
                type_name: row.key.type_name,
                side: row.key.side,
                sealed_hops: row.sealed_hops,
                sealed: row.sealed,
                delta: &delta[row.delta.clone()],
                tombstones: &tombstones[row.tombstones.clone()],
            })
            .collect();
        graph.install_adjacency_chunks(&chunks)?;
    }
    Ok(())
}

/// One decoded payload row: its identity columns and its hop data, borrowed
/// from the decoded batch and the window's scratch buffers.
struct Row<'a> {
    key: RowKey<'a>,
    sealed_hops: u64,
    sealed: &'a [u8],
    /// The row's delta hop ids' range in the window's scratch buffer.
    delta: Range<usize>,
    /// The row's tombstones' range in the window's scratch buffer.
    tombstones: Range<usize>,
}

impl<'a> Row<'a> {
    /// One payload row's frame split: the sealed count that leads
    /// `stream_bytes` and the delta hop ids after the sealed stream, with
    /// the checked hop-id arithmetic. The row's crc verified before this
    /// ran, and the install holds the sealed stream to the codec's own
    /// length for its hop count.
    fn decode(
        key: RowKey<'a>,
        total_hops: u64,
        framed: &'a [u8],
        tombstones: Range<usize>,
        delta: &mut Vec<RelId>,
    ) -> Result<Self> {
        let (sealed_hops, rest) = frame_header(framed)?;
        let delta_hops = total_hops
            .checked_sub(sealed_hops)
            .ok_or_else(|| Error::storage("payload sealed hops exceed the chunk's hops"))?;
        let delta_bytes = usize::try_from(delta_hops)
            .ok()
            .and_then(|n| n.checked_mul(8))
            .filter(|bytes| *bytes <= rest.len())
            .ok_or_else(|| Error::storage("payload hop list overflows its row"))?;
        let split = rest.len() - delta_bytes;
        let from = delta.len();
        for hop in rest[split..].chunks_exact(8) {
            delta.push(i64::from_le_bytes(hop.try_into().expect("eight hop bytes")));
        }
        Ok(Self {
            key,
            sealed_hops,
            sealed: &rest[..split],
            delta: from..delta.len(),
            tombstones,
        })
    }
}
