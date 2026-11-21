//! The payload's writing half: the opt-in configuration, the file metadata
//! and its rel row-group digests, the row columns, the Parquet writer, the
//! object publication, and the manifest-side lifecycle (validation, the
//! superseded-payload retirement, the orphan sweep).

use std::sync::Arc;

use arrow::array::{
    ArrayRef, BinaryBuilder, BooleanBuilder, Int64Builder, ListBuilder, StringBuilder,
    UInt32Builder,
};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::metadata::{KeyValue, ParquetMetaDataReader};
use parquet::file::properties::{EnabledStatistics, WriterProperties};
use parquet::file::reader::ChunkReader;
use relate_core::{Error, Result};
use relate_engine_api::{AdjacencyChunk, ChunkTombstone};

use super::super::{prepare_object_dir, reserve_exclusive, ShardWrite};
use super::{
    digest_hex, digest_key, row_crc, RowKey, DIGEST_ALGORITHM, META_ALGORITHM, META_FORMAT,
    META_LSN, PAYLOAD_FORMAT,
};
use crate::codec::parquet::{shard_envelope_refusal, VfsChunkReader, MAX_FILE, ROW_GROUP_CAP};
use crate::snapshot::{ADJACENCY_PAYLOAD_ENV, ADJACENCY_PAYLOAD_MIN_RELS_ENV};
use crate::vfs::{CappedVfsWriter, VfsFile};
use crate::{AdjacencyPayload, Manifest, ShardPart};

/// The threshold's default: a payload is written at 10M relationships.
pub(crate) const DEFAULT_MIN_RELS: u64 = 10_000_000;

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
pub(crate) fn write(
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
/// [`super::install`] instead of refusing the store.
pub(crate) fn validate(manifest: &Manifest) -> Result<()> {
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
pub(crate) fn retire(prev: Option<&Manifest>, manifest: &Manifest, retired: &mut Vec<String>) {
    if let Some(old) = prev.and_then(|m| m.adjacency_payload.as_ref()) {
        if manifest.adjacency_payload.as_ref() != Some(old) {
            retired.push(old.path.clone());
        }
    }
}

/// Whether `manifest` selects the payload object `path`.
pub(crate) fn is_referenced(manifest: &Manifest, path: &str) -> bool {
    manifest
        .adjacency_payload
        .as_ref()
        .is_some_and(|payload| payload.path == path)
}

/// Sweep the payload objects the manifest does not select: the crash
/// window between a payload's publish and the manifest that names it.
pub(crate) fn sweep(target: &crate::StoreTarget, manifest: &Manifest) {
    super::super::sweep_dir_orphans(
        target,
        crate::snapshot::PAYLOAD_DIR,
        |path| {
            matches!(
                super::super::object_name(path),
                Some(super::super::ObjectName::Payload)
            )
        },
        |path| is_referenced(manifest, path),
    );
}

/// The payload family's deletable-path check: the directory must be a real
/// directory and the object a plain file, never an alias.
pub(crate) fn validate_deletable(target: &crate::StoreTarget, name: &str) -> Result<()> {
    if let Some(root) = &target.native_root {
        super::super::validate_dir(
            root,
            crate::snapshot::PAYLOAD_DIR,
            "adjacency directory",
            super::super::DirAdmit::Required,
        )
        .map_err(|_| Error::storage(format!("invalid adjacency payload path: {name}")))?;
        super::super::validate_native_plain_file(root, name, "adjacency payload")?;
    }
    Ok(())
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
    for (key, digest) in rel_digest_entries(target, rel_parts)? {
        metadata.push(KeyValue::new(key, digest));
    }
    Ok(metadata)
}

/// Every rel part's row-group digests as `(metadata key, hex digest)` — the
/// writer folds them into the payload's file metadata, the verifier compares
/// the payload's metadata against the same list.
pub(crate) fn rel_digest_entries(
    target: &crate::StoreTarget,
    rel_parts: &[ShardPart],
) -> Result<Vec<(String, String)>> {
    let mut entries = Vec::new();
    for part in rel_parts {
        // The writer's own chunk reader: digests stream from the part, never
        // through a whole-part heap copy.
        let file = VfsChunkReader::open(Arc::clone(&target.vfs), &part.path, MAX_FILE)
            .map_err(|e| crate::storage_context(e, format_args!("rel part {}", part.path)))?;
        for digest in row_group_digests(&file)? {
            entries.push((
                digest_key(&part.path, digest.ordinal),
                digest_hex(digest.crc32),
            ));
        }
    }
    Ok(entries)
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
    graph: &dyn relate_engine_api::GraphImport,
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
fn payload_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("node_id", DataType::Int64, false),
        Field::new("type_name", DataType::Utf8, false),
        Field::new("incoming", DataType::Boolean, false),
        Field::new("hops", DataType::Int64, false),
        Field::new("stream_bytes", DataType::Binary, false),
        Field::new(
            "tombstones",
            DataType::List(Arc::new(Field::new("item", DataType::Int64, false))),
            false,
        ),
        Field::new(
            "tombstone_deleted",
            DataType::List(Arc::new(Field::new("item", DataType::Boolean, false))),
            false,
        ),
        Field::new("crc32", DataType::UInt32, false),
    ]))
}

/// The one `stream_bytes` frame: sealed hop count, sealed stream verbatim,
/// delta hop ids. The write side refuses the shapes the read side refuses —
/// [`relate_engine_api::sealed_chunk_shape`] is that check.
fn frame(chunk: &AdjacencyChunk) -> Result<Vec<u8>> {
    let sealed_hops = u32::try_from(relate_engine_api::sealed_chunk_shape(
        chunk.node_id,
        &chunk.type_name,
        chunk.sealed_hops,
        &chunk.sealed,
    )?)
    .map_err(|_| {
        Error::storage(format!(
            "adjacency chunk (node {}, type {}) seals {} hops, over the frame's u32",
            chunk.node_id, chunk.type_name, chunk.sealed_hops
        ))
    })?;
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
    type_name: Vec<String>,
    incoming: Vec<bool>,
    hops: Vec<i64>,
    stream: Vec<Vec<u8>>,
    tombstones: Vec<Vec<ChunkTombstone>>,
    crc32: Vec<u32>,
}

impl PayloadRows {
    fn push(&mut self, chunk: AdjacencyChunk) -> Result<()> {
        let total_hops = chunk.sealed_hops + chunk.delta.len() as u64;
        let hops = i64::try_from(total_hops)
            .map_err(|_| Error::storage("adjacency chunk holds more hops than i64 holds"))?;
        let framed = frame(&chunk)?;
        let key = RowKey {
            node_id: chunk.node_id,
            type_name: &chunk.type_name,
            side: chunk.side,
        };
        // The digest is taken before the name moves into its column.
        let crc32 = row_crc(key, total_hops, &framed, &chunk.tombstones);
        self.node_id.push(chunk.node_id);
        self.type_name.push(chunk.type_name);
        self.incoming.push(chunk.side.is_in());
        self.hops.push(hops);
        self.crc32.push(crc32);
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
        let mut type_name = StringBuilder::with_capacity(self.len(), 0);
        for value in &self.type_name {
            type_name.append_value(value);
        }
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
        let mut deleted = ListBuilder::new(BooleanBuilder::new()).with_field(Field::new(
            "item",
            DataType::Boolean,
            false,
        ));
        for ids in &self.tombstones {
            for tombstone in ids {
                tombstones.values().append_value(tombstone.id);
                deleted.values().append_value(tombstone.deleted);
            }
            tombstones.append(true);
            deleted.append(true);
        }
        let mut crc32 = UInt32Builder::with_capacity(self.len());
        crc32.append_slice(&self.crc32);
        let columns: Vec<ArrayRef> = vec![
            Arc::new(node_id.finish()),
            Arc::new(type_name.finish()),
            Arc::new(incoming.finish()),
            Arc::new(hops.finish()),
            Arc::new(stream.finish()),
            Arc::new(tombstones.finish()),
            Arc::new(deleted.finish()),
            Arc::new(crc32.finish()),
        ];
        let batch = RecordBatch::try_new(Arc::clone(&self.schema), columns)
            .map_err(|e| Error::storage(e.to_string()))?;
        writer
            .write(&batch)
            .map_err(|e| Error::storage(e.to_string()))?;
        self.node_id.clear();
        self.type_name.clear();
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
    graph: &dyn relate_engine_api::GraphImport,
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
        type_name: Vec::with_capacity(ROW_GROUP_CAP),
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
        .set_max_row_group_size(ROW_GROUP_CAP)
        .set_key_value_metadata(Some(metadata))
        .build()
}
