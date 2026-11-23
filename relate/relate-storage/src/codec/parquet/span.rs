//! Row-group spans: the decode units a concurrent open cuts a shard file
//! into (design: parallel checkpoint load). A shard file's row groups are
//! independently decodable, so every span reads through its own reader; the
//! loader cuts the file into at most one span per load thread of the current
//! load scope — balanced by row-group count, never splitting a group. A file
//! that cuts into one span keeps its one reader, on the current thread.
//!
//! The module owns the whole walk: [`SpanReaders`], the per-span decode and
//! install ([`load_reader`]) on the shared load pool, and the fold in
//! [`load`] that holds the file's id order across the spans after the join.

use std::ops::Range;
use std::sync::Arc;

use bytes::Bytes;
use parquet::arrow::arrow_reader::{
    ArrowReaderMetadata, ArrowReaderOptions, ParquetRecordBatchReader,
    ParquetRecordBatchReaderBuilder,
};
use parquet::file::metadata::ParquetMetaData;
use relate_core::{record_transient_lease, run_load_tasks};

use super::rules::{ascending, id_column, ShardRules};
use super::{Decoded, Error, Origin, Result, RowLoader, DECODE_BATCH_ROWS};

/// A shard file cut into decode spans: consecutive row groups, one span per
/// load thread. Holds the file and its footer metadata — both cheap to
/// clone — so every span builds its own reader without re-reading the
/// footer or re-walking the page preflight.
pub(crate) struct SpanReaders {
    file: Bytes,
    metadata: ArrowReaderMetadata,
    /// One row-group index range per span, in file order.
    groups: Vec<Range<usize>>,
}

impl SpanReaders {
    /// Cut `metadata`'s row groups into at most `spans` consecutive spans of
    /// balanced group count — the load's workers, one span each. A group is
    /// never split, so a file with fewer groups than workers keeps one span
    /// per group.
    pub(crate) fn open(file: Bytes, metadata: &Arc<ParquetMetaData>, spans: usize) -> Result<Self> {
        let groups = cut(metadata.num_row_groups(), spans);
        let metadata =
            ArrowReaderMetadata::try_new(Arc::clone(metadata), ArrowReaderOptions::default())
                .map_err(|e| Error::storage(e.to_string()))?;
        Ok(Self {
            file,
            metadata,
            groups,
        })
    }

    /// How many spans the file cuts into.
    pub(crate) fn span_count(&self) -> usize {
        self.groups.len()
    }

    /// The reader over span `span`'s row groups, batching at
    /// [`DECODE_BATCH_ROWS`] as the sequential reader does.
    pub(crate) fn reader(&self, span: usize) -> Result<ParquetRecordBatchReader> {
        let groups = self.groups[span].clone().collect();
        ParquetRecordBatchReaderBuilder::new_with_metadata(self.file.clone(), self.metadata.clone())
            .with_row_groups(groups)
            .with_batch_size(DECODE_BATCH_ROWS)
            .build()
            .map_err(|e| Error::storage(e.to_string()))
    }
}

/// Cut `groups` row groups into `spans` consecutive spans of balanced group
/// count: each span takes its even share of what is left, so the earlier
/// spans carry the smaller share (three groups over two spans is 1 + 2).
fn cut(groups: usize, spans: usize) -> Vec<Range<usize>> {
    let spans = spans.max(1).min(groups.max(1));
    let mut ranges = Vec::with_capacity(spans);
    let mut start = 0;
    for span in 0..spans {
        let end = start + (groups - start) / (spans - span);
        ranges.push(start..end);
        start = end;
    }
    ranges
}

/// The first and last id one span decoded, kept so a file split into spans
/// still proves its ids ascend across the split. `None` is a span that
/// decoded no rows.
type IdBounds = Option<FirstLast>;

/// The first and last id of one span's rows, in file order.
#[derive(Clone, Copy)]
struct FirstLast {
    first: i64,
    last: i64,
}

/// Decode one span's batches and install each as it goes, holding each
/// batch's ids to the shard rules' id window and the rows to the decoded
/// envelope. Every batch holds a transient lease only while it is
/// installed, so a span never accumulates its batches' memory.
fn load_reader(
    reader: ParquetRecordBatchReader,
    rules: Option<&ShardRules>,
    decoded: &Decoded,
    graph: &dyn relate_engine_api::GraphImport,
    origin: &Origin,
    load: RowLoader,
) -> Result<IdBounds> {
    // The columns and this span's id window come from the one `ShardRules`
    // — a monolith is handed none, so it reads no columns and checks no
    // window.
    let columns = rules.map_or(&[][..], ShardRules::columns);
    let mut window = rules.map(ShardRules::restart);
    let mut bounds: IdBounds = None;
    for batch in reader {
        let batch = batch.map_err(|e| Error::storage(e.to_string()))?;
        let _lease = record_transient_lease(batch.get_array_memory_size() as u64);
        decoded.batch(batch.num_rows())?;
        let ids = match &mut window {
            Some(window) => window.ids(&batch)?,
            None => id_column(&batch).ok_or_else(|| Error::storage("id column"))?,
        };
        if let (Some(first), Some(last)) = (ids.values().first(), ids.values().last()) {
            bounds = Some(FirstLast {
                first: bounds.map_or(*first, |bounds| bounds.first),
                last: *last,
            });
        }
        load(graph, origin, &batch, &ids, decoded, columns)?;
        drop(batch);
    }
    Ok(bounds)
}

/// One loaded file ([`super::load_batches`]): the file's spans decode and
/// install on the shared load pool, one task per span, and after the join
/// the file's id order holds across the span boundaries too. A file that
/// cuts into one span runs its one reader on the current thread. The file's
/// row and decoded accounting is one [`Decoded`] every reader charges as it
/// decodes, so a file over an envelope refuses the batch that crosses it
/// before that batch installs.
pub(super) fn load(
    spans: SpanReaders,
    rules: Option<&ShardRules>,
    graph: &dyn relate_engine_api::GraphImport,
    origin: &Origin,
    load: RowLoader,
) -> Result<()> {
    let decoded = Decoded::new();
    let tasks: Vec<_> = (0..spans.span_count())
        .map(|span| spans.reader(span))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .map(|reader| {
            let decoded = &decoded;
            move || load_reader(reader, rules, decoded, graph, origin, load)
        })
        .collect();
    let results = run_load_tasks(tasks)
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
    // The spans tile the file's row groups in file order, so folding their
    // bounds in span order is the one ascending run the serial reader
    // checked batch by batch.
    let mut last: Option<i64> = None;
    for bounds in results.into_iter().flatten() {
        if rules.is_some() {
            ascending(last, bounds.first)?;
        }
        last = Some(bounds.last);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::codec::parquet::fixtures::{
        commit_ops, load_part, write_bare_table, write_rel_table, write_table,
    };
    use crate::codec::parquet::{
        encode_wide_rows, file_ceiling, load_nodes, load_rels, preflight, with_test_envelope,
        with_wide_shard_writer, ColumnType, MAX_PAGE, MAX_ROW_BYTES, ROW_GROUP_CAP,
    };
    use crate::snapshot::Kind;
    use relate_core::{resolve_load_threads, with_load_scope, OpenTransientTracker};
    use relate_executor::Graph;
    use std::collections::HashSet;
    use std::sync::Mutex;
    use std::thread::{self, ThreadId};

    use super::*;

    /// Run `f` as a load scope of `threads` load threads.
    fn with_workers<R>(threads: usize, f: impl FnOnce() -> R) -> R {
        let threads = resolve_load_threads(Some(threads)).unwrap();
        with_load_scope(threads, Arc::default(), f)
    }

    /// The ids among the first `n` a loaded graph holds, in order.
    fn loaded_ids(graph: &Graph, n: i64) -> Vec<i64> {
        (0..n)
            .filter(|id| graph.node_at(*id, 1).is_some())
            .collect()
    }

    /// The ids a file's spans decode to, span by span.
    fn span_ids(bytes: &Bytes, spans: usize) -> (usize, Vec<i64>) {
        let builder = preflight(bytes.clone()).unwrap();
        let spans = SpanReaders::open(bytes.clone(), builder.metadata(), spans).unwrap();
        let mut ids = Vec::new();
        for span in 0..spans.span_count() {
            for batch in spans.reader(span).unwrap() {
                let batch = batch.unwrap();
                ids.extend(id_column(&batch).unwrap().values().iter().copied());
            }
        }
        (spans.span_count(), ids)
    }

    #[test]
    fn spans_are_cut_one_per_worker_over_the_row_groups() {
        // Three row groups (two full and a tail) over two workers: two
        // spans, and their readers tile the file exactly.
        let ids: Vec<i64> = (0..2 * ROW_GROUP_CAP as i64 + 5).collect();
        let bytes = Bytes::from(write_bare_table(ids.clone()).unwrap());
        let (spans, decoded) = span_ids(&bytes, 2);
        assert_eq!(spans, 2);
        assert_eq!(decoded, ids);
    }

    #[test]
    fn a_file_that_fits_one_span_keeps_one_reader() {
        // A store too small to shard — one row group, whatever the worker
        // count — is one span, and the loader takes its single-reader path.
        let ids: Vec<i64> = (0..ROW_GROUP_CAP as i64).collect();
        let bytes = Bytes::from(write_bare_table(ids.clone()).unwrap());
        let (spans, decoded) = span_ids(&bytes, 8);
        assert_eq!(spans, 1);
        assert_eq!(decoded, ids);
        let graph = Graph::default();
        load_part(load_nodes, &graph, &bytes).unwrap();
        assert_eq!(loaded_ids(&graph, ids.len() as i64), ids);
    }

    #[test]
    fn the_loader_walks_every_span_of_a_sharded_file() {
        // A three-group file over three workers; the load still decodes
        // every row, in file order.
        let ids: Vec<i64> = (0..2 * ROW_GROUP_CAP as i64 + 5).collect();
        let bytes = write_bare_table(ids.clone()).unwrap();
        with_workers(3, || {
            let graph = Graph::default();
            load_part(load_nodes, &graph, &bytes).unwrap();
            assert_eq!(loaded_ids(&graph, ids.len() as i64), ids);
        });
    }

    #[test]
    fn a_multi_span_file_over_the_decoded_envelope_refuses_during_decode_before_installing() {
        // Three row groups over two workers: each span alone fits the file's
        // decoded envelope, the two together do not. The shared charge
        // refuses the row that crosses it while the spans still decode, so
        // the file never installs every row — folding each worker's total
        // after the join would have installed them all first.
        let rows = 2 * ROW_GROUP_CAP as i64 + 5;
        let ids: Vec<i64> = (0..rows).collect();
        // One long property every row shares: its dictionary page keeps the
        // file's stored bytes far under the decoded envelope, which counts
        // each row's whole text.
        let text = format!(
            "{{\"p\":{{\"t\":\"string\",\"v\":\"{}\"}}}}",
            "a".repeat(2000)
        );
        let bytes = write_table(
            ids.clone(),
            vec![Some(Vec::new()); rows as usize],
            vec![Some(text.clone()); rows as usize],
        )
        .unwrap();
        // A node row is its 8 fixed bytes plus that text; the larger span
        // holds the tail group's rows too.
        let row_bytes = 8 + text.len() as u64;
        let span_bytes = (ROW_GROUP_CAP as u64 + 5) * row_bytes;
        let graph = Graph::default();
        let error = with_workers(2, || {
            with_test_envelope(MAX_PAGE, MAX_ROW_BYTES, span_bytes + row_bytes, || {
                load_part(load_nodes, &graph, &bytes)
            })
            .unwrap_err()
        });
        assert!(
            error.to_string().contains("actual decoded bytes"),
            "{error}"
        );
        let installed = loaded_ids(&graph, rows).len();
        assert!(
            installed < rows as usize,
            "the file installed {installed} of {rows} rows before refusing"
        );
    }

    #[test]
    fn spans_decode_the_exact_gappy_id_set_a_live_encode_writes() {
        // Ids 700..1400 never exist: `encode_live` skips them before
        // chunking, so the first row group carries the gap. A span's ids
        // must therefore come from the id column, never a stride guess.
        let graph = Graph::default();
        graph.begin_write().unwrap();
        for _ in 0..700 {
            graph.create_node(Vec::new(), Default::default()).unwrap();
        }
        commit_ops(&graph);
        graph.set_next_node_id(1400);
        graph.begin_write().unwrap();
        for _ in 0..700 {
            graph.create_node(Vec::new(), Default::default()).unwrap();
        }
        let ts = commit_ops(&graph);
        let requested: Vec<i64> = (0..2100).collect();
        let (bytes, _) = with_wide_shard_writer(Kind::Node, &[], file_ceiling(), |writer| {
            encode_wide_rows(writer, &graph, ts, Kind::Node, &requested, &[])
        })
        .unwrap();
        let expected: Vec<i64> = (0..700).chain(1400..2100).collect();
        let (spans, decoded) = span_ids(&bytes, 2);
        assert_eq!(spans, 2);
        assert_eq!(decoded, expected);
    }

    /// The restore under a load scope of one thread and of four builds the
    /// same graph, nodes and relationships alike.
    #[test]
    fn forced_worker_counts_restore_the_same_graph_as_the_serial_path() {
        let rows = 4 * ROW_GROUP_CAP as i64;
        let ids: Vec<i64> = (0..rows).collect();
        let nodes = write_bare_table(ids.clone()).unwrap();
        let rels = write_rel_table(
            ids.clone(),
            (0..rows).map(|id| id / 2).collect(),
            (0..rows).map(|id| id / 2 + 1).collect(),
            vec![Some("T".into()); rows as usize],
            vec![Some("{}".into()); rows as usize],
        )
        .unwrap();
        let restore = |workers: usize, graph: &Graph| {
            with_workers(workers, || {
                load_part(load_nodes, graph, &nodes).unwrap();
                load_part(load_rels, graph, &rels).unwrap();
            })
        };
        let serial = Graph::default();
        restore(1, &serial);
        let many = Graph::default();
        restore(4, &many);
        assert_eq!(loaded_ids(&serial, rows), ids);
        assert_eq!(loaded_ids(&many, rows), ids);
        for id in &ids {
            let from_serial = serial.node_at(*id, 1).expect("serial node");
            let from_many = many.node_at(*id, 1).expect("parallel node");
            assert_eq!(from_serial.labels, from_many.labels, "node {id}");
            assert_eq!(from_serial.properties, from_many.properties, "node {id}");
            let from_serial = serial.rel_at(*id, 1).expect("serial rel");
            let from_many = many.rel_at(*id, 1).expect("parallel rel");
            assert_eq!(from_serial.start, from_many.start, "rel {id}");
            assert_eq!(from_serial.end, from_many.end, "rel {id}");
            assert_eq!(from_serial.rel_type, from_many.rel_type, "rel {id}");
            assert_eq!(from_serial.properties, from_many.properties, "rel {id}");
        }
    }

    /// The load thread that loaded a batch, recorded by [`record_thread`].
    static PREPARED_ON: Mutex<Vec<(ThreadId, String)>> = Mutex::new(Vec::new());

    /// The node loader, noting the thread that runs it.
    fn record_thread(
        graph: &dyn relate_engine_api::GraphImport,
        origin: &Origin,
        batch: &arrow::record_batch::RecordBatch,
        ids: &arrow::array::Int64Array,
        decoded: &Decoded,
        columns: &[(String, ColumnType)],
    ) -> Result<()> {
        let current = thread::current();
        PREPARED_ON
            .lock()
            .unwrap()
            .push((current.id(), current.name().unwrap_or_default().to_owned()));
        crate::codec::parquet::wide_node_rows(graph, origin, batch, ids, decoded, columns)
    }

    #[test]
    fn spans_decode_on_shared_pool_threads_reused_across_parts_and_loads() {
        let rows = 4 * ROW_GROUP_CAP as i64;
        let ids: Vec<i64> = (0..rows).collect();
        let bytes = write_bare_table(ids.clone()).unwrap();
        let tracker = Arc::new(OpenTransientTracker::new());
        let threads = resolve_load_threads(Some(4)).unwrap();
        let mut per_load = Vec::new();
        with_load_scope(threads, Arc::clone(&tracker), || {
            // Two parts of one open, then a second load: every load's four
            // spans run on the pool's four slots.
            for _ in 0..3 {
                let file = Bytes::from(bytes.clone());
                let builder = preflight(file.clone()).unwrap();
                let rules = ShardRules::open(&builder, Kind::Node, 0..i64::MAX).unwrap();
                let spans = SpanReaders::open(file, builder.metadata(), 4).unwrap();
                let graph = Graph::default();
                PREPARED_ON.lock().unwrap().clear();
                load(
                    spans,
                    Some(&rules),
                    &graph,
                    &Origin::Part(0..i64::MAX),
                    record_thread,
                )
                .unwrap();
                assert_eq!(loaded_ids(&graph, rows), ids);
                let seen: HashSet<_> = PREPARED_ON.lock().unwrap().drain(..).collect();
                per_load.push(seen);
            }
        });
        assert_eq!(per_load[0].len(), 4, "{:?}", per_load[0]);
        assert!(per_load[0]
            .iter()
            .all(|(id, name)| *id != thread::current().id() && name.starts_with("relate-load-")));
        assert!(per_load.iter().all(|seen| *seen == per_load[0]));
        assert!(tracker.peak_bytes() > 0);
    }

    #[test]
    fn a_load_scope_records_the_transient_peak_of_a_load() {
        let ids: Vec<i64> = (0..2 * ROW_GROUP_CAP as i64 + 5).collect();
        let bytes = write_bare_table(ids.clone()).unwrap();
        for k in [1, 4] {
            let tracker = Arc::new(OpenTransientTracker::new());
            let threads = resolve_load_threads(Some(k)).unwrap();
            let graph = Graph::default();
            with_load_scope(threads, Arc::clone(&tracker), || {
                load_part(load_nodes, &graph, &bytes).unwrap();
            });
            assert_eq!(loaded_ids(&graph, ids.len() as i64), ids);
            assert!(tracker.peak_bytes() > 0, "k = {k}");
        }
    }

    #[test]
    fn a_worker_count_above_the_group_count_never_splits_a_group() {
        // Two groups over eight workers: one span per group, not an empty
        // reader for the other six.
        let ids: Vec<i64> = (0..ROW_GROUP_CAP as i64 + 5).collect();
        let bytes = Bytes::from(write_bare_table(ids.clone()).unwrap());
        let (spans, decoded) = span_ids(&bytes, 8);
        assert_eq!(spans, 2);
        assert_eq!(decoded, ids);
    }
}
