//! Parquet row codec for id-window shards (ADR-0022).

mod budget;
mod copy;
mod encode;
mod envelope;
#[cfg(test)]
mod fixtures;
mod load;
mod rules;
mod span;
mod wide;
use envelope::invalid;
pub(crate) use envelope::{preflight, VfsChunkReader};
pub(crate) use load::{load_nodes, load_rels, Loader, Origin};
use load::{Decoded, RowLoader};
use rules::{IdWindow, ShardRules};
pub(crate) use span::SpanReaders;

#[cfg(test)]
use crate::codec::with_test_override;

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

use relate_core::{Error, Result};

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

/// Rows held and flushed together (ADR-0180). 1024 since ADR-0563: at 64
/// rows the wide encoding's per-leaf, per-group page work multiplied part
/// files 8-10x — the dominant term of its checkpoint write tax.
pub(crate) const ROW_GROUP_CAP: usize = 1024;

/// Rows one decode batch reads: the copy path's reader batch size, and
/// the public bound on decode batches (`docs/api/rust/spec.md`).
pub(crate) const DECODE_BATCH_ROWS: usize = 1024;

/// A row group must not outgrow a decode batch, or a copied batch would
/// exceed the public decode bound. Raising the cap past it is a contract
/// change, not a constant edit.
const _: () = assert!(ROW_GROUP_CAP <= DECODE_BATCH_ROWS);

/// Pages a row group writes per leaf column while its values fit one data
/// page: a dictionary page and a data page.
const PAGES_PER_LEAF: u64 = 2;

impl Kind {
    /// The most rows of this kind one file holds within ADR-0086's
    /// `MAX_PAGES` (ADR-0183, Page envelope): row groups of `ROW_GROUP_CAP`
    /// rows, each writing `PAGES_PER_LEAF` pages per leaf column. Every
    /// written part is wide (ADR-0553): the leaf count adds at most
    /// [`MAX_PROMOTED_COLUMNS`] promoted columns and the residual
    /// (ADR-0534) — 958,464 node rows and 906,240 relationship rows at
    /// the cap (ADR-0563). A file holds fewer when its part promotes
    /// fewer columns or a large value spills into more pages.
    pub(crate) fn page_envelope_rows(self) -> usize {
        #[cfg(test)]
        if let Some(rows) = TEST_PAGE_ENVELOPE_ROWS.get() {
            return rows;
        }
        let leaves = self.leaf_columns() + MAX_PROMOTED_COLUMNS as u64;
        let groups = MAX_PAGES / (PAGES_PER_LEAF * leaves);
        groups as usize * ROW_GROUP_CAP
    }

    /// The row noun errors name: `node` or `relationship`.
    fn noun(self) -> &'static str {
        self.phrases().noun
    }

    /// The kind's error phrases — the one table the per-kind error
    /// vocabulary comes from (the row noun, the row-envelope phrase), so
    /// no decoder hand-spells a sibling's.
    fn phrases(self) -> KindPhrases {
        match self {
            Self::Node => KindPhrases {
                noun: "node",
                row_bytes: "node row bytes",
            },
            Self::Rel => KindPhrases {
                noun: "relationship",
                row_bytes: "relationship row bytes",
            },
        }
    }
}

/// The per-kind error phrases [`Kind::phrases`] hands out.
#[derive(Clone, Copy)]
struct KindPhrases {
    noun: &'static str,
    row_bytes: &'static str,
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

// The row cap `Kind::page_envelope_rows` reports, overridable in tests
// like `file_ceiling`. The derivation (ADR-0563: ~1M rows at the cap)
// stays pinned by `page_envelope_rows_fill_max_pages_at_the_row_group_cap`;
// the override is for tests that need a seed to cross the envelope
// without staging a million rows.
#[cfg(test)]
thread_local! {
    static TEST_PAGE_ENVELOPE_ROWS: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn with_test_page_envelope_rows<T>(rows: usize, f: impl FnOnce() -> T) -> T {
    with_test_override(&TEST_PAGE_ENVELOPE_ROWS, rows, f)
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
fn check_row(bytes: u64, limits: Envelope) -> Result<()> {
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
    check_row(bytes, envelope()).is_ok()
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
