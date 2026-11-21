//! Supernode mini-pages with zone-map headers: the paging format the sealed
//! codec can engage for supernode chunks — a chunk split into fixed
//! hop-count pages, every page leading with a zone-map header (ts min/max,
//! hop count, tombstoned count, first/last rel id, and the width table: the
//! bit widths the page's packed sections are written at, hoisted out of the
//! stream so a reader can walk or skip a page without decoding it).
//!
//! Not engaged anywhere, by the page-size measurements the feature took: no
//! paging pays on the measured corpora — 86.8% of SF0.1 chunks are 1 hop
//! (median 1, mean 2.66), every consumer materializes the whole chunk,
//! full-chunk decode of sealed chunks already runs at 4.1–4.6 ns/hop — and
//! the SF corpora carry zero ts, so no ranged read exists to prune for. The
//! live sealed stream keeps its ADR-0599 headerless form (a per-chunk header
//! with no reader would only re-grow the fixed cost that node removed); the
//! format, its encoder and its zone-map reader live here, proven by tests,
//! until a page size or a ts-bearing corpus makes paging pay. The module is
//! test-gated for the same reason — no call site splits, so it compiles only
//! where its proofs run; widen the gate when a consumer appears.

use std::collections::HashSet;

use relate_core::RelId;

use super::codec::{self, WidthReader, WidthWriter};
use super::{hop_count, Hop, Tombstone};

/// The page header's width table as a reader: the widths come from the
/// header, so the payload's sections carry none — `take` consumes no byte.
struct TableReader<'a> {
    widths: std::slice::Iter<'a, u8>,
}

impl WidthReader for TableReader<'_> {
    fn take(&mut self, _rest: &[u8]) -> (u32, usize) {
        (
            *self.widths.next().expect("pages: width table short") as u32,
            0,
        )
    }
}

/// The page header's width table as a writer: each section's width is
/// collected here, the payload getting only the packed bytes.
struct TableWriter<'a> {
    widths: &'a mut Vec<u8>,
}

impl WidthWriter for TableWriter<'_> {
    fn put(&mut self, width: u32, packed: &[u8], out: &mut Vec<u8>) {
        self.widths.push(width as u8);
        out.extend_from_slice(packed);
    }
}

/// One page's zone-map header: the summary a ranged read prunes on, plus
/// the width table that makes the page skippable without decoding it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ZoneMap {
    /// Lowest present ts in the page; with [`Self::ts_max`], the `(1, 0)`
    /// empty interval when the page carries no ts at all.
    pub ts_min: i64,
    /// Highest present ts in the page — the stream is ts-DESC, so this is
    /// the page's first present ts.
    pub ts_max: i64,
    /// Hops in the page.
    pub hops: u32,
    /// The page's hops whose rel id carries a tombstone — an upper bound:
    /// a tombstone hides a hop per snapshot version, and one can reference
    /// an id the chunk no longer holds.
    pub tombstoned: u32,
    /// The page's first hop's rel id.
    pub first_id: RelId,
    /// The page's last hop's rel id.
    pub last_id: RelId,
    /// The width table: one bit width per packed section, ts-gap sections
    /// first, then id-delta sections. The payload's sections carry no
    /// inline width byte — this table is the one place the widths live.
    pub widths: Vec<u8>,
}

impl ZoneMap {
    /// Whether the page's ts interval intersects `[lo, hi]` (inclusive). A
    /// page with no present ts encodes the empty interval `(1, 0)` and
    /// overlaps no range.
    pub(crate) fn overlaps(&self, lo: i64, hi: i64) -> bool {
        self.ts_min <= self.ts_max && lo <= self.ts_max && hi >= self.ts_min
    }
}

/// Encode `hops` — already in `cmp_ts_desc` order — as one block of
/// `page_size`-hop pages; `None` (or a size at or above the hop count)
/// leaves the whole chunk as one page whose header is the block header.
/// `tombstones` feed each header's tombstoned count.
pub(crate) fn encode_block(
    hops: &[Hop],
    tombstones: &[Tombstone],
    page_size: Option<usize>,
) -> Vec<u8> {
    debug_assert!(
        page_size.is_none_or(|s| s > 0),
        "page size is at least one hop"
    );
    let tombstoned: HashSet<&RelId> = tombstones.iter().map(|t| &t.id).collect();
    let size = page_size.unwrap_or(usize::MAX);
    let mut out = Vec::new();
    for page in hops.chunks(size) {
        encode_page(page, &tombstoned, &mut out);
    }
    out
}

/// Encode one page: zone-map header, then the payload — the presence
/// bitmap, the ts stream and the id stream exactly as the sealed codec
/// lays them out, except the per-section width bytes, which go to the
/// header's width table instead of the stream.
fn encode_page(hops: &[Hop], tombstoned: &HashSet<&RelId>, out: &mut Vec<u8>) {
    // The zone map: the stream is ts-DESC, so the first present ts is the
    // max and the last the min; a page with no present ts encodes the
    // empty interval (1, 0), which overlaps no range.
    let present: Vec<i64> = hops.iter().filter_map(|h| h.ts).collect();
    let (ts_min, ts_max) = match (present.first(), present.last()) {
        (Some(&first), Some(&last)) => (last, first),
        _ => (1, 0),
    };
    // A page is a non-empty chunk of `encode_block`'s hops — the invariant
    // the first/last ids rest on.
    let first_id = hops.first().expect("a page holds at least one hop").id;
    let last_id = hops.last().expect("a page holds at least one hop").id;
    let tombstoned_count = hops.iter().filter(|h| tombstoned.contains(&h.id)).count();

    // The payload — the bitmap, the ts stream and the id stream — is the
    // sealed codec's, with the per-section widths hoisted into the header's
    // table instead of leading the stream.
    let mut widths: Vec<u8> = Vec::new();
    let mut payload = Vec::new();
    codec::encode_stream(
        hops,
        &mut payload,
        &mut TableWriter {
            widths: &mut widths,
        },
    );
    assert!(
        widths.len() <= u8::MAX as usize,
        "a page holds at most 255 packed sections; split into smaller pages"
    );

    out.extend_from_slice(&ts_min.to_le_bytes());
    out.extend_from_slice(&ts_max.to_le_bytes());
    out.extend_from_slice(&hop_count(hops.len()).to_le_bytes());
    out.extend_from_slice(&(tombstoned_count as u32).to_le_bytes());
    out.extend_from_slice(&first_id.to_le_bytes());
    out.extend_from_slice(&last_id.to_le_bytes());
    out.push(widths.len() as u8);
    out.extend_from_slice(&widths);
    out.extend_from_slice(&payload);
}

/// Cursor over a block's bytes while its header is parsed.
struct HeaderReader<'a> {
    rest: &'a [u8],
}

impl HeaderReader<'_> {
    fn take(&mut self, n: usize) -> &[u8] {
        let (head, tail) = self.rest.split_at(n);
        self.rest = tail;
        head
    }
}

fn le_i64(rd: &mut HeaderReader) -> i64 {
    i64::from_le_bytes(rd.take(8).try_into().expect("pages: truncated header"))
}

fn le_u32(rd: &mut HeaderReader) -> u32 {
    u32::from_le_bytes(rd.take(4).try_into().expect("pages: truncated header"))
}

/// Parse the next page's zone-map header, returning it with the page's
/// payload slice and the bytes after the page; `None` at the block's end.
fn next_page(bytes: &[u8]) -> Option<(ZoneMap, &[u8], &[u8])> {
    if bytes.is_empty() {
        return None;
    }
    let mut rd = HeaderReader { rest: bytes };
    let ts_min = le_i64(&mut rd);
    let ts_max = le_i64(&mut rd);
    let hops = le_u32(&mut rd);
    let tombstoned = le_u32(&mut rd);
    let first_id = le_i64(&mut rd);
    let last_id = le_i64(&mut rd);
    let width_count = rd.take(1)[0] as usize;
    let widths = rd.take(width_count).to_vec();
    let map = ZoneMap {
        ts_min,
        ts_max,
        hops,
        tombstoned,
        first_id,
        last_id,
        widths,
    };
    let payload_len = page_payload_len(&map, rd.rest);
    let (payload, tail) = rd.rest.split_at(payload_len);
    Some((map, payload, tail))
}

/// The byte length of a page's payload — its bitmap, raw heads, and packed
/// sections — so a reader can skip a page without decoding it. The widths
/// come from the header's table; the present count, from the bitmap (the
/// one payload piece a skip is allowed to read).
fn page_payload_len(map: &ZoneMap, bytes: &[u8]) -> usize {
    let mut reader = TableReader {
        widths: map.widths.iter(),
    };
    codec::stream_len_with(bytes, map.hops as usize, &mut reader)
}

/// Decode one page's payload — the bitmap, then the ts and id streams —
/// using the header's width table for the packed sections.
fn decode_page(map: &ZoneMap, payload: &[u8]) -> Vec<Hop> {
    let mut reader = TableReader {
        widths: map.widths.iter(),
    };
    codec::decode_stream(payload, map.hops as usize, &mut reader)
}

/// Decode a block's every page.
pub(crate) fn decode_block(bytes: &[u8]) -> Vec<Hop> {
    let mut hops = Vec::new();
    let mut rest = bytes;
    while let Some((map, payload, tail)) = next_page(rest) {
        hops.extend(decode_page(&map, payload));
        rest = tail;
    }
    hops
}

/// Decode only the pages whose zone map overlaps `[lo, hi]`, returning
/// their hops and how many pages were decoded — the count is the pruning
/// proof. Page-granular: a mixed page's absent-ts hops ride along, and a
/// page with no present ts decodes for no range.
pub(crate) fn decode_ranged(bytes: &[u8], lo: i64, hi: i64) -> (Vec<Hop>, usize) {
    let mut hops = Vec::new();
    let mut decoded = 0;
    let mut rest = bytes;
    while let Some((map, payload, tail)) = next_page(rest) {
        if map.overlaps(lo, hi) {
            hops.extend(decode_page(&map, payload));
            decoded += 1;
        }
        rest = tail;
    }
    (hops, decoded)
}

/// A block's zone-map headers, read without decoding any payload.
pub(crate) fn zone_maps(bytes: &[u8]) -> Vec<ZoneMap> {
    let mut maps = Vec::new();
    let mut rest = bytes;
    while let Some((map, _, tail)) = next_page(rest) {
        maps.push(map);
        rest = tail;
    }
    maps
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` hops, all ts present, descending by 5; ids ascending so the id
    /// deltas are a constant +1 (zigzag 2, width 2) and the ts gaps a
    /// constant 5 (width 3) — widths the tests can name exactly.
    fn regular(n: i64) -> Vec<Hop> {
        (0..n)
            .map(|i| Hop {
                ts: Some(1500 - 5 * i),
                id: i,
            })
            .collect()
    }

    fn tombstones_of(ids: &[RelId]) -> Vec<Tombstone> {
        ids.iter()
            .map(|&id| Tombstone {
                id,
                xmax: 10,
                deleted: Some(10),
            })
            .collect()
    }

    #[test]
    fn zone_map_headers_carry_the_named_fields() {
        let hops = regular(300);
        let tombstones = tombstones_of(&[5, 105, 107, 9999]);
        let block = encode_block(&hops, &tombstones, Some(100));
        let maps = zone_maps(&block);
        assert_eq!(maps.len(), 3);
        for (p, map) in maps.iter().enumerate() {
            assert_eq!(map.hops, 100);
            assert_eq!(map.ts_max, 1500 - 5 * (100 * p) as i64);
            assert_eq!(map.ts_min, 1500 - 5 * (100 * p + 99) as i64);
            assert_eq!(map.first_id, (100 * p) as i64);
            assert_eq!(map.last_id, (100 * p + 99) as i64);
            // One ts-gap section (99 gaps < TS_BLOCK) and two id-delta
            // sections (99 deltas = 64 + 43), all gap-5 / delta-+1 widths.
            assert_eq!(map.widths, vec![3, 2, 2]);
        }
        assert_eq!(maps[0].tombstoned, 1); // id 5
        assert_eq!(maps[1].tombstoned, 2); // ids 105, 107
        assert_eq!(maps[2].tombstoned, 0); // 9999 is not a hop of this chunk

        // The unpaged block: one page whose header is the block header.
        let single = zone_maps(&encode_block(&hops, &tombstones, None));
        assert_eq!(single.len(), 1);
        assert_eq!(single[0].hops, 300);
        assert_eq!(single[0].ts_min, 1500 - 5 * 299);
        assert_eq!(single[0].ts_max, 1500);
        assert_eq!(single[0].first_id, 0);
        assert_eq!(single[0].last_id, 299);
        assert_eq!(single[0].tombstoned, 3);
        // 299 ts gaps = 128 + 128 + 43 sections; 299 id deltas = 64*4 + 43.
        assert_eq!(single[0].widths, vec![3, 3, 3, 2, 2, 2, 2, 2]);
    }

    #[test]
    fn ranged_read_decodes_only_overlapping_pages() {
        // Synthetic ts — the SF corpora carry none. Three pages with
        // disjoint ts intervals: 1005..=1500, 505..=1000, 5..=500.
        let hops = regular(300);
        let block = encode_block(&hops, &[], Some(100));

        // Only the middle page overlaps [850, 860].
        let (hit, decoded) = decode_ranged(&block, 850, 860);
        assert_eq!(decoded, 1);
        assert_eq!(hit, hops[100..200].to_vec());

        // A range spanning every page decodes all three.
        let (hit, decoded) = decode_ranged(&block, 5, 1500);
        assert_eq!(decoded, 3);
        assert_eq!(hit, hops);

        // A page with no present ts overlaps nothing — not even the full
        // i64 range.
        let absent: Vec<Hop> = (0..50).map(|i| Hop { ts: None, id: i }).collect();
        let absent_block = encode_block(&absent, &[], Some(10));
        let (hit, decoded) = decode_ranged(&absent_block, i64::MIN, i64::MAX);
        assert_eq!(decoded, 0);
        assert!(hit.is_empty());

        // A mixed page is page-granular: its absent-prefix hops ride along.
        let mut mixed: Vec<Hop> = (0..80)
            .map(|i| Hop {
                ts: (i >= 20).then_some(200 - i),
                id: i,
            })
            .collect();
        mixed.sort_unstable_by(crate::adjacency::cmp_ts_desc);
        let mixed_block = encode_block(&mixed, &[], Some(40));
        // Page 0: 20 absent hops + the 20 highest ts (161..=180).
        let (hit, decoded) = decode_ranged(&mixed_block, 165, 175);
        assert_eq!(decoded, 1);
        assert_eq!(hit, mixed[0..40].to_vec());
    }

    #[test]
    fn paged_blocks_decode_byte_identical_to_the_unpaged_encoding() {
        // Missing-ts first, then ts DESC with equal-ts ties newest-first —
        // the chunk shapes the store holds, including block tails.
        let mut ordered: Vec<Hop> = (0..300)
            .map(|i| Hop {
                ts: (i % 4 != 0).then_some(1000 - i),
                id: i64::MIN + i,
            })
            .collect();
        ordered.sort_unstable_by(crate::adjacency::cmp_ts_desc);
        let tombstones = tombstones_of(&[i64::MIN, i64::MIN + 4]);
        let unpaged = codec::encode_chunk(&ordered);
        for size in [
            None,
            Some(1usize),
            Some(7),
            Some(63),
            Some(64),
            Some(65),
            Some(128),
            Some(299),
            Some(usize::MAX),
        ] {
            let block = encode_block(&ordered, &tombstones, size);
            let decoded = decode_block(&block);
            assert_eq!(decoded, ordered, "page size {size:?}");
            // Byte-identical: the paged block's hops re-encode to exactly
            // the unpaged encoding of the same hops.
            assert_eq!(codec::encode_chunk(&decoded), unpaged, "page size {size:?}");
        }
        assert!(decode_block(&encode_block(&[], &[], None)).is_empty());
    }
}
