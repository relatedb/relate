//! The hybrid adjacency codec (ADR-0551, after ADR-0206): one sealed chunk's
//! ts-DESC hop stream as parallel bit-packed streams.
//!
//! Layout: `u32` hop count, a ts-presence bitmap (1 bit per hop), the ts
//! stream (first present value raw `i64`, then non-negative gaps — ts is
//! non-increasing — in 128-value blocks), and the id stream (first id raw,
//! then zigzag deltas in 64-value blocks). Decoding re-emits hops in the
//! original order, so newest-first iteration is byte-identical.
//!
//! Scalar only: ADR-0206 measured the NEON block decoder at 0.72–1.07x the
//! scalar reader, and the experiment copy under
//! `testing/relate-datasets/src/codec` stays pinned as its reference.

use relate_core::RelId;

use super::bitpack::{bit_width, unzigzag, zigzag, BitReader, BitWriter};
use super::AdjHop;

/// ts-gap block size.
const TS_BLOCK: usize = 128;
/// zigzag-delta block size for the id stream.
const ID_BLOCK: usize = 64;

/// Encode one chunk's hop stream. `hops` must already be in `cmp_ts_desc`
/// order.
pub(crate) fn encode_chunk(hops: &[AdjHop]) -> Vec<u8> {
    let n = u32::try_from(hops.len()).expect("adjacency chunk longer than u32::MAX hops");
    let mut out = Vec::new();
    out.extend_from_slice(&n.to_le_bytes());

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
            out.push(width as u8);
            let mut w = BitWriter::new();
            for &gap in block {
                w.write(gap, width);
            }
            out.extend_from_slice(&w.finish());
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
            out.push(width as u8);
            let mut w = BitWriter::new();
            for &delta in block {
                w.write(delta, width);
            }
            out.extend_from_slice(&w.finish());
        }
    }
    out
}

/// The hop count encoded in a chunk's header.
pub(crate) fn chunk_len(bytes: &[u8]) -> usize {
    // Only this process's own encoder writes a chunk stream, so its header is
    // always there; a short buffer is a broken chunk, not a zero-hop stream.
    let head = bytes
        .get(..4)
        .expect("chunk stream carries its hop-count header");
    u32::from_le_bytes(head.try_into().expect("four bytes")) as usize
}

/// Decode one chunk encoded by [`encode_chunk`]; the hop stream comes back
/// in the original chunk order.
pub(crate) fn decode_chunk(bytes: &[u8]) -> Vec<AdjHop> {
    let n = chunk_len(bytes);
    let mut rest = &bytes[4..];
    let bitmap_len = n.div_ceil(8);
    let (bitmap, tail) = rest.split_at(bitmap_len);
    rest = tail;

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
            let width = rest[0] as u32;
            let packed_len = (take * width as usize).div_ceil(8);
            let mut r = BitReader::new(&rest[1..1 + packed_len]);
            rest = &rest[1 + packed_len..];
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
            let width = rest[0] as u32;
            let packed_len = (take * width as usize).div_ceil(8);
            let mut r = BitReader::new(&rest[1..1 + packed_len]);
            rest = &rest[1 + packed_len..];
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
        .map(|(i, &id)| AdjHop {
            ts: (bitmap[i / 8] & (1 << (i % 8)) != 0)
                .then(|| next_ts.next().expect("codec: ts stream short")),
            id,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(hops: &[AdjHop]) {
        assert_eq!(decode_chunk(&encode_chunk(hops)), hops);
        assert_eq!(chunk_len(&encode_chunk(hops)), hops.len());
    }

    #[test]
    #[should_panic(expected = "chunk stream carries its hop-count header")]
    fn a_short_chunk_stream_panics() {
        chunk_len(&[0u8, 1, 2]);
    }

    #[test]
    fn roundtrips_the_chunk_shapes_the_store_holds() {
        roundtrip(&[]);
        roundtrip(&[AdjHop { ts: None, id: 7 }]);
        roundtrip(&[AdjHop {
            ts: Some(42),
            id: i64::MAX,
        }]);
        // Missing-ts first, then ts DESC with equal-ts ties newest-first.
        let mut ordered: Vec<AdjHop> = (0..300)
            .map(|i| AdjHop {
                ts: if i % 4 == 0 { None } else { Some(1000 - i) },
                id: i,
            })
            .collect();
        ordered.sort_unstable_by(crate::adjacency::cmp_ts_desc);
        roundtrip(&ordered);
        // Block tails and the full i64 width.
        for n in [63usize, 64, 65, 127, 129] {
            let hops: Vec<AdjHop> = (0..n as i64)
                .map(|i| AdjHop {
                    ts: Some(i64::MAX - i),
                    id: i64::MIN + i,
                })
                .collect();
            roundtrip(&hops);
        }
    }
}
