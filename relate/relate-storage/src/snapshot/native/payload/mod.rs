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
//! | `type_name` | Utf8 | the chunk's rel type name — the install interns it into its own numbering |
//! | `incoming` | Boolean | `true` for the node's incoming (end) side |
//! | `hops` | Int64 | the chunk's total hop count (sealed + delta) |
//! | `stream_bytes` | Binary | the framed hop data below |
//! | `tombstones` | List(Int64) | the hop ids a tombstone hides |
//! | `tombstone_deleted` | List(Boolean) | per `tombstones` entry: deleted (`true`) or repositioned |
//! | `crc32` | UInt32 | [`row_crc`] over the identity columns and the hop data |
//!
//! The direction flag and the tombstone kinds are the wire spelling of the
//! typed side and tombstone ([`AdjacencySide`], [`ChunkTombstone`]); the
//! install converts each column once.
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
//! the verbatim bytes to answer like a re-fold — and its kind, because only
//! the export walk knows which supersede put it there.
//!
//! At open the payload is a cache tier: [`install`] decodes its rows over
//! the checkpoint load's own span workers, cut at node-id boundaries, and
//! installs each chunk borrowed — the sealed stream straight into the
//! arena, the delta hop ids straight into their chunk. Every row's
//! [`row_crc`] verifies before its chunk lands, over the row's identity
//! columns as well as its hop data, so a bit-flipped hop id, side or hop
//! count degrades instead of installing a wrong hop. Every failure (a
//! payload naming another checkpoint, a corrupt or missing object, a rel
//! row group whose digest moved, a row that does not hash to its `crc32`)
//! degrades to the full row re-fold with a warning; it never refuses the
//! store. The row's own columns live in [`write`], the reading and
//! installing half in [`install`].

mod install;
mod write;

pub(crate) use install::{install, Installed};
pub(crate) use write::{
    is_referenced, retire, sweep, validate, validate_deletable, write, PayloadConfig,
};

// The test suites read the writer's properties and digest helpers directly.
#[cfg(test)]
pub(crate) use write::{payload_writer_properties, row_group_digests, DEFAULT_MIN_RELS};

use arrow::array::Array;
use arrow::record_batch::RecordBatch;
use parquet::file::metadata::KeyValue;
use relate_core::{Error, Result};
use relate_engine_api::{AdjacencySide, ChunkTombstone};

/// The digest algorithm named in the payload's file metadata: `crc32fast`'s
/// CRC-32, rendered as eight lowercase hex digits per row group.
pub(crate) const DIGEST_ALGORITHM: &str = "crc32";

/// The payload format's metadata version. The schema's typed side, tombstone
/// kind and integrity columns moved the layout, so a payload of an earlier
/// version is refused at [`PayloadHeader::parse`] and degrades like any other
/// stale cache tier. Version 4 keys the chunks by the rel type's name instead
/// of the writer's interned id, which a load renumbers.
const PAYLOAD_FORMAT: &str = "4";
const META_FORMAT: &str = "adjacency.payload.format";
pub(crate) const META_ALGORITHM: &str = "adjacency.digest.algorithm";
pub(crate) const META_LSN: &str = "adjacency.checkpoint_lsn";
/// The metadata key every rel row-group digest shares; the writer and the
/// verifier build it through [`digest_key`] alone.
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

/// One payload row's identity and direction columns — named, because the
/// integers and the side swap silently as positional arguments. The type is
/// its name: the interned id is per graph instance and a load renumbers it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RowKey<'a> {
    pub(crate) node_id: i64,
    pub(crate) type_name: &'a str,
    pub(crate) side: AdjacencySide,
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

/// One payload row's integrity digest: the CRC-32 of the row's identity
/// columns — node, type name (length-prefixed, so two names cannot run
/// together), side, total hop count — followed by its framed `stream_bytes`
/// and then each tombstone id with its kind, all little-endian. The install
/// recomputes it before the row's chunk lands, so a bit flip anywhere in the
/// row degrades to the re-fold instead of installing wrong data; the writer
/// computes the same value through the one function.
pub(crate) fn row_crc(
    key: RowKey<'_>,
    hops: u64,
    stream: &[u8],
    tombstones: &[ChunkTombstone],
) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&key.node_id.to_le_bytes());
    hasher.update(&(key.type_name.len() as u64).to_le_bytes());
    hasher.update(key.type_name.as_bytes());
    hasher.update(&[u8::from(key.side.is_in())]);
    hasher.update(&hops.to_le_bytes());
    hasher.update(stream);
    for tombstone in tombstones {
        hasher.update(&tombstone.id.to_le_bytes());
        hasher.update(&[u8::from(tombstone.deleted)]);
    }
    hasher.finalize()
}

/// The one decode of a batch's column `what`, typed as the payload schema
/// holds it — a payload of another shape is the install's degrade. Columns
/// are addressed by name, so the order lives in the writer's schema alone.
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

#[cfg(test)]
mod tests;
