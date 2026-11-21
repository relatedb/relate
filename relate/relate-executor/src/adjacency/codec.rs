//! The hybrid adjacency codec (ADR-0551, after ADR-0206; header dropped
//! by ADR-0599): one sealed chunk's ts-DESC hop stream as parallel
//! bit-packed streams.
//!
//! Layout: a ts-presence bitmap (1 bit per hop), the ts stream (first
//! present value raw `i64`, then non-negative gaps — ts is non-increasing
//! — in 128-value blocks), and the id stream (first id raw, then zigzag
//! deltas in 64-value blocks). The hop count is no longer a leading `u32`
//! header: it rides the arena's 12-byte entry, so the stream the entry's
//! `off` points at starts with the bitmap (and the first raw id leads the
//! id stream, as before). Decoding re-emits hops in the original order, so
//! newest-first iteration is byte-identical.
//!
//! The paged format ([`super::pages`]) shares this section walk: it lays
//! the same bitmap, ts and id streams down, but hoists each packed
//! section's bit width into its page header's table instead of leading the
//! stream with it. The width seam ([`WidthReader`] / [`WidthWriter`]) is
//! the one point the two formats differ; [`encode_stream`],
//! [`stream_len_with`] and [`decode_stream`] are written once over it.
//!
//! Scalar only: ADR-0206 measured the NEON block decoder at 0.72–1.07x the
//! scalar reader, and the experiment copy under
//! `testing/relate-datasets/src/codec` stays pinned as its reference.

use relate_core::RelId;

use super::bitpack::{bit_width, unzigzag, zigzag, BitReader, BitWriter};
use super::Hop;

/// ts-gap block size.
pub(crate) const TS_BLOCK: usize = 128;
/// zigzag-delta block size for the id stream.
pub(crate) const ID_BLOCK: usize = 64;

/// Where a stream's packed sections read their bit widths from: the chunk
/// codec keeps each width inline ahead of its block, the paged format
/// hoists them into its page header's table. The one seam the shared
/// section walk is parameterized by.
pub(crate) trait WidthReader {
    /// The next section's width from the stream at `rest`, with the number
    /// of bytes the width itself occupies there — `1` inline, `0` from a
    /// table.
    fn take(&mut self, rest: &[u8]) -> (u32, usize);
}

/// Where a stream's packed sections write their bit widths to — inline
/// ahead of the block, or collected into a table. The write half of the
/// width seam.
pub(crate) trait WidthWriter {
    /// Append one packed section to `out`: the width, then the packed bytes.
    fn put(&mut self, width: u32, packed: &[u8], out: &mut Vec<u8>);
}

/// The chunk codec's inline widths: each section leads with its width byte.
struct Inline;

impl WidthReader for Inline {
    fn take(&mut self, rest: &[u8]) -> (u32, usize) {
        (rest[0] as u32, 1)
    }
}

impl WidthWriter for Inline {
    fn put(&mut self, width: u32, packed: &[u8], out: &mut Vec<u8>) {
        out.push(width as u8);
        out.extend_from_slice(packed);
    }
}

/// Encode one chunk's hop stream, without the leading hop-count header.
/// `hops` must already be in `cmp_ts_desc` order.
pub(crate) fn encode_chunk(hops: &[Hop]) -> Vec<u8> {
    let mut out = Vec::new();
    encode_stream(hops, &mut out, &mut Inline);
    out
}

/// Encode `hops`' ts-DESC stream — the presence bitmap, the ts stream and
/// the id stream — appending the packed sections through `widths`. The one
/// encoder the chunk codec (inline widths) and the paged format (widths
/// hoisted into the page header's table) route through.
pub(crate) fn encode_stream(hops: &[Hop], out: &mut Vec<u8>, widths: &mut impl WidthWriter) {
    // (a) ts-presence bitmap.
    let mut bitmap = vec![0u8; hops.len().div_ceil(8)];
    for (i, hop) in hops.iter().enumerate() {
        if hop.ts.is_some() {
            bitmap[i / 8] |= 1 << (i % 8);
        }
    }
    out.extend_from_slice(&bitmap);

    // (b) ts stream: first value raw, gaps bit-packed per block.
    let present: Vec<i64> = hops.iter().filter_map(|h| h.ts).collect();
    if let Some((&first, rest)) = present.split_first() {
        out.extend_from_slice(&first.to_le_bytes());
        let mut gaps = Vec::with_capacity(rest.len());
        let mut prev = first;
        for &ts in rest {
            // ts is non-increasing, so the gap is non-negative; wrapping
            // subtraction keeps an i64::MAX..i64::MIN span exact in u64.
            gaps.push(prev.wrapping_sub(ts) as u64);
            prev = ts;
        }
        for block in gaps.chunks(TS_BLOCK) {
            let width = bit_width(
                block
                    .iter()
                    .copied()
                    .max()
                    .expect("chunks() yields non-empty blocks"),
            );
            let mut w = BitWriter::new();
            for &gap in block {
                w.write(gap, width);
            }
            widths.put(width, &w.finish(), out);
        }
    }

    // (c) id stream: first id raw, zigzag deltas bit-packed per block.
    if let Some((&head, tail)) = hops.split_first() {
        out.extend_from_slice(&(head.id as u64).to_le_bytes());
        let mut deltas = Vec::with_capacity(tail.len());
        let mut prev = head.id;
        for hop in tail {
            deltas.push(zigzag(hop.id.wrapping_sub(prev)));
            prev = hop.id;
        }
        for block in deltas.chunks(ID_BLOCK) {
            let width = bit_width(
                block
                    .iter()
                    .copied()
                    .max()
                    .expect("chunks() yields non-empty blocks"),
            );
            let mut w = BitWriter::new();
            for &delta in block {
                w.write(delta, width);
            }
            widths.put(width, &w.finish(), out);
        }
    }
}

/// The encoded byte length of a chunk stream for `n` hops, starting at the
/// stream head: the bytes [`encode_chunk`] produced for the same hops. Walks
/// the block structure without decoding, so the arena can find the end of a
/// chunk's slice without an allocation.
pub(crate) fn stream_len(bytes: &[u8], n: usize) -> usize {
    stream_len_with(bytes, n, &mut Inline)
}

/// The byte length of one stream for `n` hops without decoding it, reading
/// each packed section's width through `widths` (inline for the codec, the
/// page header's table for the paged format).
pub(crate) fn stream_len_with(bytes: &[u8], n: usize, widths: &mut impl WidthReader) -> usize {
    let bitmap_len = n.div_ceil(8);
    let present: usize = bytes[..bitmap_len]
        .iter()
        .map(|b| b.count_ones() as usize)
        .sum();
    let mut rest = &bytes[bitmap_len..];
    let mut consumed = bitmap_len;
    if present > 0 {
        consumed += 8;
        rest = &rest[8..];
        consumed += section_len(&mut rest, present - 1, TS_BLOCK, widths);
    }
    if n > 0 {
        consumed += 8;
        rest = &rest[8..];
        consumed += section_len(&mut rest, n - 1, ID_BLOCK, widths);
    }
    consumed
}

/// Walk one stream's packed section — `count` values in blocks of `block` —
/// advancing `rest` past it and returning the bytes it occupies. Each
/// block's width comes from `widths`, its packed length from the width and
/// the block's value count.
fn section_len(
    rest: &mut &[u8],
    count: usize,
    block: usize,
    widths: &mut impl WidthReader,
) -> usize {
    let mut consumed = 0;
    let mut remaining = count;
    while remaining > 0 {
        let take = remaining.min(block);
        let (width, skip) = widths.take(rest);
        let packed_len = (take * width as usize).div_ceil(8);
        consumed += skip + packed_len;
        *rest = &rest[skip + packed_len..];
        remaining -= take;
    }
    consumed
}

/// Whether `bytes` can hold an `n`-hop stream: the structural minimum
/// [`decode_stream`] reads — the presence bitmap, the id stream's raw head
/// and one width byte per id block. The open-time payload install checks
/// bytes it never encoded with this, so a short stream degrades instead of
/// panicking the first read.
pub(crate) fn stream_fits(bytes: &[u8], n: usize) -> bool {
    let bitmap_len = n.div_ceil(8);
    let id_blocks = n.saturating_sub(1).div_ceil(ID_BLOCK);
    let head = if n > 0 { 8 + id_blocks } else { 0 };
    bytes.len() >= bitmap_len + head
}

/// Decode one chunk encoded by [`encode_chunk`], whose hop count `n` rides
/// the arena entry; the hop stream comes back in the original chunk order.
pub(crate) fn decode_chunk(bytes: &[u8], n: usize) -> Vec<Hop> {
    decode_stream(bytes, n, &mut Inline)
}

/// Decode one stream of `n` hops, reading each packed section's width
/// through `widths` (inline for the codec, the page header's table for the
/// paged format). The one decoder both formats route through.
pub(crate) fn decode_stream(bytes: &[u8], n: usize, widths: &mut impl WidthReader) -> Vec<Hop> {
    let bitmap_len = n.div_ceil(8);
    let (bitmap, mut rest) = bytes.split_at(bitmap_len);

    // (a) ts stream.
    let present = bitmap.iter().map(|b| b.count_ones() as usize).sum();
    let mut tss: Vec<i64> = Vec::with_capacity(present);
    if present > 0 {
        let (raw, tail) = rest.split_at(8);
        rest = tail;
        let mut prev = i64::from_le_bytes(raw.try_into().expect("codec: ts head"));
        tss.push(prev);
        let mut remaining = present - 1;
        while remaining > 0 {
            let take = remaining.min(TS_BLOCK);
            let (width, skip) = widths.take(rest);
            let packed_len = (take * width as usize).div_ceil(8);
            let mut r = BitReader::new(&rest[skip..skip + packed_len]);
            rest = &rest[skip + packed_len..];
            for _ in 0..take {
                prev = prev.wrapping_sub(r.read(width) as i64);
                tss.push(prev);
            }
            remaining -= take;
        }
    }

    // (b) id stream.
    let mut ids: Vec<RelId> = Vec::with_capacity(n);
    if n > 0 {
        let (raw, tail) = rest.split_at(8);
        rest = tail;
        let mut prev = i64::from_le_bytes(raw.try_into().expect("codec: id head"));
        ids.push(prev);
        let mut remaining = n - 1;
        while remaining > 0 {
            let take = remaining.min(ID_BLOCK);
            let (width, skip) = widths.take(rest);
            let packed_len = (take * width as usize).div_ceil(8);
            let mut r = BitReader::new(&rest[skip..skip + packed_len]);
            rest = &rest[skip + packed_len..];
            for _ in 0..take {
                prev = prev.wrapping_add(unzigzag(r.read(width)));
                ids.push(prev);
            }
            remaining -= take;
        }
    }

    // Zip the parallel streams back into hops, original order.
    let mut next_ts = tss.into_iter();
    ids.iter()
        .enumerate()
        .map(|(i, &id)| Hop {
            ts: (bitmap[i / 8] & (1 << (i % 8)) != 0)
                .then(|| next_ts.next().expect("codec: ts stream short")),
            id,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(hops: &[Hop]) {
        let bytes = encode_chunk(hops);
        assert_eq!(decode_chunk(&bytes, hops.len()), hops);
        assert_eq!(stream_len(&bytes, hops.len()), bytes.len());
    }

    #[test]
    fn roundtrips_the_chunk_shapes_the_store_holds() {
        roundtrip(&[]);
        roundtrip(&[Hop { ts: None, id: 7 }]);
        roundtrip(&[Hop {
            ts: Some(42),
            id: i64::MAX,
        }]);
        // Missing-ts first, then ts DESC with equal-ts ties newest-first.
        let mut ordered: Vec<Hop> = (0..300)
            .map(|i| Hop {
                ts: if i % 4 == 0 { None } else { Some(1000 - i) },
                id: i,
            })
            .collect();
        ordered.sort_unstable_by(crate::adjacency::cmp_ts_desc);
        roundtrip(&ordered);
        // Block tails and the full i64 width.
        for n in [63usize, 64, 65, 127, 129] {
            let hops: Vec<Hop> = (0..n as i64)
                .map(|i| Hop {
                    ts: Some(i64::MAX - i),
                    id: i64::MIN + i,
                })
                .collect();
            roundtrip(&hops);
        }
    }
}
