//! The Parquet read side (ADR-0022): one shard file decoded into the
//! graph — what wrote it ([`Origin`]), the file's decoded-envelope
//! accounting ([`Decoded`]), the per-kind row decoders the load workers
//! run on each batch, and the staging that installs their rows in bounded
//! slices.

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};

use arrow::array::{Array, Int64Array, ListArray, StringArray};
use arrow::record_batch::RecordBatch;
use bytes::Bytes;
use relate_core::{Error, Node, Relationship, Result, Value};
use relate_engine_api::props_from_json;

use super::rules::ShardRules;
use super::{
    accumulate, check_row, encode, envelope, invalid, preflight, span, wide, ColumnType, Envelope,
    SpanReaders, MAX_ROWS,
};
use crate::codec::model::{install_slice_bytes, INSTALL_SLICE_ROWS};
use crate::snapshot::Kind;

/// Charge `add` to one of the file's shared counters, refusing when the
/// running total crosses `limit`. Every charge counts exactly once whichever
/// worker makes it, so the totals are the file's rather than a worker's.
fn charge(counter: &AtomicU64, add: u64, limit: u64, what: &str) -> Result<()> {
    let total = counter.fetch_add(add, Ordering::Relaxed).checked_add(add);
    match total {
        Some(total) if total <= limit => Ok(()),
        _ => Err(invalid(what)),
    }
}

/// Actual rows and decoded row bytes of one loaded file, held to the
/// ADR-0086 row and decoded envelopes across all of its batches. One
/// `Decoded` per file is shared by every span worker: the counters are
/// atomic adds and the limits are read once where the load starts, so the
/// file's envelopes hold while its spans decode concurrently — the row
/// that crosses an envelope refuses before it installs, not after the
/// workers join. Slices of its batch staged ahead of it may already be
/// installed ([`Staged`]); a failed open discards the graph.
pub(super) struct Decoded {
    limits: Envelope,
    /// The install slice, read with the limits on the loading thread so
    /// every worker of the file stages to the same bound.
    slice_bytes: u64,
    rows: AtomicU64,
    bytes: AtomicU64,
}

impl Decoded {
    pub(super) fn new() -> Self {
        Self {
            limits: envelope(),
            slice_bytes: install_slice_bytes(),
            rows: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
        }
    }

    pub(super) fn batch(&self, rows: usize) -> Result<()> {
        charge(&self.rows, rows as u64, MAX_ROWS, "actual decoded rows")
    }

    pub(super) fn row(&self, bytes: u64) -> Result<()> {
        check_row(bytes, self.limits)?;
        charge(
            &self.bytes,
            bytes,
            self.limits.decoded,
            "actual decoded bytes",
        )
    }

    /// Read-side row-envelope accounting (ADR-0086): the row's decoded bytes
    /// toward its bound — the one byte of slack so the check that follows
    /// (`row` over `check_row`) refuses a row at exactly the bound instead of
    /// passing it. Every per-row decoder site charges through here, never its
    /// own copy of the bound.
    pub(super) fn count_row_bytes(&self, sum: &mut u64, add: u64, what: &str) -> Result<()> {
        accumulate(sum, add, self.limits.row.saturating_add(1), what)
    }
}

/// Loads one shard file into the graph ([`load_nodes`] or [`load_rels`]):
/// the file's bytes, and what wrote it, which decides the rules it is held
/// to.
pub(crate) type Loader = fn(&dyn relate_engine_api::GraphImport, Vec<u8>, Origin) -> Result<()>;

/// What wrote a loaded file, which decides the rules it is held to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    /// A manifest part declaring id range `[id_lo, id_hi)`, held to
    /// [`ShardRules`] over that range as a checkpoint copying from it holds
    /// it (ADR-0183).
    Part(Range<i64>),
    /// A format ≤ 2 monolith (ADR-0005), not held to [`ShardRules`]: its
    /// writer predates v0.4.0, the oldest known to write the shard schema
    /// and ids ascending and unique within a file, so its schema and row
    /// order are unknown. It declares no range, and keeps the loader's
    /// column-type, persisted-id and properties-object checks. No checkpoint
    /// copies from it.
    Monolith,
}

impl Origin {
    /// The one policy for a slot a pre-shard monolith's writer could
    /// leave null and a manifest part's writer never does: a part —
    /// held to the shard rules, its writer one this binary knows —
    /// fails closed naming `what`; the monolith keeps its historical
    /// tolerance, taking `tolerated`.
    fn tolerated<T>(&self, what: &str, tolerated: T) -> Result<T> {
        match self {
            Origin::Part(_) => Err(Error::storage(what)),
            Origin::Monolith => Ok(tolerated),
        }
    }
}

/// One decoded batch's rows, installed into the graph: the per-kind row
/// decoder a load worker runs on the batch it just decoded.
pub(super) type RowLoader = fn(
    &dyn relate_engine_api::GraphImport,
    &Origin,
    &RecordBatch,
    &Int64Array,
    &Decoded,
    &[(String, ColumnType)],
) -> Result<()>;

/// Read one loaded file's batches into `load` with its id column and, for
/// a manifest part, its promoted columns — read once per file from the
/// schema the part carries (ADR-0534): preflighted, counted against the
/// ADR-0086 row envelope, and for a manifest part held to [`ShardRules`],
/// which check the ids it is handed. A monolith hands no columns — the
/// fixed layout, which is the wide schema with none — so one decode
/// path serves both shapes; it alone tolerates the pre-shard writers'
/// null label and type slots.
///
/// The file decodes through one reader per [`SpanReaders`] span, at most one
/// span per load thread of the current load scope ([`span::load`]): the
/// shared load pool decodes each span's batches and installs them as it
/// goes, charging the file's one [`Decoded`] and leasing one batch at a
/// time. Outside a load scope, or for a file too small to shard, the one
/// reader runs on the current thread.
fn load_batches(
    graph: &dyn relate_engine_api::GraphImport,
    bytes: Vec<u8>,
    kind: Kind,
    origin: &Origin,
    load: RowLoader,
) -> Result<()> {
    let file = Bytes::from(bytes);
    let builder = preflight(file.clone())?;
    let rules = match origin {
        Origin::Part(range) => Some(ShardRules::open(&builder, kind, range.clone())?),
        Origin::Monolith => None,
    };
    // A monolith's ids are not held to the ascending-and-unique rule (only
    // parts are), so its rows install on one thread: a duplicate id keeps
    // file order instead of racing the spans.
    let workers = if rules.is_some() {
        relate_core::current_load_threads().get()
    } else {
        1
    };
    let spans = SpanReaders::open(file, builder.metadata(), workers)?;
    span::load(spans, rules.as_ref(), graph, origin, load)
}

/// A shard's properties column: one tagged-JSON object per row.
struct Properties<'a>(&'a StringArray);

impl<'a> Properties<'a> {
    fn column(batch: &'a RecordBatch, index: usize) -> Result<Self> {
        let props = batch
            .column(index)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| Error::storage("properties column"))?;
        Ok(Self(props))
    }

    /// Row `i`'s JSON text, or `None` when the row has no properties.
    fn text(&self, i: usize) -> Option<&'a str> {
        (!self.0.is_null(i)).then(|| self.0.value(i))
    }

    /// Row `i`'s properties, decoded as the WAL decodes a property map: JSON
    /// other than an object is an error.
    fn decode(&self, i: usize) -> Result<BTreeMap<String, Value>> {
        let Some(text) = self.text(i) else {
            return Ok(BTreeMap::new());
        };
        let json: serde_json::Value =
            serde_json::from_str(text).map_err(|e| Error::storage(e.to_string()))?;
        props_from_json(&json)
    }
}

pub(crate) fn load_nodes(
    graph: &dyn relate_engine_api::GraphImport,
    bytes: Vec<u8>,
    origin: Origin,
) -> Result<()> {
    load_batches(graph, bytes, Kind::Node, &origin, wide_node_rows)
}

/// One node batch (ADR-0534): the reserved head, one typed column per
/// promoted key, and the residual tagged-JSON tail — a monolith the same
/// walk with no columns. The kind's labels feed the shared row tail;
/// a manifest part refuses a null label list or item, only a pre-shard
/// monolith's writer is tolerated (fail-closed for everything else).
pub(super) fn wide_node_rows(
    graph: &dyn relate_engine_api::GraphImport,
    origin: &Origin,
    batch: &RecordBatch,
    ids: &Int64Array,
    decoded: &Decoded,
    columns: &[(String, ColumnType)],
) -> Result<()> {
    let head = wide::reserved_width(Kind::Node);
    if batch.num_columns() != head + columns.len() + 1 {
        return Err(Error::storage("node shard: wide column layout"));
    }
    let labels = batch
        .column(1)
        .as_any()
        .downcast_ref::<ListArray>()
        .ok_or_else(|| Error::storage("labels column"))?;
    let values: Vec<wide::ColumnValues<'_>> = columns
        .iter()
        .enumerate()
        .map(|(j, (_, ty))| wide::ColumnValues::column(batch, head + j, *ty))
        .collect::<Result<_>>()?;
    let props = Properties::column(batch, head + columns.len())?;
    let phrases = Kind::Node.phrases();
    let mut nodes = Staged::new(decoded.slice_bytes, |nodes: &[Node]| {
        graph.load_checkpoint_nodes(nodes)
    });
    for i in 0..batch.num_rows() {
        let mut row_bytes = encode::fixed_column_bytes(Kind::Node);
        let mut row_labels = Vec::new();
        if !labels.is_null(i) {
            let arr = labels.value(i);
            let items = arr
                .as_any()
                .downcast_ref::<StringArray>()
                .ok_or_else(|| Error::storage("label items"))?;
            for j in 0..items.len() {
                if !items.is_null(j) {
                    let label = items.value(j);
                    decoded.count_row_bytes(
                        &mut row_bytes,
                        label.len() as u64,
                        phrases.row_bytes,
                    )?;
                    row_labels.push(label.to_string());
                } else {
                    origin.tolerated::<()>("node shard: null label", ())?;
                }
            }
        } else {
            origin.tolerated::<()>("node shard: null labels", ())?;
        }
        let row = RowColumns {
            i,
            columns,
            values: &values,
            props: &props,
        };
        let (properties, row_bytes) = row.properties(Kind::Node, decoded, row_bytes)?;
        let node = Node {
            id: ids.value(i),
            labels: std::sync::Arc::new(row_labels),
            properties: relate_core::PropertyMap::share(properties),
        };
        nodes.push(node, row_bytes)?;
    }
    nodes.flush()
}

pub(crate) fn load_rels(
    graph: &dyn relate_engine_api::GraphImport,
    bytes: Vec<u8>,
    origin: Origin,
) -> Result<()> {
    load_batches(graph, bytes, Kind::Rel, &origin, wide_rel_rows)
}

/// One relationship batch (ADR-0534), the sibling of [`wide_node_rows`]:
/// the reserved head — ids, endpoints, the type — one typed column per
/// promoted key, the residual tail. A manifest part refuses a null type
/// slot, only a pre-shard monolith's writer is tolerated.
fn wide_rel_rows(
    graph: &dyn relate_engine_api::GraphImport,
    origin: &Origin,
    batch: &RecordBatch,
    ids: &Int64Array,
    decoded: &Decoded,
    columns: &[(String, ColumnType)],
) -> Result<()> {
    let head = wide::reserved_width(Kind::Rel);
    if batch.num_columns() != head + columns.len() + 1 {
        return Err(Error::storage("relationship shard: wide column layout"));
    }
    let starts = batch
        .column(1)
        .as_any()
        .downcast_ref::<Int64Array>()
        .ok_or_else(|| Error::storage("start_id column"))?;
    let ends = batch
        .column(2)
        .as_any()
        .downcast_ref::<Int64Array>()
        .ok_or_else(|| Error::storage("end_id column"))?;
    let types = batch
        .column(3)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| Error::storage("type column"))?;
    let values: Vec<wide::ColumnValues<'_>> = columns
        .iter()
        .enumerate()
        .map(|(j, (_, ty))| wide::ColumnValues::column(batch, head + j, *ty))
        .collect::<Result<_>>()?;
    let props = Properties::column(batch, head + columns.len())?;
    let phrases = Kind::Rel.phrases();
    let mut rels = Staged::new(decoded.slice_bytes, |rels: &[Relationship]| {
        graph.load_checkpoint_rels(rels)
    });
    for i in 0..batch.num_rows() {
        let mut row_bytes = encode::fixed_column_bytes(Kind::Rel);
        let rel_type = if types.is_null(i) {
            origin.tolerated("relationship shard: null type", String::new())?
        } else {
            let rel_type = types.value(i);
            decoded.count_row_bytes(&mut row_bytes, rel_type.len() as u64, phrases.row_bytes)?;
            rel_type.to_string()
        };
        let row = RowColumns {
            i,
            columns,
            values: &values,
            props: &props,
        };
        // The bulk installer checks the id and both endpoints, so the
        // decode does not re-validate them.
        let (properties, row_bytes) = row.properties(Kind::Rel, decoded, row_bytes)?;
        let rel = Relationship {
            id: ids.value(i),
            start: starts.value(i),
            end: ends.value(i),
            rel_type,
            properties: relate_core::PropertyMap::share(properties),
        };
        rels.push(rel, row_bytes)?;
    }
    rels.flush()
}

/// One decode batch's rows on their way to the bulk installer, installed
/// a slice at a time: once the staged rows reach the codec's row ceiling
/// ([`INSTALL_SLICE_ROWS`]) or their decoded bytes reach the byte bound,
/// they install and the slice starts over. A batch that fails mid-way
/// keeps the slices it already installed, as a file that fails keeps its
/// earlier batches; a failed open discards the graph.
struct Staged<T, I> {
    rows: Vec<T>,
    bytes: u64,
    slice_bytes: u64,
    install: I,
}

impl<T, I: Fn(&[T]) -> Result<()>> Staged<T, I> {
    /// An empty run installing through `install`, a slice at most
    /// `slice_bytes` decoded bytes.
    fn new(slice_bytes: u64, install: I) -> Self {
        Self {
            rows: Vec::new(),
            bytes: 0,
            slice_bytes,
            install,
        }
    }

    /// Stage `row` of `bytes` decoded bytes, installing the slice once it
    /// reaches either bound.
    fn push(&mut self, row: T, bytes: u64) -> Result<()> {
        self.rows.push(row);
        self.bytes = self.bytes.saturating_add(bytes);
        if self.rows.len() >= INSTALL_SLICE_ROWS || self.bytes >= self.slice_bytes {
            self.flush()?;
        }
        Ok(())
    }

    /// Install whatever is staged.
    fn flush(&mut self) -> Result<()> {
        if self.rows.is_empty() {
            return Ok(());
        }
        (self.install)(&self.rows)?;
        self.rows.clear();
        self.bytes = 0;
        Ok(())
    }
}

/// One row's column side: everything the shared row tail reads — the
/// row's index in its batch, the promoted columns with their decoded
/// values, and the residual text column.
struct RowColumns<'a> {
    i: usize,
    columns: &'a [(String, ColumnType)],
    values: &'a [wide::ColumnValues<'a>],
    props: &'a Properties<'a>,
}

impl RowColumns<'_> {
    /// The per-row tail both decoders share (the read-side counterpart of
    /// the batch builders' shared loop): the row-envelope accounting over
    /// the columns' values — the retained key beside its value, the same
    /// bytes the legacy column counted inside its text — and the residual
    /// text, the decoded-row check, and the residual decode with the
    /// column overlay, returning the row's properties and its decoded
    /// bytes. The row id is checked by the bulk installer, not here.
    fn properties(
        &self,
        kind: Kind,
        decoded: &Decoded,
        mut row_bytes: u64,
    ) -> Result<(BTreeMap<String, Value>, u64)> {
        let phrases = kind.phrases();
        for ((key, _), value) in self.columns.iter().zip(self.values) {
            // A null slot retains neither the key nor its value; a carried
            // one retains both, and the envelope counts both.
            if value.carries(self.i) {
                decoded.count_row_bytes(&mut row_bytes, key.len() as u64, phrases.row_bytes)?;
            }
            decoded.count_row_bytes(&mut row_bytes, value.bytes(self.i), phrases.row_bytes)?;
        }
        if let Some(text) = self.props.text(self.i) {
            decoded.count_row_bytes(&mut row_bytes, text.len() as u64, phrases.row_bytes)?;
        }
        decoded.row(row_bytes)?;
        let mut properties = self.props.decode(self.i)?;
        wide::overlay_values(self.values, &mut properties, self.columns, self.i);
        Ok((properties, row_bytes))
    }
}

#[cfg(test)]
mod tests;
