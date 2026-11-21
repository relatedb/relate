//! The opt-in adjacency payload (ADR-0551): the manifest entry, the
//! payload object's schema and row order, the sealed/unsealed split, the
//! rel-row-group digests, the rel-count gate and the opt-in gate — and the
//! roundtrip suites: an installed open answering like a re-folded one over
//! every node's adjacency, the tamper matrix (a rel-row edit, a truncated
//! payload, a payload from another checkpoint) and the default-off open.
//!
//! The support helpers come first; the tests follow as a list.

use super::super::payload::{
    column, column_of, digest_hex, digest_key, frame_header, named, payload_writer_properties,
    row_crc, row_group_digests, PayloadConfig, RowKey, DEFAULT_MIN_RELS, DIGEST_ALGORITHM,
    META_ALGORITHM, META_DIGEST_PREFIX, META_LSN,
};
use super::super::*;
use super::harness::*;
use crate::snapshot::{ADJACENCY_PAYLOAD_ENV, ADJACENCY_PAYLOAD_MIN_RELS_ENV};
use crate::{memory_store, memory_store_with_codec, Wal};
use arrow::array::{
    ArrayRef, BinaryArray, BinaryBuilder, BooleanArray, Int64Array, ListArray, StringArray,
    UInt32Array,
};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;
use parquet::file::metadata::KeyValue;
use relate_core::Value;
use relate_engine_api::{AdjacencyChunkRef, AdjacencySide, ChunkTombstone, RedoOp};
use relate_executor::node_adjacencies;
use relate_executor::Graph;
use relate_executor::Side;
use relate_test_support::{EnvLock, EnvRestore};
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// Two nodes and `rels` relationships between them, committed.
fn two_nodes_with_rels(rels: usize) -> (Graph, Wal) {
    let graph = Graph::default();
    let mut wal = Wal::memory();
    graph.begin_write().unwrap();
    graph.create_node(vec![], Default::default()).unwrap();
    graph.create_node(vec![], Default::default()).unwrap();
    commit_open_write(&graph, &mut wal);
    if rels > 0 {
        graph.begin_write().unwrap();
        for _ in 0..rels {
            graph
                .create_rel(0, 1, "LINK".into(), Default::default())
                .unwrap();
        }
        commit_open_write(&graph, &mut wal);
    }
    (graph, wal)
}

fn checkpoint(store: &StoreTarget, graph: &Graph, wal: &Wal, payload: PayloadConfig) -> Result<()> {
    checkpoint_at_with_config(
        store,
        graph,
        wal.lsn,
        graph.commit_ts(),
        MAX_FILE,
        None,
        payload,
    )
}

/// One payload row, decoded.
#[derive(Debug, PartialEq, Eq)]
struct Row {
    node_id: i64,
    type_name: String,
    side: AdjacencySide,
    hops: i64,
    stream: Vec<u8>,
    tombstones: Vec<ChunkTombstone>,
    crc32: u32,
}

/// Every payload row, with the row-group boundaries kept as the batches
/// the reader yields (its batch size is one row group). The columns decode
/// by name through the payload module's one helper, so this reader holds no
/// second copy of the column order.
fn payload_batches(store: &StoreTarget, manifest: &Manifest) -> Vec<Vec<Row>> {
    let payload = manifest
        .adjacency_payload
        .as_ref()
        .expect("the manifest names a payload");
    let bytes = bytes::Bytes::from(get_object(store, &payload.path).unwrap());
    let reader =
        parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(bytes.clone())
            .unwrap()
            .build()
            .unwrap();
    let mut batches = Vec::new();
    for batch in reader {
        let batch = batch.unwrap();
        let node_id = column::<Int64Array>(&batch, "node_id").unwrap();
        let type_name = column::<StringArray>(&batch, "type_name").unwrap();
        let incoming = column::<BooleanArray>(&batch, "incoming").unwrap();
        let hops = column::<Int64Array>(&batch, "hops").unwrap();
        let stream = column::<BinaryArray>(&batch, "stream_bytes").unwrap();
        let tombstones = column::<ListArray>(&batch, "tombstones").unwrap();
        let deleted = column::<ListArray>(&batch, "tombstone_deleted").unwrap();
        let crc32 = column::<UInt32Array>(&batch, "crc32").unwrap();
        let ids = column_of::<Int64Array>(tombstones.values().as_ref(), "tombstones").unwrap();
        let kinds =
            column_of::<BooleanArray>(deleted.values().as_ref(), "tombstone_deleted").unwrap();
        let offsets = tombstones.value_offsets();
        assert_eq!(deleted.value_offsets(), offsets);
        let rows = (0..batch.num_rows())
            .map(|i| Row {
                node_id: node_id.value(i),
                type_name: type_name.value(i).to_owned(),
                side: AdjacencySide::from_is_in(incoming.value(i)),
                hops: hops.value(i),
                stream: stream.value(i).to_vec(),
                tombstones: (offsets[i] as usize..offsets[i + 1] as usize)
                    .map(|j| ChunkTombstone {
                        id: ids.value(j),
                        deleted: kinds.value(j),
                    })
                    .collect(),
                crc32: crc32.value(i),
            })
            .collect();
        batches.push(rows);
    }
    batches
}

fn payload_rows(store: &StoreTarget, manifest: &Manifest) -> Vec<Row> {
    payload_batches(store, manifest)
        .into_iter()
        .flatten()
        .collect()
}

/// The delta hop ids framed in `stream` (sealed count 0 here).
fn delta_ids(stream: &[u8]) -> Vec<i64> {
    let (sealed_hops, ids) = frame_header(stream).unwrap();
    assert_eq!(sealed_hops, 0);
    ids.chunks_exact(8)
        .map(|chunk| i64::from_le_bytes(chunk.try_into().unwrap()))
        .collect()
}

/// The delta hop ids framed in `row`'s `stream_bytes`, in stored order.
fn hop_ids(row: &Row) -> Vec<i64> {
    let (sealed_hops, rest) = frame_header(&row.stream).unwrap();
    let split = rest.len() - 8 * (row.hops as u64 - sealed_hops) as usize;
    rest[split..]
        .chunks_exact(8)
        .map(|hop| i64::from_le_bytes(hop.try_into().unwrap()))
        .collect()
}

/// `row`'s integrity digest as the writer computed it.
fn crc_of(row: &Row) -> u32 {
    row_crc(
        RowKey {
            node_id: row.node_id,
            type_name: &row.type_name,
            side: row.side,
        },
        row.hops as u64,
        &row.stream,
        &row.tombstones,
    )
}

/// The hop ids `row`'s tombstones hide.
fn tombstone_ids(row: &Row) -> Vec<i64> {
    row.tombstones
        .iter()
        .map(|tombstone| tombstone.id)
        .collect()
}

/// One payload row as the install reads it: the framed `stream_bytes`
/// splits where the row's total hop count leaves the sealed stream; the
/// ref borrows the row's stream and tombstones and the decoded delta.
fn chunk_ref<'a>(row: &'a Row, delta: &'a [i64]) -> AdjacencyChunkRef<'a> {
    let (sealed_hops, rest) = frame_header(&row.stream).unwrap();
    let split = rest.len() - 8 * delta.len();
    AdjacencyChunkRef {
        node_id: row.node_id,
        type_name: &row.type_name,
        side: row.side,
        sealed_hops,
        sealed: &rest[..split],
        delta,
        tombstones: &row.tombstones,
    }
}

/// Append `ops` at the next commit timestamp and apply them onto `graph`,
/// as a database commit does.
fn commit(graph: &Graph, wal: &mut Wal, ops: Vec<RedoOp>) {
    let ts = graph.commit_ts() + 1;
    wal.append_commit(ts, &ops).unwrap();
    graph.apply_redo(ops, ts).unwrap();
}

fn create_node(id: i64) -> RedoOp {
    RedoOp::create_node(id, vec!["N".into()], BTreeMap::new())
}

/// A `LINK` rel, with an explicit `ts` property when `ts` is given.
fn create_rel(id: i64, start: i64, end: i64, ts: Option<i64>) -> RedoOp {
    let props = ts.map_or_else(BTreeMap::new, |ts| {
        BTreeMap::from([("ts".to_string(), Value::Int(ts))])
    });
    RedoOp::create_rel(id, start, end, "LINK".into(), props)
}

/// A store of three nodes; a 32-hop `LINK` chunk the checkpoint's own GC
/// pass folds (so the payload carries a sealed stream) and a second rel type
/// between other endpoints (the direction expansion's other arm).
fn every_tier_store() -> crate::StoreTarget {
    let store = memory_store();
    let (vfs, prefix) = store.wal_vfs();
    let mut wal = Wal::open(vfs, prefix, 0).unwrap();
    let writer = Graph::default();
    commit(
        &writer,
        &mut wal,
        vec![create_node(0), create_node(1), create_node(2)],
    );
    let mut rels: Vec<RedoOp> = (0..32)
        .map(|id| create_rel(id, 0, 1, (id % 4 == 0).then(|| 100 - id)))
        .collect();
    rels.push(RedoOp::create_rel(
        32,
        0,
        2,
        "OTHER".into(),
        BTreeMap::new(),
    ));
    rels.push(RedoOp::create_rel(
        33,
        2,
        1,
        "OTHER".into(),
        BTreeMap::new(),
    ));
    commit(&writer, &mut wal, rels);
    // The checkpoint's GC pass folds the 32-hop chunk (ADR-0551's floor).
    writer.gc_versions();
    checkpoint(&store, &writer, &wal, PayloadConfig::enabled(1)).unwrap();
    store
}

/// Two nodes and six rels between them, two of them carrying distinct ts
/// values — a chunk whose hop order the payload's ts-less hop list still
/// has to reproduce. Returns the store with the checkpoint and the writer's
/// WAL.
fn equivalence_store() -> (crate::StoreTarget, Graph, Wal) {
    let store = memory_store();
    let (vfs, prefix) = store.wal_vfs();
    let mut wal = Wal::open(vfs, prefix, 0).unwrap();
    let writer = Graph::default();
    commit(&writer, &mut wal, vec![create_node(0), create_node(1)]);
    let rels = vec![
        create_rel(0, 0, 1, Some(5)),
        create_rel(1, 0, 1, Some(3)),
        create_rel(2, 0, 1, None),
        create_rel(3, 0, 1, None),
        create_rel(4, 0, 1, None),
        create_rel(5, 0, 1, None),
    ];
    commit(&writer, &mut wal, rels);
    checkpoint(&store, &writer, &wal, PayloadConfig::enabled(1)).unwrap();
    (store, writer, wal)
}

/// An open's adjacency answers: the ts-visible neighbors per node and
/// direction plus both physical degrees.
fn adjacency_answers(graph: &Graph, ts: u64) -> Vec<(i64, Vec<i64>, Vec<i64>)> {
    let mut answers = Vec::new();
    for node in 0..3 {
        answers.push((
            node,
            graph.visible_adjacency(node, Side::Start, ts),
            graph.degree_out(node),
        ));
        answers.push((
            node,
            graph.visible_adjacency(node, Side::End, ts),
            graph.degree_in(node),
        ));
    }
    answers
}

/// The same store opened with the payload selection dropped, so it derives
/// adjacency from the rel rows — the reference every installed open answers
/// against.
fn refold_after_dropping_payload_selection(store: &StoreTarget) -> Graph {
    let mut forged = manifest(store);
    forged.adjacency_payload = None;
    put_manifest(store, &forged);
    crate::open_store::<Graph>(store).unwrap().0
}

/// Open the store and assert it degraded: one warning naming `needle` —
/// in the payload path or the cause — and answers matching a payload-less
/// open of the same store.
fn assert_degrades(store: &StoreTarget, needle: &str) -> Graph {
    let (opened, _, _, report) = crate::open_store_with_report::<Graph>(store).unwrap();
    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    let warning = &report.warnings[0];
    assert!(
        warning.path.contains(needle) || warning.cause.contains(needle),
        "{warning:?}"
    );
    let refolded = refold_after_dropping_payload_selection(store);
    let ts = refolded.commit_ts();
    for at in [1, ts] {
        assert_eq!(
            adjacency_answers(&opened, at),
            adjacency_answers(&refolded, at),
            "at ts {at}"
        );
    }
    opened
}

/// The degraded open fell back to the rows: the 6-hop chunk sealed, which
/// the payload kept delta.
fn assert_refolded(store: &StoreTarget, needle: &str) -> Graph {
    let opened = assert_degrades(store, needle);
    assert_eq!(opened.degree_out(0).len(), 6);
    assert_eq!(opened.sealed_chunk_count(0, Side::Start), 1);
    opened
}

/// The payload at `path`, rewritten with `edit` applied to its file
/// metadata and its rows left unchanged — the metadata-tamper constructor.
fn rewrite_payload_metadata(
    store: &StoreTarget,
    path: &str,
    edit: impl FnOnce(&mut Vec<KeyValue>),
) -> Vec<u8> {
    let bytes = bytes::Bytes::from(get_object(store, path).unwrap());
    let reader =
        parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(bytes).unwrap();
    let schema = reader.schema().clone();
    let mut key_values = reader
        .metadata()
        .file_metadata()
        .key_value_metadata()
        .unwrap()
        .clone();
    edit(&mut key_values);
    let mut out = Vec::new();
    let mut writer = ArrowWriter::try_new(
        &mut out,
        schema,
        Some(payload_writer_properties(key_values)),
    )
    .unwrap();
    for batch in reader.build().unwrap() {
        writer.write(&batch.unwrap()).unwrap();
    }
    writer.close().unwrap();
    out
}

/// The payload at `path`, its rows and metadata unchanged except that the
/// first rel-row-group digest names a hash no rel part produces.
fn payload_with_forged_digest(store: &StoreTarget, path: &str) -> Vec<u8> {
    rewrite_payload_metadata(store, path, |key_values| {
        let digest = key_values
            .iter_mut()
            .find(|kv| kv.key.starts_with(META_DIGEST_PREFIX))
            .expect("the payload names rel row-group digests");
        digest.value = Some(digest_hex(0));
    })
}

/// The payload at `path` with the first delta hop id of its first row
/// retargeted to another live rel id (`id ^ 1`) — one byte inside the hop
/// list — and the crc32 column still naming the original bytes.
fn payload_with_flipped_delta(store: &StoreTarget, path: &str) -> Vec<u8> {
    let bytes = bytes::Bytes::from(get_object(store, path).unwrap());
    let reader =
        parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(bytes).unwrap();
    let schema = reader.schema().clone();
    let key_values = reader
        .metadata()
        .file_metadata()
        .key_value_metadata()
        .unwrap()
        .clone();
    let mut out = Vec::new();
    let mut writer = ArrowWriter::try_new(
        &mut out,
        schema,
        Some(payload_writer_properties(key_values)),
    )
    .unwrap();
    let mut flipped = false;
    for batch in reader.build().unwrap() {
        let batch = batch.unwrap();
        let hops = column::<Int64Array>(&batch, "hops").unwrap();
        let stream = column::<BinaryArray>(&batch, "stream_bytes").unwrap();
        let mut rebuilt = BinaryBuilder::new();
        for i in 0..batch.num_rows() {
            let mut value = stream.value(i).to_vec();
            if !flipped {
                let (sealed, _) = frame_header(&value).unwrap();
                let delta = hops.value(i) as u64 - sealed;
                let start = value.len() - 8 * delta as usize;
                let id = i64::from_le_bytes(value[start..start + 8].try_into().unwrap());
                value[start..start + 8].copy_from_slice(&(id ^ 1).to_le_bytes());
                flipped = true;
            }
            rebuilt.append_value(&value);
        }
        let mut columns: Vec<ArrayRef> = batch.columns().to_vec();
        let stream_column = batch.schema().index_of("stream_bytes").unwrap();
        columns[stream_column] = Arc::new(rebuilt.finish());
        let batch = RecordBatch::try_new(batch.schema(), columns).unwrap();
        writer.write(&batch).unwrap();
    }
    assert!(flipped, "the payload carries a delta hop id");
    writer.close().unwrap();
    out
}

#[test]
fn the_opt_in_is_off_by_default_with_the_documented_threshold() {
    let _env = EnvLock::acquire();
    let _payload = EnvRestore::new(ADJACENCY_PAYLOAD_ENV);
    let _min_rels = EnvRestore::new(ADJACENCY_PAYLOAD_MIN_RELS_ENV);
    std::env::remove_var(ADJACENCY_PAYLOAD_ENV);
    std::env::remove_var(ADJACENCY_PAYLOAD_MIN_RELS_ENV);
    assert_eq!(
        PayloadConfig::from_env().unwrap(),
        PayloadConfig::disabled()
    );
    assert_eq!(DEFAULT_MIN_RELS, 10_000_000);
}

#[test]
fn payload_is_listed_at_the_checkpoint_lsn_with_the_chunk_rows() {
    let store = memory_store();
    let (graph, wal) = two_nodes_with_rels(3);
    checkpoint(&store, &graph, &wal, PayloadConfig::enabled(1)).unwrap();
    let manifest = manifest(&store);
    let payload = manifest.adjacency_payload.as_ref().unwrap();
    assert!(is_payload_object_name(&payload.path), "{}", payload.path);
    let rows = payload_rows(&store, &manifest);
    // One out chunk on node 0 and one in chunk on node 1, each 3 hops.
    assert_eq!(rows.len(), 2, "{rows:?}");
    for row in &rows {
        assert_eq!(row.hops, 3);
        assert_eq!(delta_ids(&row.stream).len(), 3);
        // Every row carries the integrity digest over its identity columns
        // and its hop data.
        assert_eq!(row.crc32, crc_of(row));
    }
    assert_eq!((rows[0].node_id, rows[0].side), (0, AdjacencySide::Out));
    assert_eq!((rows[1].node_id, rows[1].side), (1, AdjacencySide::In));
    assert_eq!(rows[0].type_name, rows[1].type_name);
}

#[test]
fn payload_rows_are_row_group_ordered_by_node_id() {
    let store = memory_store();
    let (graph, wal) = two_nodes_with_rels(2);
    checkpoint(&store, &graph, &wal, PayloadConfig::enabled(1)).unwrap();
    for batch in payload_batches(&store, &manifest(&store)) {
        assert!(
            batch.len() <= crate::codec::parquet::ROW_GROUP_CAP,
            "{batch:?}"
        );
        assert!(
            batch
                .windows(2)
                .all(|pair| pair[0].node_id <= pair[1].node_id),
            "{batch:?}"
        );
    }
}

#[test]
fn payload_covers_the_one_hop_tail() {
    let store = memory_store();
    let (graph, mut wal) = two_nodes_with_rels(0);
    // The first checkpoint has no relationship; the tail below is a chunk
    // of exactly one hop, far under the seal floor, so it stays a hop list.
    checkpoint(&store, &graph, &wal, PayloadConfig::enabled(1)).unwrap();
    assert!(manifest(&store).adjacency_payload.is_none());
    graph.begin_write().unwrap();
    let rel = graph
        .create_rel(0, 1, "LINK".into(), Default::default())
        .unwrap();
    commit_open_write(&graph, &mut wal);
    checkpoint(&store, &graph, &wal, PayloadConfig::enabled(1)).unwrap();
    let rows = payload_rows(&store, &manifest(&store));
    let tail: Vec<&Row> = rows.iter().filter(|row| row.hops == 1).collect();
    assert_eq!(tail.len(), 2, "{rows:?}");
    for row in tail {
        assert_eq!(delta_ids(&row.stream), vec![rel]);
    }
}

#[test]
fn payload_carries_a_sealed_stream_verbatim() {
    let store = memory_store();
    let (graph, wal) = two_nodes_with_rels(32);
    // The checkpoint's GC pass folds a 32-hop chunk (ADR-0551's floor).
    graph.gc_versions();
    checkpoint(&store, &graph, &wal, PayloadConfig::enabled(32)).unwrap();
    let first = payload_rows(&store, &manifest(&store));
    let row = first
        .iter()
        .find(|row| row.hops == 32)
        .expect("a sealed chunk");
    let (sealed_hops, stream) = frame_header(&row.stream).unwrap();
    assert_eq!(sealed_hops, 32);
    // The codec stream is the chunk's only payload — no trailing hop ids —
    // and is denser than the 8-byte hop list it replaces.
    assert!(
        !stream.is_empty() && stream.len() < 32 * 8,
        "{}",
        stream.len()
    );
    // A second publication of the unchanged chunk carries the same bytes.
    checkpoint(&store, &graph, &wal, PayloadConfig::enabled(32)).unwrap();
    let second = payload_rows(&store, &manifest(&store));
    let again = second
        .iter()
        .find(|candidate| {
            (candidate.node_id, candidate.type_name.as_str()) == (row.node_id, &row.type_name)
        })
        .expect("the sealed chunk again");
    assert_eq!(again.stream, row.stream);
}

#[test]
fn payload_is_absent_when_the_opt_in_is_off() {
    let store = memory_store();
    let (graph, wal) = two_nodes_with_rels(2);
    checkpoint(&store, &graph, &wal, PayloadConfig::disabled()).unwrap();
    let manifest = manifest(&store);
    assert!(manifest.adjacency_payload.is_none());
    assert!(!store.vfs.exists(crate::snapshot::PAYLOAD_DIR).unwrap());
}

#[test]
fn payload_is_absent_below_the_rel_threshold() {
    let store = memory_store();
    let (graph, wal) = two_nodes_with_rels(2);
    checkpoint(&store, &graph, &wal, PayloadConfig::enabled(3)).unwrap();
    let manifest = manifest(&store);
    assert!(manifest.adjacency_payload.is_none());
    assert!(!store.vfs.exists(crate::snapshot::PAYLOAD_DIR).unwrap());
}

#[test]
fn a_json_snapshot_publishes_no_payload() {
    let store = memory_store_with_codec(SnapshotCodecKind::Json).unwrap();
    let (graph, wal) = two_nodes_with_rels(2);
    crate::checkpoint_with_lsn(&store, &graph, wal.lsn, None).unwrap();
    assert!(!store.vfs.exists(crate::snapshot::PAYLOAD_DIR).unwrap());
}

#[test]
fn a_payload_at_another_checkpoint_degrades_to_the_refold() {
    let store = memory_store();
    let (graph, wal) = two_nodes_with_rels(6);
    checkpoint(&store, &graph, &wal, PayloadConfig::enabled(1)).unwrap();
    let manifest = manifest(&store);
    let path = manifest.adjacency_payload.as_ref().unwrap().path.clone();
    let forged = rewrite_payload_metadata(&store, &path, |key_values| {
        for kv in key_values.iter_mut().filter(|kv| kv.key == META_LSN) {
            kv.value = Some((manifest.checkpoint_lsn + 1).to_string());
        }
    });
    put_object(&store, &path, &forged).unwrap();
    // A torn publication degrades, never refuses: the store opens on the
    // row re-fold, and the warning names both checkpoints.
    let (opened, _, _, report) = crate::open_store_with_report::<Graph>(&store).unwrap();
    assert_eq!(opened.degree_out(0).len(), 6);
    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    let warning = &report.warnings[0];
    assert!(warning.cause.contains("checkpoint lsn"), "{warning:?}");
    // The re-fold ran: the 6-hop chunk sealed, which the payload kept
    // delta.
    assert_eq!(opened.sealed_chunk_count(0, Side::Start), 1);
}

#[test]
fn the_payload_digests_every_rel_row_group_and_an_edit_flips_one() {
    let store = memory_store();
    // 1024-row row groups: 1100 relationships span two.
    let (graph, wal) = two_nodes_with_rels(1100);
    checkpoint(&store, &graph, &wal, PayloadConfig::enabled(1)).unwrap();
    let manifest = manifest(&store);
    let part = &manifest.rel_parts[0];
    let part_bytes = get_object(&store, &part.path).unwrap();
    let metadata = parquet::file::metadata::ParquetMetaDataReader::new()
        .parse_and_finish(&bytes::Bytes::from(part_bytes.clone()))
        .unwrap();
    assert_eq!(metadata.num_row_groups(), 2);
    let digests = row_group_digests(&bytes::Bytes::from(part_bytes.clone())).unwrap();
    assert_eq!(digests.len(), 2, "{digests:?}");

    // The payload names the algorithm and every rel row group's digest.
    let payload = manifest.adjacency_payload.as_ref().unwrap();
    let payload_bytes = bytes::Bytes::from(get_object(&store, &payload.path).unwrap());
    let reader =
        parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(payload_bytes)
            .unwrap();
    let key_values = reader
        .metadata()
        .file_metadata()
        .key_value_metadata()
        .unwrap();
    assert_eq!(named(key_values, META_ALGORITHM), Some(DIGEST_ALGORITHM));
    for digest in &digests {
        assert_eq!(
            named(key_values, &digest_key(&part.path, digest.ordinal)),
            Some(digest_hex(digest.crc32).as_str())
        );
    }

    // A one-byte edit inside the first row group flips only its digest.
    let mut edited = part_bytes;
    edited[digests[0].start as usize + digests[0].len as usize / 2] ^= 0x01;
    let after = row_group_digests(&bytes::Bytes::from(edited)).unwrap();
    assert_ne!(after[0].crc32, digests[0].crc32);
    assert_eq!(after[1].crc32, digests[1].crc32);
}

/// A hop deleted after its chunk folded stays in the sealed stream; the
/// payload carries the delete's tombstone beside it, and an install of that
/// row hides the hop exactly as the running store at the watermark and a
/// re-folded open do.
#[test]
fn a_delete_after_the_fold_installs_with_its_tombstone() {
    const DELETED: i64 = 7;
    let store = memory_store();
    let (graph, mut wal) = two_nodes_with_rels(32);
    // The checkpoint's GC pass folds the 32-hop chunk (ADR-0551's floor);
    // the delete lands on the sealed stream, never rewrites it.
    graph.gc_versions();
    graph.begin_write().unwrap();
    graph.delete_rel(DELETED);
    let deleted_at = commit_open_write(&graph, &mut wal);
    checkpoint(&store, &graph, &wal, PayloadConfig::enabled(1)).unwrap();

    let rows = payload_rows(&store, &manifest(&store));
    assert_eq!(rows.len(), 2, "{rows:?}");
    for row in &rows {
        assert_eq!(row.hops, 32, "{row:?}");
        assert_eq!(tombstone_ids(row), vec![DELETED], "{row:?}");
        // The delete's tombstone travels with its kind, never re-derived.
        assert!(row.tombstones.iter().all(|tombstone| tombstone.deleted));
    }

    let installed = Graph::default();
    let deltas: Vec<Vec<i64>> = rows.iter().map(hop_ids).collect();
    let chunks: Vec<AdjacencyChunkRef<'_>> = rows
        .iter()
        .zip(&deltas)
        .map(|(row, delta)| chunk_ref(row, delta))
        .collect();
    installed.install_adjacency_chunks(&chunks).unwrap();
    // The same checkpoint opened without an install re-folds the rows.
    let (refolded, ..) = crate::open_store::<Graph>(&store).unwrap();

    assert_eq!(installed.sealed_chunk_count(0, Side::Start), 1);
    for (node, side) in [(0, Side::Start), (1, Side::End)] {
        let running = graph.visible_adjacency(node, side, deleted_at);
        assert_eq!(running.len(), 31, "{running:?}");
        assert_eq!(
            installed.visible_adjacency(node, side, deleted_at),
            running,
            "installed vs running, node {node}"
        );
        for ts in [1, deleted_at] {
            assert_eq!(
                installed.visible_adjacency(node, side, ts),
                refolded.visible_adjacency(node, side, ts),
                "installed vs re-folded, node {node} ts {ts}"
            );
        }
    }
}

/// The payload-installed open and the row re-fold of the same checkpoint
/// answer every live node adjacency alike: the raw out and in lists and all
/// three direction expansions, over a sealed stream, a delta chunk and a
/// second type.
#[test]
fn an_installed_payload_matches_the_refold_across_every_node_adjacency() {
    let store = every_tier_store();
    let (installed, _, _, report) = crate::open_store_with_report::<Graph>(&store).unwrap();
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let refolded = refold_after_dropping_payload_selection(&store);
    let observed = node_adjacencies(&installed);
    assert_eq!(observed.len(), 3, "{observed:?}");
    assert_eq!(observed, node_adjacencies(&refolded));
}

/// The install keeps the payload's tiers — the checkpoint serializes
/// before its own GC fold, so the 6-hop chunk stays delta where a re-fold
/// seals it — while the answers and the replayed tail match a re-folded
/// open of the same store.
#[test]
fn an_installed_payload_answers_like_a_refolded_open_over_a_wal_tail() {
    let (store, writer, mut wal) = equivalence_store();
    // A WAL tail past the checkpoint: replay must extend the installed
    // adjacency exactly as it extends a re-folded one. The ts-9 hop is
    // the order-sensitive one — it belongs ahead of the installed ts-5
    // hops, which only a hop list whose ts the install read back can say.
    commit(
        &writer,
        &mut wal,
        vec![create_node(2), create_rel(6, 0, 1, Some(9))],
    );

    let (installed, _, _, report) = crate::open_store_with_report::<Graph>(&store).unwrap();
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let refolded = refold_after_dropping_payload_selection(&store);

    let ts = refolded.commit_ts();
    for at in [1, ts] {
        assert_eq!(
            adjacency_answers(&installed, at),
            adjacency_answers(&refolded, at),
            "at ts {at}"
        );
    }
    // The tail hop is visible on the installed graph too, in its ts order.
    assert!(installed.degree_out(0).contains(&6));
}

/// The payload object's bytes replaced by something that is not a Parquet
/// file.
#[test]
fn a_corrupt_payload_degrades_to_the_refold() {
    let (store, _writer, _wal) = equivalence_store();
    let path = manifest(&store).adjacency_payload.unwrap().path;
    put_object(&store, &path, b"not a parquet payload").unwrap();
    assert_refolded(&store, &path);
}

/// The manifest selects a payload object the store does not hold.
#[test]
fn a_missing_payload_degrades_to_the_refold() {
    let (store, _writer, _wal) = equivalence_store();
    let path = manifest(&store).adjacency_payload.unwrap().path;
    store.vfs.remove_file(&path).unwrap();
    assert_refolded(&store, "missing");
}

/// The tamper matrix's fourth row: one byte flipped inside a delta hop id,
/// retargeting it to another live rel id, with the crc32 column untouched.
/// The install verifies the row before its chunk lands, so the open
/// degrades instead of installing the wrong hop.
#[test]
fn a_flipped_delta_hop_degrades_to_the_refold() {
    let (store, _writer, _wal) = equivalence_store();
    let path = manifest(&store).adjacency_payload.unwrap().path;
    let forged = payload_with_flipped_delta(&store, &path);
    put_object(&store, &path, &forged).unwrap();
    assert_refolded(&store, "crc32");
}

/// The payload at `path` with its first row's `node_id` retargeted to
/// another node — one identity byte flipped — and the crc32 column still
/// naming the original row.
fn payload_with_flipped_node_id(store: &StoreTarget, path: &str) -> Vec<u8> {
    let bytes = bytes::Bytes::from(get_object(store, path).unwrap());
    let reader =
        parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(bytes).unwrap();
    let schema = reader.schema().clone();
    let key_values = reader
        .metadata()
        .file_metadata()
        .key_value_metadata()
        .unwrap()
        .clone();
    let mut out = Vec::new();
    let mut writer = ArrowWriter::try_new(
        &mut out,
        schema,
        Some(payload_writer_properties(key_values)),
    )
    .unwrap();
    let mut flipped = false;
    for batch in reader.build().unwrap() {
        let batch = batch.unwrap();
        let node = column::<Int64Array>(&batch, "node_id").unwrap();
        let mut rebuilt = arrow::array::Int64Builder::new();
        for i in 0..batch.num_rows() {
            let mut value = node.value(i);
            if !flipped {
                value ^= 1;
                flipped = true;
            }
            rebuilt.append_value(value);
        }
        let mut columns: Vec<ArrayRef> = batch.columns().to_vec();
        let node_column = batch.schema().index_of("node_id").unwrap();
        columns[node_column] = Arc::new(rebuilt.finish());
        let batch = RecordBatch::try_new(batch.schema(), columns).unwrap();
        writer.write(&batch).unwrap();
    }
    assert!(flipped, "the payload carries a row");
    writer.close().unwrap();
    out
}

/// The tamper matrix's fifth row: a row's identity column edited offline,
/// with its crc32 column untouched. The row's digest covers its node, type,
/// side and hop count, so the install refuses it instead of filing the
/// chunk under another node.
#[test]
fn a_flipped_node_id_degrades_to_the_refold() {
    let (store, _writer, _wal) = equivalence_store();
    let path = manifest(&store).adjacency_payload.unwrap().path;
    let forged = payload_with_flipped_node_id(&store, &path);
    put_object(&store, &path, &forged).unwrap();
    assert_refolded(&store, "crc32");
}

/// The payload rewritten with one rel-row-group digest that no rel part
/// hashes to.
#[test]
fn a_rel_row_group_digest_mismatch_degrades_to_the_refold() {
    let (store, _writer, _wal) = equivalence_store();
    let path = manifest(&store).adjacency_payload.unwrap().path;
    let forged = payload_with_forged_digest(&store, &path);
    put_object(&store, &path, &forged).unwrap();
    assert_refolded(&store, "rel_row_group");
}

/// The tamper matrix's first row: a rel part edited offline after the
/// payload published — one byte inside its first row group, so its digest
/// moved — degrades the open to the rows, which still decode.
#[test]
fn an_offline_one_byte_rel_edit_degrades_to_the_refold() {
    let (store, _writer, _wal) = equivalence_store();
    let part = manifest(&store).rel_parts[0].path.clone();
    let mut bytes = get_object(&store, &part).unwrap();
    // Offset 186 is "LINK"'s first letter in the type column's dictionary
    // value: flipping its low bit renames the type, so the rows still
    // decode, under another name.
    bytes[186] ^= 0x01;
    put_object(&store, &part, &bytes).unwrap();
    // The edit is a row change, not a torn shard: the rows still decode and
    // the re-fold answers from them.
    assert_refolded(&store, "rel parts hashes to");
}

/// The tamper matrix's second row: the payload file cut short, so its
/// Parquet footer is gone.
#[test]
fn a_truncated_payload_degrades_to_the_refold() {
    let (store, _writer, _wal) = equivalence_store();
    let path = manifest(&store).adjacency_payload.unwrap().path;
    let bytes = get_object(&store, &path).unwrap();
    put_object(&store, &path, &bytes[..bytes.len() / 2]).unwrap();
    assert_refolded(&store, "unreadable parquet metadata");
}

/// The tamper matrix's third row: a later checkpoint's payload file placed
/// over the one this manifest names.
#[test]
fn a_payload_from_another_checkpoint_degrades_to_the_refold() {
    let (store, writer, mut wal) = equivalence_store();
    let foreign = get_object(&store, &manifest(&store).adjacency_payload.unwrap().path).unwrap();
    // A second checkpoint at a later LSN publishes its own payload under a
    // fresh name.
    commit(&writer, &mut wal, vec![create_rel(6, 0, 1, None)]);
    checkpoint(&store, &writer, &wal, PayloadConfig::enabled(1)).unwrap();
    let path = manifest(&store).adjacency_payload.unwrap().path;
    put_object(&store, &path, &foreign).unwrap();
    let opened = assert_degrades(&store, "checkpoint lsn");
    assert_eq!(opened.all_rel_ids_at(opened.commit_ts()).len(), 7);
}

/// The default-off open: no payload is published, the open answers like the
/// writer, and it never opens a payload object.
#[test]
fn a_default_off_open_answers_like_the_writer_and_reads_no_payload() {
    let store = memory_store();
    let (graph, wal) = two_nodes_with_rels(6);
    checkpoint(&store, &graph, &wal, PayloadConfig::disabled()).unwrap();
    assert!(manifest(&store).adjacency_payload.is_none());
    // An object in the payload's directory the manifest never names — the
    // open must not open it.
    let planted = crate::snapshot::payload_object_name().unwrap();
    put_object(&store, &planted, b"never read").unwrap();
    let (watched, opens) = watch_opens(&store, &planted);
    let (opened, _, _, report) = crate::open_store_with_report::<Graph>(&watched).unwrap();
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(opens.load(Ordering::SeqCst), 0, "the open read a payload");
    assert_eq!(node_adjacencies(&opened), node_adjacencies(&graph));
}
