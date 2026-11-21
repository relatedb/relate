//! The opt-in adjacency checkpoint payload (ADR-0551): one Parquet object a
//! checkpoint publishes beside its node/relationship parts, holding every
//! adjacency chunk of the checkpointed graph. A sealed chunk carries its
//! ADR-0551 codec stream verbatim; an unsealed chunk carries its hop list.
//! The file's metadata names the digest algorithm and, per row group of the
//! rel parts this checkpoint wrote, that row group's digest.
//!
//! Default off, Parquet-only, and gated on the checkpoint's rel count:
//!
//! - `RELATE_ADJACENCY_PAYLOAD=1` turns it on (`0`/unset off, anything else —
//!   an empty value included — fails the checkpoint loudly).
//! - `RELATE_ADJACENCY_PAYLOAD_MIN_RELS` (default 10,000,000) is the rel
//!   count at or above which a payload is written; below it none is.
//!
//! Payload schema (`adjacency/payload-<128-bit-hex>.parquet`, one row per
//! adjacency chunk, rows ordered by `node_id` and the writer's 1024-row
//! groups follow that order):
//!
//! | column | type | meaning |
//! | --- | --- | --- |
//! | `node_id` | Int64 | the chunk's node |
//! | `type_id` | UInt32 | the chunk's interned rel type |
//! | `incoming` | Boolean | `true` for the node's incoming (end) side |
//! | `hops` | Int64 | the chunk's total hop count (sealed + delta) |
//! | `stream_bytes` | Binary | the framed hop data below |
//! | `tombstones` | List(Int64) | the hop ids a tombstone hides |
//! | `crc32` | UInt32 | [`row_crc`] over `stream_bytes` and the tombstone ids |
//!
//! There is no timestamp column: the payload installs only for the
//! manifest checkpoint it names, so a per-row watermark would repeat the
//! checkpoint identity the install already checks.
//!
//! `stream_bytes` frame: a little-endian `u32` sealed hop count, then the
//! sealed ADR-0551 codec stream verbatim when that count is non-zero, then
//! the delta hop ids, one little-endian `i64` each, in stored ts-DESC
//! order. A chunk with neither tier (a tombstone-only region) carries the
//! zero count alone.
//!
//! `tombstones` names the ids the sealed stream still holds but a read at
//! the watermark hides (ADR-0551): a delete or reposition after a fold
//! never rewrites the stream, so the install needs the tombstone beside
//! the verbatim bytes to answer like a re-fold.
//!
//! At open the payload is a cache tier: [`install`] decodes its rows over
//! the checkpoint load's own span workers, cut at node-id boundaries, and
//! installs each chunk borrowed — the sealed stream straight into the
//! arena, the delta hop ids straight into their chunk. Every row's
//! [`row_crc`] verifies before its chunk lands, so a bit-flipped hop id
//! inside `stream_bytes` degrades instead of installing a wrong hop.
//! Every failure (a payload naming another checkpoint, a corrupt or
//! missing object, a rel row group whose digest moved, a row that does
//! not hash to its `crc32`) degrades to the full row re-fold with a
//! warning; it never refuses the store.

use std::ops::Range;
use std::sync::Arc;

use arrow::array::{
    Array, ArrayRef, BinaryArray, BinaryBuilder, BooleanArray, BooleanBuilder, Int64Array,
    Int64Builder, ListArray, ListBuilder, UInt32Array, UInt32Builder,
};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use bytes::Bytes;
use parquet::arrow::arrow_reader::{
    ArrowReaderMetadata, ArrowReaderOptions, ParquetRecordBatchReaderBuilder,
};
use parquet::arrow::ArrowWriter;
use parquet::arrow::ProjectionMask;
use parquet::basic::Compression;
use parquet::file::metadata::{KeyValue, ParquetMetaData, ParquetMetaDataReader};
use parquet::file::properties::{EnabledStatistics, WriterProperties};
use parquet::file::reader::ChunkReader;
use relate_core::{Error, NodeId, RelId, Result};
use relate_engine_api::{AdjacencyChunk, AdjacencyChunkRef, GraphImport};

use super::{prepare_object_dir, reserve_exclusive, ShardWrite};
use crate::codec::parquet::{
    join_or_resume, load_workers, shard_envelope_refusal, SpanReaders, VfsChunkReader,
    DECODE_BATCH_ROWS, MAX_FILE, ROW_GROUP_CAP,
};
use crate::snapshot::{ADJACENCY_PAYLOAD_ENV, ADJACENCY_PAYLOAD_MIN_RELS_ENV};
use crate::vfs::{CappedVfsWriter, VfsFile};
use crate::{AdjacencyPayload, Manifest, ShardPart};

/// The threshold's default: a payload is written at 10M relationships.
pub(crate) const DEFAULT_MIN_RELS: u64 = 10_000_000;

/// The digest algorithm named in the payload's file metadata: `crc32fast`'s
/// CRC-32, rendered as eight lowercase hex digits per row group.
pub(crate) const DIGEST_ALGORITHM: &str = "crc32";

const PAYLOAD_FORMAT: &str = "2";
const META_FORMAT: &str = "adjacency.payload.format";
pub(crate) const META_ALGORITHM: &str = "adjacency.digest.algorithm";
pub(crate) const META_LSN: &str = "adjacency.checkpoint_lsn";
pub(crate) const META_DIGEST_PREFIX: &str = "adjacency.rel_row_group.";

/// The metadata value naming one rel part's row group: the key and the
/// digest spelling live here once, so the writer and the verifier cannot
/// drift.
pub(crate) fn digest_key(path: &str, ordinal: usize) -> String {
    format!("{META_DIGEST_PREFIX}{path}.{ordinal}")
}

/// One row-group digest's metadata value: eight lowercase hex digits.
pub(crate) fn digest_hex(crc32: u32) -> String {
    format!("{crc32:08x}")
}

/// The first metadata value named `key`.
pub(crate) fn named<'a>(key_values: &'a [KeyValue], key: &str) -> Option<&'a str> {
    key_values
        .iter()
        .find(|kv| kv.key == key)
        .and_then(|kv| kv.value.as_deref())
}

/// The payload file's own metadata, typed: parsing validates the format and
/// the digest algorithm in one place and yields the checkpoint the payload
/// was published at; the install compares that lsn against the manifest.
struct PayloadHeader {
    checkpoint_lsn: u64,
}

impl PayloadHeader {
    fn parse(key_values: &[KeyValue]) -> Result<Self> {
        let format = named(key_values, META_FORMAT);
        if format != Some(PAYLOAD_FORMAT) {
            return Err(Error::storage(format!(
                "its format is {format:?}, not {PAYLOAD_FORMAT}"
            )));
        }
        let algorithm = named(key_values, META_ALGORITHM);
        if algorithm != Some(DIGEST_ALGORITHM) {
            return Err(Error::storage(format!(
                "its digest algorithm is {algorithm:?}, not {DIGEST_ALGORITHM}"
            )));
        }
        let checkpoint_lsn = named(key_values, META_LSN)
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| Error::storage("it names no readable checkpoint lsn"))?;
        Ok(Self { checkpoint_lsn })
    }
}

/// Whether and when a checkpoint writes the adjacency payload: `None` is
/// off, `Some(min_rels)` writes at or above that live-relationship count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PayloadConfig {
    min_rels: Option<u64>,
}

impl PayloadConfig {
    pub(crate) const fn disabled() -> Self {
        Self { min_rels: None }
    }

    pub(crate) const fn enabled(min_rels: u64) -> Self {
        Self {
            min_rels: Some(min_rels),
        }
    }

    /// The environment's configuration, read once per checkpoint. An
    /// unknown opt-in value or a malformed threshold is a loud error, never
    /// a silent default; a disabled payload does not parse the threshold.
    pub(crate) fn from_env() -> Result<Self> {
        let enabled = match std::env::var(ADJACENCY_PAYLOAD_ENV) {
            Err(std::env::VarError::NotPresent) => false,
            Ok(value) if value == "0" => false,
            Ok(value) if value == "1" => true,
            Ok(value) => {
                return Err(Error::storage(format!(
                    "{ADJACENCY_PAYLOAD_ENV} must be 0 or 1, not {value:?}"
                )))
            }
            Err(_) => {
                return Err(Error::storage(format!(
                    "{ADJACENCY_PAYLOAD_ENV} must be valid UTF-8"
                )))
            }
        };
        if !enabled {
            return Ok(Self::disabled());
        }
        let min_rels = match std::env::var(ADJACENCY_PAYLOAD_MIN_RELS_ENV) {
            Err(std::env::VarError::NotPresent) => DEFAULT_MIN_RELS,
            Ok(value) => value.parse::<u64>().map_err(|_| {
                Error::storage(format!(
                    "{ADJACENCY_PAYLOAD_MIN_RELS_ENV} must be a non-negative integer, not {value:?}"
                ))
            })?,
            Err(_) => {
                return Err(Error::storage(format!(
                    "{ADJACENCY_PAYLOAD_MIN_RELS_ENV} must be valid UTF-8"
                )))
            }
        };
        Ok(Self::enabled(min_rels))
    }
}

/// Write this checkpoint's adjacency payload when `config` enables it and
/// the checkpointed graph holds at least its threshold of relationships.
/// The object publishes before the manifest that selects it, under the
/// shared exclusive-create/sync/durable-name rules.
pub(super) fn write(
    ctx: &mut ShardWrite<'_>,
    checkpoint_lsn: u64,
    rel_parts: &[ShardPart],
    config: PayloadConfig,
) -> Result<Option<AdjacencyPayload>> {
    let Some(min_rels) = config.min_rels else {
        return Ok(None);
    };
    if (ctx.graph.live_rel_count(ctx.ts) as u64) < min_rels {
        return Ok(None);
    }
    let metadata = payload_metadata(ctx.target, checkpoint_lsn, rel_parts)?;
    let path = publish(ctx.target, ctx.graph, ctx.ts, metadata, ctx.owned)?;
    Ok(Some(AdjacencyPayload { path }))
}

/// The manifest-side validation of a published payload (run on every
/// manifest read): the path is the payload grammar. The payload's own
/// checkpoint identity is not checked here: the payload is a cache tier,
/// so a payload naming another checkpoint degrades to the row re-fold at
/// [`install`] instead of refusing the store.
pub(super) fn validate(manifest: &Manifest) -> Result<()> {
    let Some(payload) = &manifest.adjacency_payload else {
        return Ok(());
    };
    if !crate::snapshot::is_payload_object_name(&payload.path) {
        return Err(Error::storage(format!(
            "invalid manifest adjacency payload path: {}",
            payload.path
        )));
    }
    Ok(())
}

/// Retire `prev`'s payload when the new manifest no longer selects it: the
/// successor checkpoint published one, or the opt-in turned off.
pub(super) fn retire(prev: Option<&Manifest>, manifest: &Manifest, retired: &mut Vec<String>) {
    if let Some(old) = prev.and_then(|m| m.adjacency_payload.as_ref()) {
        if manifest.adjacency_payload.as_ref() != Some(old) {
            retired.push(old.path.clone());
        }
    }
}

/// Whether `manifest` selects the payload object `path`.
pub(super) fn is_referenced(manifest: &Manifest, path: &str) -> bool {
    manifest
        .adjacency_payload
        .as_ref()
        .is_some_and(|payload| payload.path == path)
}

/// What installing the manifest's adjacency payload did (ADR-0551).
pub(super) enum Installed {
    /// The manifest selects no payload.
    Absent,
    /// Every chunk installed; the load tail keeps the installed adjacency.
    Complete,
    /// The payload cannot be installed; the warning names why and the load
    /// re-folds adjacency from the rel rows, so the store opens as it would
    /// without a payload. The payload is a cache tier: every failure is a
    /// degrade, never a refusal.
    Degraded(String),
}

/// Install the manifest's adjacency payload into the loading graph.
pub(super) fn install(
    target: &crate::StoreTarget,
    graph: &dyn GraphImport,
    manifest: &Manifest,
) -> Installed {
    let Some(payload) = &manifest.adjacency_payload else {
        return Installed::Absent;
    };
    match install_payload(target, graph, manifest, payload) {
        Ok(()) => Installed::Complete,
        Err(error) => Installed::Degraded(format!(
            "ignoring adjacency payload {}: {error}; re-folding adjacency from the rel rows",
            payload.path
        )),
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
    let Some(bytes) = super::get_parquet_opt(target, &payload.path)? else {
        return Err(Error::storage(
            "the manifest names it but the object is missing",
        ));
    };
    let bytes = Bytes::from(bytes);
    let metadata = Arc::new(
        ParquetMetaDataReader::new()
            .parse_and_finish(&bytes)
            .map_err(|e| Error::storage(format!("unreadable parquet metadata: {e}")))?,
    );
    let key_values = metadata
        .file_metadata()
        .key_value_metadata()
        .map_or(&[][..], Vec::as_slice);
    // The payload's one identity check: its own metadata names the
    // checkpoint the manifest names.
    let header = PayloadHeader::parse(key_values)?;
    if header.checkpoint_lsn != manifest.checkpoint_lsn {
        return Err(crate::manifest::checkpoint_identity_mismatch(
            "adjacency payload",
            manifest.checkpoint_lsn,
            "payload",
            header.checkpoint_lsn,
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
    for part in &manifest.rel_parts {
        // The writer's own chunk reader: digests stream from the part, never
        // through a whole-part heap copy.
        let file = VfsChunkReader::open(Arc::clone(&target.vfs), &part.path, MAX_FILE)
            .map_err(|e| crate::storage_context(e, format_args!("rel part {}", part.path)))?;
        for digest in row_group_digests(&file)? {
            let key = digest_key(&part.path, digest.ordinal);
            let expected = digest_hex(digest.crc32);
            if named(key_values, &key) != Some(expected.as_str()) {
                return Err(Error::storage(format!(
                    "row group {key} of the rel parts hashes to {expected}, the payload names {}",
                    named(key_values, &key).unwrap_or("no digest")
                )));
            }
        }
    }
    Ok(())
}

/// Decode one window's rows and install them. The delta hop ids land in one
/// scratch vector per batch and each chunk borrows its own slice of it, so
/// a chunk costs no intermediate allocation; the sealed stream is borrowed
/// straight from the decoded row, and the tombstone ids borrow the list
/// column's child array.
fn install_window(graph: &dyn GraphImport, readers: &SpanReaders, window: usize) -> Result<()> {
    for batch in readers.reader(window)? {
        let batch = batch.map_err(|e| Error::storage(format!("unreadable payload rows: {e}")))?;
        let node = column::<Int64Array>(&batch, "node_id")?;
        let types = column::<UInt32Array>(&batch, "type_id")?;
        let incoming = column::<BooleanArray>(&batch, "incoming")?;
        let hops = column::<Int64Array>(&batch, "hops")?;
        let stream = column::<BinaryArray>(&batch, "stream_bytes")?;
        // The tombstone ids stay in the list's own child array: each row
        // borrows its range of it, no per-row list copy.
        let tombstones = column::<ListArray>(&batch, "tombstones")?;
        let crc32 = column::<UInt32Array>(&batch, "crc32")?;
        let tombstone_values = column_of::<Int64Array>(tombstones.values().as_ref(), "tombstones")?;
        let tombstone_offsets = tombstones.value_offsets();
        let mut delta: Vec<RelId> = Vec::new();
        let mut rows: Vec<Row<'_>> = Vec::with_capacity(batch.num_rows());
        for i in 0..batch.num_rows() {
            let hidden = &tombstone_values.values()
                [tombstone_offsets[i] as usize..tombstone_offsets[i + 1] as usize];
            rows.push(Row::decode(
                RowKey {
                    node_id: node.value(i),
                    type_id: types.value(i),
                    incoming: incoming.value(i),
                },
                hops.value(i),
                stream.value(i),
                hidden,
                crc32.value(i),
                &mut delta,
            )?);
        }
        let chunks: Vec<AdjacencyChunkRef<'_>> = rows
            .iter()
            .map(|row| AdjacencyChunkRef {
                node_id: row.node_id,
                type_id: row.type_id,
                incoming: row.incoming,
                sealed_hops: row.sealed_hops,
                sealed: row.sealed,
                delta: &delta[row.delta.clone()],
                tombstones: row.tombstones,
            })
            .collect();
        graph.install_adjacency_chunks(&chunks)?;
    }
    Ok(())
}

/// One decoded payload row's scalar columns; its delta hop ids stay in the
/// window's scratch vector, addressed by the row's range, its sealed stream
/// borrows the decoded row, and its tombstone ids the list's child array.
struct Row<'a> {
    node_id: NodeId,
    type_id: u32,
    incoming: bool,
    sealed_hops: u64,
    sealed: &'a [u8],
    tombstones: &'a [RelId],
    /// The row's delta hop ids' range in the window's scratch buffer.
    delta: Range<usize>,
}

/// One payload row's identity and direction columns — named, because the
/// integers and the flag swap silently as positional arguments.
#[derive(Clone, Copy)]
struct RowKey {
    node_id: NodeId,
    type_id: u32,
    incoming: bool,
}

impl<'a> Row<'a> {
    /// One payload row's decode: the crc32 verification, the frame split
    /// and the checked hop-id arithmetic, in the order the install needs
    /// them. The row verifies before any of its hop data is used: a
    /// bit-flipped hop id would otherwise install as a real hop.
    fn decode(
        RowKey {
            node_id,
            type_id,
            incoming,
        }: RowKey,
        total_hops: i64,
        framed: &'a [u8],
        tombstones: &'a [RelId],
        expected_crc: u32,
        delta: &mut Vec<RelId>,
    ) -> Result<Self> {
        let digest = row_crc(framed, tombstones);
        if digest != expected_crc {
            return Err(Error::storage(format!(
                "payload row for node {node_id} hashes to {digest:08x}, its crc32 column names {expected_crc:08x}"
            )));
        }
        let (sealed_hops, rest) = frame_header(framed)?;
        let total = u64::try_from(total_hops)
            .map_err(|_| Error::storage("payload hop count is negative"))?;
        let delta_hops = total
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
            node_id,
            type_id,
            incoming,
            sealed_hops,
            sealed: &rest[..split],
            tombstones,
            delta: from..delta.len(),
        })
    }
}

/// The one decode of a batch's column `what`, typed as the payload schema
/// holds it — a payload of another shape is the install's degrade. Columns
/// are addressed by name, so the order lives in [`payload_schema`] alone.
pub(crate) fn column<'a, T: Array + 'static>(batch: &'a RecordBatch, what: &str) -> Result<&'a T> {
    let array = batch
        .column_by_name(what)
        .ok_or_else(|| Error::storage(format!("payload {what} column")))?;
    column_of(array, what)
}

/// [`column`]'s array-level half: the typed view of one decoded array,
/// shared with the list columns' child arrays.
pub(crate) fn column_of<'a, T: Array + 'static>(array: &'a dyn Array, what: &str) -> Result<&'a T> {
    array
        .as_any()
        .downcast_ref::<T>()
        .ok_or_else(|| Error::storage(format!("payload {what} column")))
}

/// One payload row's `stream_bytes` head: the little-endian `u32` sealed hop
/// count and the bytes after it.
pub(crate) fn frame_header(stream: &[u8]) -> Result<(u64, &[u8])> {
    let Some((count, rest)) = stream.split_at_checked(4) else {
        return Err(Error::storage(
            "payload stream_bytes shorter than its frame",
        ));
    };
    Ok((
        u64::from(u32::from_le_bytes(
            count.try_into().expect("four frame bytes"),
        )),
        rest,
    ))
}

/// The payload's file metadata: the format, the digest algorithm, the
/// checkpoint LSN, and one digest per row group of every rel part.
fn payload_metadata(
    target: &crate::StoreTarget,
    checkpoint_lsn: u64,
    rel_parts: &[ShardPart],
) -> Result<Vec<KeyValue>> {
    let mut metadata = vec![
        KeyValue::new(META_FORMAT.to_owned(), PAYLOAD_FORMAT.to_owned()),
        KeyValue::new(META_ALGORITHM.to_owned(), DIGEST_ALGORITHM.to_owned()),
        KeyValue::new(META_LSN.to_owned(), checkpoint_lsn.to_string()),
    ];
    for part in rel_parts {
        let file = VfsChunkReader::open(Arc::clone(&target.vfs), &part.path, MAX_FILE)
            .map_err(|e| crate::storage_context(e, format_args!("rel part {}", part.path)))?;
        for digest in row_group_digests(&file)? {
            metadata.push(KeyValue::new(
                digest_key(&part.path, digest.ordinal),
                digest_hex(digest.crc32),
            ));
        }
    }
    Ok(metadata)
}

/// One row group's digest and the byte span it covers in its part file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RowGroupDigest {
    pub(crate) ordinal: usize,
    pub(crate) crc32: u32,
    pub(crate) start: u64,
    pub(crate) len: u64,
}

/// The CRC-32 of every row group of a part file, over that row group's
/// column-chunk byte span. Row groups are byte-disjoint, so an edit inside
/// one flips exactly that group's digest.
pub(crate) fn row_group_digests<T: ChunkReader>(file: &T) -> Result<Vec<RowGroupDigest>> {
    let metadata = ParquetMetaDataReader::new()
        .parse_and_finish(file)
        .map_err(|e| Error::storage(format!("payload digest metadata: {e}")))?;
    let mut digests = Vec::with_capacity(metadata.num_row_groups());
    for (ordinal, group) in metadata.row_groups().iter().enumerate() {
        let (mut start, mut end) = (u64::MAX, 0u64);
        for column in group.columns() {
            let (offset, len) = column.byte_range();
            start = start.min(offset);
            end = end.max(offset + len);
        }
        let Some(len) = end.checked_sub(start).filter(|len| *len > 0) else {
            continue;
        };
        let bytes = file.get_bytes(start, len as usize).map_err(|e| {
            Error::storage(format!(
                "payload digest read at {start} of {len} bytes: {e}"
            ))
        })?;
        digests.push(RowGroupDigest {
            ordinal,
            crc32: crc32fast::hash(&bytes),
            start,
            len,
        });
    }
    Ok(digests)
}

/// Publish the payload object and return its durable name.
fn publish(
    target: &crate::StoreTarget,
    graph: &dyn GraphImport,
    ts: u64,
    metadata: Vec<KeyValue>,
    owned: &mut Vec<String>,
) -> Result<String> {
    let (path, mut file) = reserve_exclusive(
        &*target.vfs,
        || {
            let path = crate::snapshot::payload_object_name()?;
            prepare_object_dir(target, crate::snapshot::PAYLOAD_DIR, "adjacency directory")?;
            Ok(path)
        },
        owned,
        "adjacency payload",
    )?;
    let result = write_parquet(&mut *file, graph, ts, metadata)
        .and_then(|()| file.sync_all())
        .and_then(|()| target.vfs.sync_dir(crate::snapshot::PAYLOAD_DIR));
    drop(file);
    result?;
    Ok(path)
}

/// The payload's Parquet schema.
pub(crate) fn payload_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("node_id", DataType::Int64, false),
        Field::new("type_id", DataType::UInt32, false),
        Field::new("incoming", DataType::Boolean, false),
        Field::new("hops", DataType::Int64, false),
        Field::new("stream_bytes", DataType::Binary, false),
        Field::new(
            "tombstones",
            DataType::List(Arc::new(Field::new("item", DataType::Int64, false))),
            false,
        ),
        Field::new("crc32", DataType::UInt32, false),
    ]))
}

/// One payload row's integrity digest: the CRC-32 of its framed
/// `stream_bytes` followed by its tombstone ids, each little-endian. The
/// install recomputes it before the row's chunk lands, so a bit flip in
/// either half degrades to the re-fold instead of installing wrong data.
pub(crate) fn row_crc(stream: &[u8], tombstones: &[i64]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(stream);
    for id in tombstones {
        hasher.update(&id.to_le_bytes());
    }
    hasher.finalize()
}

/// The one `stream_bytes` frame: sealed hop count, sealed stream verbatim,
/// delta hop ids. The write side refuses the shapes the read side refuses.
fn frame(chunk: &AdjacencyChunk) -> Result<Vec<u8>> {
    let sealed_hops = u32::try_from(chunk.sealed_hops).map_err(|_| {
        Error::storage(format!(
            "adjacency chunk (node {}, type {}) seals {} hops, over the frame's u32",
            chunk.node_id, chunk.type_id, chunk.sealed_hops
        ))
    })?;
    if chunk.sealed.is_empty() != (sealed_hops == 0) {
        return Err(Error::storage(format!(
            "adjacency chunk (node {}, type {}) carries {} sealed hops and {} stream bytes",
            chunk.node_id,
            chunk.type_id,
            sealed_hops,
            chunk.sealed.len()
        )));
    }
    let mut framed = Vec::with_capacity(4 + chunk.sealed.len() + 8 * chunk.delta.len());
    framed.extend_from_slice(&sealed_hops.to_le_bytes());
    framed.extend_from_slice(&chunk.sealed);
    for hop in &chunk.delta {
        framed.extend_from_slice(&hop.to_le_bytes());
    }
    Ok(framed)
}

/// The open payload file's writer: rows stream in 1024-chunk batches, one
/// written row group each.
struct PayloadRows {
    schema: Arc<Schema>,
    node_id: Vec<i64>,
    type_id: Vec<u32>,
    incoming: Vec<bool>,
    hops: Vec<i64>,
    stream: Vec<Vec<u8>>,
    tombstones: Vec<Vec<i64>>,
    crc32: Vec<u32>,
}

impl PayloadRows {
    fn push(&mut self, chunk: AdjacencyChunk) -> Result<()> {
        self.node_id.push(chunk.node_id);
        self.type_id.push(chunk.type_id);
        self.incoming.push(chunk.incoming);
        self.hops
            .push(chunk.sealed_hops as i64 + chunk.delta.len() as i64);
        let framed = frame(&chunk)?;
        self.crc32.push(row_crc(&framed, &chunk.tombstones));
        self.stream.push(framed);
        self.tombstones.push(chunk.tombstones);
        Ok(())
    }

    fn len(&self) -> usize {
        self.node_id.len()
    }

    fn flush(&mut self, writer: &mut ArrowWriter<&mut CappedVfsWriter<'_>>) -> Result<()> {
        if self.node_id.is_empty() {
            return Ok(());
        }
        let mut node_id = Int64Builder::with_capacity(self.len());
        node_id.append_slice(&self.node_id);
        let mut type_id = UInt32Builder::with_capacity(self.len());
        type_id.append_slice(&self.type_id);
        let mut incoming = BooleanBuilder::with_capacity(self.len());
        incoming.append_slice(&self.incoming);
        let mut hops = Int64Builder::with_capacity(self.len());
        hops.append_slice(&self.hops);
        let mut stream = BinaryBuilder::with_capacity(self.len(), 0);
        for value in &self.stream {
            stream.append_value(value);
        }
        let mut tombstones = ListBuilder::new(Int64Builder::new()).with_field(Field::new(
            "item",
            DataType::Int64,
            false,
        ));
        for ids in &self.tombstones {
            tombstones.values().append_slice(ids);
            tombstones.append(true);
        }
        let mut crc32 = UInt32Builder::with_capacity(self.len());
        crc32.append_slice(&self.crc32);
        let columns: Vec<ArrayRef> = vec![
            Arc::new(node_id.finish()),
            Arc::new(type_id.finish()),
            Arc::new(incoming.finish()),
            Arc::new(hops.finish()),
            Arc::new(stream.finish()),
            Arc::new(tombstones.finish()),
            Arc::new(crc32.finish()),
        ];
        let batch = RecordBatch::try_new(Arc::clone(&self.schema), columns)
            .map_err(|e| Error::storage(e.to_string()))?;
        writer
            .write(&batch)
            .map_err(|e| Error::storage(e.to_string()))?;
        self.node_id.clear();
        self.type_id.clear();
        self.incoming.clear();
        self.hops.clear();
        self.stream.clear();
        self.tombstones.clear();
        self.crc32.clear();
        Ok(())
    }
}

/// Stream every adjacency chunk into the open payload file.
fn write_parquet(
    file: &mut dyn VfsFile,
    graph: &dyn GraphImport,
    ts: u64,
    metadata: Vec<KeyValue>,
) -> Result<()> {
    let properties = payload_writer_properties(metadata);
    let mut sink = CappedVfsWriter::new(file, MAX_FILE, shard_envelope_refusal());
    let schema = payload_schema();
    let mut writer = ArrowWriter::try_new(&mut sink, Arc::clone(&schema), Some(properties))
        .map_err(|e| Error::storage(e.to_string()))?;
    let mut rows = PayloadRows {
        schema,
        node_id: Vec::with_capacity(ROW_GROUP_CAP),
        type_id: Vec::with_capacity(ROW_GROUP_CAP),
        incoming: Vec::with_capacity(ROW_GROUP_CAP),
        hops: Vec::with_capacity(ROW_GROUP_CAP),
        stream: Vec::with_capacity(ROW_GROUP_CAP),
        tombstones: Vec::with_capacity(ROW_GROUP_CAP),
        crc32: Vec::with_capacity(ROW_GROUP_CAP),
    };
    graph.for_each_adjacency_chunk(ts, &mut |chunk| {
        rows.push(chunk)?;
        if rows.len() == ROW_GROUP_CAP {
            rows.flush(&mut writer)?;
        }
        Ok(())
    })?;
    rows.flush(&mut writer)?;
    writer
        .close()
        .map(|_| ())
        .map_err(|e| Error::storage(e.to_string()))
}

/// The payload file's writer properties — the writer's and the tests'
/// tamper rebuilds', so an untouched rewrite stays byte-comparable.
pub(crate) fn payload_writer_properties(metadata: Vec<KeyValue>) -> WriterProperties {
    WriterProperties::builder()
        .set_compression(Compression::SNAPPY)
        .set_statistics_enabled(EnabledStatistics::None)
        .set_max_row_group_row_count(Some(ROW_GROUP_CAP))
        .set_max_row_group_bytes(None)
        .set_key_value_metadata(Some(metadata))
        .build()
}

/// Sweep the payload objects the manifest does not select: the crash
/// window between a payload's publish and the manifest that names it.
pub(super) fn sweep(target: &crate::StoreTarget, manifest: &Manifest) {
    super::sweep_dir_orphans(
        target,
        crate::snapshot::PAYLOAD_DIR,
        |path| matches!(super::object_name(path), Some(super::ObjectName::Payload)),
        |path| is_referenced(manifest, path),
    );
}

/// The payload family's deletable-path check: the directory must be a real
/// directory and the object a plain file, never an alias.
pub(super) fn validate_deletable(target: &crate::StoreTarget, name: &str) -> Result<()> {
    if let Some(root) = &target.native_root {
        super::validate_dir(
            root,
            crate::snapshot::PAYLOAD_DIR,
            "adjacency directory",
            super::DirAdmit::Required,
        )
        .map_err(|_| Error::storage(format!("invalid adjacency payload path: {name}")))?;
        super::validate_native_plain_file(root, name, "adjacency payload")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two row groups in one file, each digesting its own byte span: an
    /// edit inside one flips exactly that group's digest.
    #[test]
    fn a_one_byte_edit_flips_only_its_row_group_digest() {
        let file = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));
        let mut bytes = Vec::new();
        {
            let properties = WriterProperties::builder()
                .set_max_row_group_row_count(Some(16))
                .build();
            let mut writer = ArrowWriter::try_new(&mut bytes, file, Some(properties)).unwrap();
            for group in 0..2 {
                let mut ids = Int64Builder::with_capacity(16);
                for row in 0..16 {
                    ids.append_value(group * 16 + row);
                }
                let batch = RecordBatch::try_new(
                    Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)])),
                    vec![Arc::new(ids.finish())],
                )
                .unwrap();
                writer.write(&batch).unwrap();
            }
            writer.close().unwrap();
        }
        let digests = row_group_digests(&bytes::Bytes::from(bytes.clone())).unwrap();
        assert_eq!(digests.len(), 2, "{digests:?}");
        assert_ne!(digests[0].crc32, digests[1].crc32);
        // One byte inside the first group's span: only its digest moves.
        let mut edited = bytes;
        edited[digests[0].start as usize + digests[0].len as usize / 2] ^= 0x01;
        let after = row_group_digests(&bytes::Bytes::from(edited)).unwrap();
        assert_ne!(after[0].crc32, digests[0].crc32);
        assert_eq!(after[1].crc32, digests[1].crc32);
    }
}
