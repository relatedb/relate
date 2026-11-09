//! Parquet row codec for id-window shards (ADR-0022).

mod budget;
mod copy;
mod encode;
mod envelope;
#[cfg(test)]
mod fixtures;
mod rules;
mod wide;
use envelope::invalid;
pub(crate) use envelope::{preflight, VfsChunkReader};
use rules::{id_column, ShardRules};

pub(crate) use budget::DecodedBudget;
#[cfg(test)]
pub(crate) use budget::{decoded_preflight, take_measured_rows};
pub(crate) use copy::{previous_shard_columns, ShardCursor};
pub(crate) use encode::{
    encode_wide_rows, with_part_shard_writer_to, OutputLimit, ShardError, ShardWriter,
};
#[cfg(test)]
pub(crate) use encode::{take_encoded_rows, with_wide_shard_writer};
#[cfg(test)]
pub(crate) use fixtures::{write_bare_table, write_int_label_table, write_rel_table, write_table};
pub(crate) use wide::{ColumnType, MAX_PROMOTED_COLUMNS};

use std::collections::BTreeMap;
use std::ops::Range;

use arrow::array::{Array, Int64Array, ListArray, StringArray};
use arrow::record_batch::RecordBatch;
use bytes::Bytes;
use relate_core::{Error, Node, Relationship, Result, Value};
use relate_engine_api::props_from_json;

use crate::snapshot::Kind;
use crate::storage_context;

// ADR-0086: these are per object, not per database. Keep the preflight ahead of
// Arrow's page reader: in parquet 54.3.1 it allocates from PageHeader before
// handing a page to a caller, even when the batch size is small.
pub(crate) const MAX_FILE: u64 = 1 << 30;
/// The one rendering of the shard file-envelope refusal (ADR-0183): the
/// message [`shard_envelope_refusal`] carries when a shard crosses
/// `MAX_FILE`, single-sourced beside the cap so the two paths cannot drift.
pub(crate) const SHARD_FILE_ENVELOPE_MESSAGE: &str = "Parquet shard exceeds 1 GiB file envelope";

/// The one construction of the shard file-envelope refusal (ADR-0183): the
/// typed channel both shard writers — the direct-to-VFS
/// `with_shard_writer_to` and the in-memory test twin `with_shard_writer` —
/// refuse through when a shard crosses `MAX_FILE`, so the twins' envelope
/// errors are identical by construction, never two spellings of the message.
pub(crate) fn shard_envelope_refusal() -> crate::vfs::Refusal {
    crate::vfs::Refusal::Fixed(Error::storage(SHARD_FILE_ENVELOPE_MESSAGE))
}
const MAX_METADATA: usize = 16 << 20;
const MAX_HEADER: usize = 64 << 10;
const MAX_PAGE: u64 = 128 << 20;
const MAX_PAGES: u64 = 65_536;
const MAX_ROWS: u64 = 1_000_000;
const MAX_ROW_BYTES: u64 = 384 << 20;
const MAX_DECODED: u64 = 2 << 30;

/// Rows held and flushed together (ADR-0180). The old 64-row prefix sample
/// used the same width; it is a row-group cap now, not a size estimate.
pub(crate) const ROW_GROUP_CAP: usize = 64;

/// Pages a row group writes per leaf column while its values fit one data
/// page: a dictionary page and a data page.
const PAGES_PER_LEAF: u64 = 2;

impl Kind {
    /// The most rows of this kind one file holds within ADR-0086's
    /// `MAX_PAGES` (ADR-0183, Page envelope): row groups of `ROW_GROUP_CAP`
    /// rows, each writing `PAGES_PER_LEAF` pages per leaf column. A legacy
    /// part's fixed schema holds 699,008 node rows and 419,392 relationship
    /// rows; a wide part's leaf count adds at most
    /// [`MAX_PROMOTED_COLUMNS`] promoted columns and the residual
    /// (ADR-0534) — 59,904 node rows and 56,640 relationship rows at the
    /// cap. A file holds fewer when its part promotes fewer columns or a
    /// large value spills into more pages.
    pub(crate) fn page_envelope_rows(self, wide: bool) -> usize {
        let leaves = self.leaf_columns() + u64::from(wide) * MAX_PROMOTED_COLUMNS as u64;
        let groups = MAX_PAGES / (PAGES_PER_LEAF * leaves);
        groups as usize * ROW_GROUP_CAP
    }

    /// The row noun errors name: `node` or `relationship`.
    fn noun(self) -> &'static str {
        self.phrases().noun
    }

    /// The kind's error phrases — the one table the per-kind error
    /// vocabulary comes from (the row noun, the row-envelope phrase, the
    /// id phrase), so no decoder hand-spells a sibling's.
    fn phrases(self) -> KindPhrases {
        match self {
            Self::Node => KindPhrases {
                noun: "node",
                row_bytes: "node row bytes",
                id: "node ID",
            },
            Self::Rel => KindPhrases {
                noun: "relationship",
                row_bytes: "relationship row bytes",
                id: "relationship ID",
            },
        }
    }
}

/// The per-kind error phrases [`Kind::phrases`] hands out.
#[derive(Clone, Copy)]
struct KindPhrases {
    noun: &'static str,
    row_bytes: &'static str,
    id: &'static str,
}

#[cfg(test)]
thread_local! {
    static TEST_FILE_CEILING: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

pub(crate) fn file_ceiling() -> u64 {
    #[cfg(test)]
    if let Some(limit) = TEST_FILE_CEILING.get() {
        return limit;
    }
    MAX_FILE
}

#[cfg(test)]
pub(crate) fn with_test_file_ceiling<T>(limit: u64, f: impl FnOnce() -> T) -> T {
    with_test_override(&TEST_FILE_CEILING, limit, f)
}

/// Run `f` with the thread-local test override `cell` set to `value`, and
/// restore the override it replaced once `f` returns or panics, so
/// overrides of one cell nest.
#[cfg(test)]
fn with_test_override<V: 'static, T>(
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

#[derive(Clone, Copy)]
struct Envelope {
    page: u64,
    row: u64,
    decoded: u64,
}

fn envelope() -> Envelope {
    #[cfg(test)]
    if let Some(limits) = TEST_ENVELOPE.get() {
        return limits;
    }
    Envelope {
        page: MAX_PAGE,
        row: MAX_ROW_BYTES,
        decoded: MAX_DECODED,
    }
}

#[cfg(test)]
thread_local! {
    static TEST_ENVELOPE: std::cell::Cell<Option<Envelope>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn with_test_envelope<T>(page: u64, row: u64, decoded: u64, f: impl FnOnce() -> T) -> T {
    with_test_override(&TEST_ENVELOPE, Envelope { page, row, decoded }, f)
}

/// ADR-0086's checks on one decoded row, shared by the load side
/// ([`Decoded::row`]) and the checkpoint side ([`DecodedBudget`]): it must
/// fit the row envelope, and the decoded envelope by itself. The error names
/// the envelope the row misses.
fn check_row(bytes: u64) -> Result<()> {
    let limits = envelope();
    if bytes > limits.row {
        return Err(invalid("384 MiB decoded row"));
    }
    if bytes > limits.decoded {
        return Err(invalid("actual decoded bytes"));
    }
    Ok(())
}

/// Whether a row passes [`check_row`].
fn row_fits(bytes: u64) -> bool {
    check_row(bytes).is_ok()
}

fn bounded(value: i64, limit: u64, what: &str) -> Result<u64> {
    let n = u64::try_from(value).map_err(|_| invalid(what))?;
    if n > limit {
        return Err(invalid(what));
    }
    Ok(n)
}

fn accumulate(sum: &mut u64, value: u64, limit: u64, what: &str) -> Result<()> {
    *sum = sum
        .checked_add(value)
        .filter(|n| *n <= limit)
        .ok_or_else(|| invalid(what))?;
    Ok(())
}

/// Read-side row-envelope accounting (ADR-0086): the row's decoded bytes
/// toward its bound — the one byte of slack so the check that follows
/// (`Decoded::row` over `check_row`) refuses a row at exactly the bound
/// instead of passing it. Every per-row decoder site charges through
/// here, never its own copy of the bound.
fn count_row_bytes(sum: &mut u64, add: u64, what: &str) -> Result<()> {
    accumulate(sum, add, envelope().row.saturating_add(1), what)
}

/// Actual rows and decoded row bytes of one loaded file, held to the
/// ADR-0086 row and decoded envelopes across all of its batches.
#[derive(Default)]
struct Decoded {
    rows: u64,
    bytes: u64,
}

impl Decoded {
    fn batch(&mut self, rows: usize) -> Result<()> {
        accumulate(&mut self.rows, rows as u64, MAX_ROWS, "actual decoded rows")
    }

    fn row(&mut self, bytes: u64) -> Result<()> {
        check_row(bytes)?;
        accumulate(
            &mut self.bytes,
            bytes,
            envelope().decoded,
            "actual decoded bytes",
        )
    }
}

/// Preflighting entry point kept for tests that assert a direct encode
/// rejects a decoded-envelope overage before allocating.
#[cfg(test)]
pub(crate) fn nodes_parquet_ids(
    graph: &dyn relate_engine_api::GraphImport,
    ts: u64,
    keys: &[i64],
) -> Result<Vec<u8>> {
    encode_ids(graph, ts, keys, Kind::Node)
}

/// Relationship sibling of [`nodes_parquet_ids`], kept for direct-encode tests.
#[cfg(test)]
pub(crate) fn rels_parquet_ids(
    graph: &dyn relate_engine_api::GraphImport,
    ts: u64,
    rids: &[i64],
) -> Result<Vec<u8>> {
    encode_ids(graph, ts, rids, Kind::Rel)
}

#[cfg(test)]
fn encode_ids(
    graph: &dyn relate_engine_api::GraphImport,
    ts: u64,
    ids: &[i64],
    kind: Kind,
) -> Result<Vec<u8>> {
    decoded_preflight(graph, ts, ids, kind)?;
    let (bytes, _) = with_wide_shard_writer(kind, &[], file_ceiling(), |writer| {
        encode_wide_rows(writer, graph, ts, kind, ids, &[])
    })?;
    Ok(bytes.into())
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

/// Read one loaded file's batches into `load` with its id column and, for
/// a manifest part, its promoted columns — read once per file from the
/// schema the part carries (ADR-0534): preflighted, counted against the
/// ADR-0086 row envelope, and for a manifest part held to [`ShardRules`],
/// which check the ids it is handed. A monolith hands no columns — the
/// fixed layout, which is the wide schema with none — so one decode
/// path serves both shapes; it alone tolerates the pre-shard writers'
/// null label and type slots.
fn load_batches(
    bytes: Vec<u8>,
    kind: Kind,
    origin: &Origin,
    mut load: impl FnMut(&RecordBatch, &Int64Array, &mut Decoded, &[(String, ColumnType)]) -> Result<()>,
) -> Result<()> {
    let builder = preflight(Bytes::from(bytes))?;
    let mut rules = match origin {
        Origin::Part(range) => Some(ShardRules::open(&builder, kind, range.clone())?),
        Origin::Monolith => None,
    };
    // A manifest part decodes its own promoted columns (ADR-0534); a
    // monolith carries none — the fixed layout, which is the wide schema
    // with no promoted columns, so one decode path serves both.
    let columns = rules
        .as_ref()
        .map(|rules| rules.columns().to_vec())
        .unwrap_or_default();
    let reader = builder.build().map_err(|e| Error::storage(e.to_string()))?;
    let mut decoded = Decoded::default();
    for batch in reader {
        let batch = batch.map_err(|e| Error::storage(e.to_string()))?;
        decoded.batch(batch.num_rows())?;
        let ids = match &mut rules {
            Some(rules) => rules.ids(&batch)?,
            None => id_column(&batch).ok_or_else(|| Error::storage("id column"))?,
        };
        load(&batch, &ids, &mut decoded, &columns)?;
    }
    Ok(())
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
    load_batches(
        bytes,
        Kind::Node,
        &origin,
        |batch, ids, decoded, columns| wide_node_rows(graph, &origin, batch, ids, decoded, columns),
    )
}

/// One node batch (ADR-0534): the reserved head, one typed column per
/// promoted key, and the residual tagged-JSON tail — a monolith the same
/// walk with no columns. The kind's labels feed the shared row tail;
/// a manifest part refuses a null label list or item, only a pre-shard
/// monolith's writer is tolerated (fail-closed for everything else).
fn wide_node_rows(
    graph: &dyn relate_engine_api::GraphImport,
    origin: &Origin,
    batch: &RecordBatch,
    ids: &Int64Array,
    decoded: &mut Decoded,
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
                    count_row_bytes(&mut row_bytes, label.len() as u64, phrases.row_bytes)?;
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
            id: ids.value(i),
            columns,
            values: &values,
            props: &props,
        };
        let properties = row.properties(Kind::Node, decoded, row_bytes)?;
        graph.load_checkpoint_node(Node {
            id: row.id,
            labels: std::sync::Arc::new(row_labels),
            properties: relate_core::PropertyMap::share(properties),
        })?;
    }
    Ok(())
}

pub(crate) fn load_rels(
    graph: &dyn relate_engine_api::GraphImport,
    bytes: Vec<u8>,
    origin: Origin,
) -> Result<()> {
    load_batches(bytes, Kind::Rel, &origin, |batch, ids, decoded, columns| {
        wide_rel_rows(graph, &origin, batch, ids, decoded, columns)
    })
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
    decoded: &mut Decoded,
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
    for i in 0..batch.num_rows() {
        let mut row_bytes = encode::fixed_column_bytes(Kind::Rel);
        let rel_type = if types.is_null(i) {
            origin.tolerated("relationship shard: null type", String::new())?
        } else {
            let rel_type = types.value(i);
            count_row_bytes(&mut row_bytes, rel_type.len() as u64, phrases.row_bytes)?;
            rel_type.to_string()
        };
        let row = RowColumns {
            i,
            id: ids.value(i),
            columns,
            values: &values,
            props: &props,
        };
        let properties = row.properties(Kind::Rel, decoded, row_bytes)?;
        let id = row.id;
        let start = starts.value(i);
        let end = ends.value(i);
        relate_core::validate_persisted_id(id, phrases.id)?;
        relate_core::validate_persisted_id(start, "relationship start ID")?;
        relate_core::validate_persisted_id(end, "relationship end ID")?;
        graph.load_checkpoint_rel(Relationship {
            id,
            start,
            end,
            rel_type,
            properties: relate_core::PropertyMap::share(properties),
        })?;
    }
    Ok(())
}

/// One row's column side: everything the shared row tail reads — the
/// promoted columns with their decoded values, the residual text column,
/// and the row's id.
struct RowColumns<'a> {
    i: usize,
    id: i64,
    columns: &'a [(String, ColumnType)],
    values: &'a [wide::ColumnValues<'a>],
    props: &'a Properties<'a>,
}

impl RowColumns<'_> {
    /// The per-row tail both decoders share (the read-side counterpart of
    /// the batch builders' shared loop): the row-envelope accounting over
    /// the columns' values — the retained key beside its value, the same
    /// bytes the legacy column counted inside its text — and the residual
    /// text, the decoded-row check, the id validation, and the residual
    /// decode with the column overlay, returning the row's properties.
    fn properties(
        &self,
        kind: Kind,
        decoded: &mut Decoded,
        mut row_bytes: u64,
    ) -> Result<BTreeMap<String, Value>> {
        let phrases = kind.phrases();
        for ((key, _), value) in self.columns.iter().zip(self.values) {
            // A null slot retains neither the key nor its value; a carried
            // one retains both, and the envelope counts both.
            if value.carries(self.i) {
                count_row_bytes(&mut row_bytes, key.len() as u64, phrases.row_bytes)?;
            }
            count_row_bytes(&mut row_bytes, value.bytes(self.i), phrases.row_bytes)?;
        }
        if let Some(text) = self.props.text(self.i) {
            count_row_bytes(&mut row_bytes, text.len() as u64, phrases.row_bytes)?;
        }
        decoded.row(row_bytes)?;
        relate_core::validate_persisted_id(self.id, phrases.id)?;
        let mut properties = self.props.decode(self.i)?;
        wide::overlay_values(self.values, &mut properties, self.columns, self.i);
        Ok(properties)
    }
}

/// The one shape of a failure to read previous shard `path`: `error`,
/// naming the shard.
pub(super) fn source_error(path: &str, error: Error) -> Error {
    storage_context(error, format_args!("previous Parquet shard {path}"))
}

/// One new part's promoted columns (ADR-0534): the unit's `carried`
/// columns plus the keys the rows it encodes — `ids`, its dirty and new
/// ids — promote and the carried set does not already hold.
pub(crate) fn part_columns(
    kind: Kind,
    graph: &dyn relate_engine_api::GraphImport,
    ts: u64,
    ids: &[i64],
    carried: &[(String, ColumnType)],
) -> Vec<(String, ColumnType)> {
    let derived = match kind {
        Kind::Node => wide::derive_columns(ids, |id| graph.node_at(id, ts).map(|n| n.properties)),
        Kind::Rel => wide::derive_columns(ids, |id| graph.rel_at(id, ts).map(|r| r.properties)),
    };
    wide::merge_columns(carried, derived)
}

#[cfg(test)]
mod tests;
