//! Compressed-adjacency codec prototype: scalar encode/decode plus a SIMD
//! decode variant measured against it.
//!
//! Two schemes, both encoding one `TypedAdj` chunk's hop stream
//! (`AdjHop { ts: Option<i64>, id: RelId }`, ts-DESC by invariant with
//! missing-ts first, see `relate-executor/src/adjacency/delta.rs`) as
//! **parallel streams**:
//!
//! 1. ts-presence bitmap (1 bit per hop, shared by both schemes)
//! 2. ts stream: first present value raw `i64`, then non-negative gaps
//!    (`prev - cur`, ts is non-increasing) bit-packed in 128-value blocks
//! 3. id stream, per scheme:
//!    - [`Scheme::For128`]: frame-of-reference per 128-value block — block
//!      min raw `u64`, values stored as `value - min` at the block bit-width
//!    - [`Scheme::Hybrid`]: GDS-style packed tail — first id raw, then
//!      zigzag deltas (`cur - prev`, tolerates arbitrary order) bit-packed
//!      in 64-value blocks
//!
//! Chunk layout: `u32` hop count, bitmap (`ceil(n/8)` bytes), ts section,
//! id section. Decoding re-emits hops in the original chunk order, so
//! newest-first iteration semantics are byte-identical.
//!
//! [`decode_chunk_simd`] decodes the same byte format through a SIMD bit
//! unpacker: explicit NEON intrinsics on aarch64 (always available there, so
//! no runtime detection), scalar fallback elsewhere so the crate compiles and
//! behaves identically on x86_64. NEON was chosen over a portable
//! autovectorization bet because the measurements run on an aarch64 host and
//! explicit per-lane variable shifts (`vshlq_u64`) are the one operation the
//! scalar bit reader cannot express; `cfg(target_arch)` keeps every
//! aarch64-specific item out of other targets.
//!
//! This is a standalone measurement prototype: it never touches the live
//! store. Hop streams are reconstructed from query results (see
//! [`extract_chunks`]); chunk contents and order replicate the engine's
//! `cmp_ts_desc` exactly.

mod bitpack;
mod measure;
mod simd;

use bitpack::{bit_width, unzigzag, zigzag, BitWriter};
use relate_core::RelId;
use simd::{unpack_block_scalar, unpack_block_simd};

pub use measure::{
    extract_chunks, measure_dataset, BaselineTable, CodecDatasetReport, SchemeReport, BASELINE,
};
pub use simd::simd_available;

/// One adjacency hop: optional integer `ts` property plus relationship id.
/// Storage twin of the engine's `pub(crate) AdjHop`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hop {
    pub ts: Option<i64>,
    pub id: RelId,
}

/// Compressed-adjacency scheme selecting the id stream encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scheme {
    /// FOR + bitpacking over 128-value blocks (SIMD-BP128 shape, scalar).
    For128,
    /// GDS-style hybrid: first id raw, zigzag deltas bit-packed per
    /// 64-value block.
    Hybrid,
}

impl Scheme {
    pub const ALL: [Scheme; 2] = [Scheme::For128, Scheme::Hybrid];

    pub fn as_str(self) -> &'static str {
        match self {
            Scheme::For128 => "for128",
            Scheme::Hybrid => "hybrid",
        }
    }

    /// Slot in a per-scheme array (`ALL` order); crate-internal, so the
    /// array-slot layout stays an implementation detail.
    pub(crate) fn index(self) -> usize {
        match self {
            Scheme::For128 => 0,
            Scheme::Hybrid => 1,
        }
    }
}

/// id block size for [`Scheme::For128`].
const FOR_BLOCK: usize = 128;
/// zigzag-delta block size for [`Scheme::Hybrid`].
const HYBRID_BLOCK: usize = 64;
/// ts gap block size (both schemes).
const TS_BLOCK: usize = 128;

/// The engine's chunk order (ADR-0035 / ADR-0203): missing ts first, then
/// ts DESC, ties broken by relationship id DESC. Storage-order twin of the
/// executor's crate-private `cmp_ts_desc` (`relate-executor/src/adjacency/delta.rs`).
pub fn cmp_ts_desc(a: &Hop, b: &Hop) -> std::cmp::Ordering {
    match (a.ts, b.ts) {
        (None, None) => b.id.cmp(&a.id),
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(ta), Some(tb)) => tb.cmp(&ta).then(b.id.cmp(&a.id)),
    }
}

// ---------------------------------------------------------------------------
// Chunk encode / decode.
// ---------------------------------------------------------------------------

/// Encode one chunk's hop stream under `scheme`.
pub fn encode_chunk(scheme: Scheme, hops: &[Hop]) -> Vec<u8> {
    let n = hops.len();
    assert!(u32::try_from(n).is_ok(), "chunk longer than u32::MAX hops");
    let mut out = Vec::new();
    out.extend_from_slice(&(n as u32).to_le_bytes());

    // (a) ts-presence bitmap.
    let mut bitmap = vec![0u8; n.div_ceil(8)];
    for (i, hop) in hops.iter().enumerate() {
        if hop.ts.is_some() {
            bitmap[i / 8] |= 1 << (i % 8);
        }
    }
    out.extend_from_slice(&bitmap);

    // (b) ts stream: first value raw, gaps bit-packed per TS_BLOCK block.
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
            let width = bit_width(block.iter().copied().max().unwrap_or(0));
            out.push(width as u8);
            let mut w = BitWriter::new();
            for &gap in block {
                w.write(gap, width);
            }
            out.extend_from_slice(&w.finish());
        }
    }

    // (c) id stream.
    match scheme {
        Scheme::For128 => {
            for block in hops.chunks(FOR_BLOCK) {
                let min = block.iter().map(|h| h.id as u64).min().unwrap_or(0);
                let width = bit_width(block.iter().map(|h| (h.id as u64) - min).max().unwrap_or(0));
                out.extend_from_slice(&min.to_le_bytes());
                out.push(width as u8);
                let mut w = BitWriter::new();
                for hop in block {
                    w.write((hop.id as u64) - min, width);
                }
                out.extend_from_slice(&w.finish());
            }
        }
        Scheme::Hybrid => {
            if let Some((&head, tail)) = hops.split_first() {
                out.extend_from_slice(&(head.id as u64).to_le_bytes());
                let mut deltas = Vec::with_capacity(tail.len());
                let mut prev = head.id;
                for hop in tail {
                    deltas.push(zigzag(hop.id.wrapping_sub(prev)));
                    prev = hop.id;
                }
                for block in deltas.chunks(HYBRID_BLOCK) {
                    let width = bit_width(block.iter().copied().max().unwrap_or(0));
                    out.push(width as u8);
                    let mut w = BitWriter::new();
                    for &delta in block {
                        w.write(delta, width);
                    }
                    out.extend_from_slice(&w.finish());
                }
            }
        }
    }
    out
}

/// Decode one chunk encoded by [`encode_chunk`]; the hop stream comes back
/// in the original chunk order.
pub fn decode_chunk(scheme: Scheme, bytes: &[u8]) -> Vec<Hop> {
    decode_chunk_impl(scheme, bytes, unpack_block_scalar)
}

/// Decode one chunk through the SIMD bit unpacker (NEON on aarch64, scalar
/// fallback elsewhere). Byte-identical output to [`decode_chunk`].
pub fn decode_chunk_simd(scheme: Scheme, bytes: &[u8]) -> Vec<Hop> {
    decode_chunk_impl(scheme, bytes, unpack_block_simd)
}

/// Block unpacker a decoder runs on: the scalar bit reader or the SIMD
/// entry point. Choosing it once per decode keeps the mode out of the
/// per-block loop.
type UnpackBlock = fn(&[u8], u32, usize, &mut Vec<u64>);

fn decode_chunk_impl(scheme: Scheme, bytes: &[u8], unpack: UnpackBlock) -> Vec<Hop> {
    let (head, mut rest) = bytes.split_at(4.min(bytes.len()));
    let n = u32::from_le_bytes(head.try_into().expect("codec: missing count")) as usize;
    let bitmap_len = n.div_ceil(8);
    let bitmap = &rest[..bitmap_len];
    rest = &rest[bitmap_len..];

    // ts stream.
    let m = bitmap
        .iter()
        .map(|b| b.count_ones() as usize)
        .sum::<usize>();
    let mut tss: Vec<i64> = Vec::with_capacity(m);
    if m > 0 {
        let (raw, tail) = rest.split_at(8);
        let first = i64::from_le_bytes(raw.try_into().expect("codec: ts head"));
        rest = tail;
        tss.push(first);
        let mut remaining = m - 1;
        while remaining > 0 {
            let take = remaining.min(TS_BLOCK);
            let width = rest[0] as u32;
            rest = &rest[1..];
            let packed_len = take * width as usize;
            let packed_len = packed_len.div_ceil(8);
            let packed = &rest[..packed_len];
            rest = &rest[packed_len..];
            let mut gaps = Vec::with_capacity(take);
            unpack(packed, width, take, &mut gaps);
            let mut prev = *tss.last().expect("codec: ts state");
            for gap in gaps {
                let ts = prev.wrapping_sub(gap as i64);
                tss.push(ts);
                prev = ts;
            }
            remaining -= take;
        }
    }

    // id stream.
    let mut ids: Vec<RelId> = Vec::with_capacity(n);
    match scheme {
        Scheme::For128 => {
            let mut remaining = n;
            while remaining > 0 {
                let take = remaining.min(FOR_BLOCK);
                let (raw, tail) = rest.split_at(8);
                let min = u64::from_le_bytes(raw.try_into().expect("codec: id min"));
                rest = tail;
                let width = rest[0] as u32;
                rest = &rest[1..];
                let packed_len = (take * width as usize).div_ceil(8);
                let packed = &rest[..packed_len];
                rest = &rest[packed_len..];
                let mut vals = Vec::with_capacity(take);
                unpack(packed, width, take, &mut vals);
                ids.extend(vals.into_iter().map(|v| (min + v) as i64));
                remaining -= take;
            }
        }
        Scheme::Hybrid => {
            if n > 0 {
                let (raw, tail) = rest.split_at(8);
                let first = i64::from_le_bytes(raw.try_into().expect("codec: id head"));
                rest = tail;
                ids.push(first);
                let mut remaining = n - 1;
                while remaining > 0 {
                    let take = remaining.min(HYBRID_BLOCK);
                    let width = rest[0] as u32;
                    rest = &rest[1..];
                    let packed_len = (take * width as usize).div_ceil(8);
                    let packed = &rest[..packed_len];
                    rest = &rest[packed_len..];
                    let mut zs = Vec::with_capacity(take);
                    unpack(packed, width, take, &mut zs);
                    let mut prev = *ids.last().expect("codec: id state");
                    for z in zs {
                        prev = prev.wrapping_add(unzigzag(z));
                        ids.push(prev);
                    }
                    remaining -= take;
                }
            }
        }
    }

    // Zip the parallel streams back into hops, original order.
    let mut next_ts = tss.iter().copied();
    ids.iter()
        .enumerate()
        .map(|(i, &id)| {
            let has_ts = bitmap[i / 8] & (1 << (i % 8)) != 0;
            Hop {
                ts: if has_ts { next_ts.next() } else { None },
                id,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(scheme: Scheme, hops: &[Hop]) {
        let bytes = encode_chunk(scheme, hops);
        assert_eq!(&decode_chunk(scheme, &bytes), hops, "{scheme:?}");
        // The SIMD decoder is exercised on every edge case too; on non-aarch64
        // it is the scalar fallback, on aarch64 the NEON path.
        assert_eq!(&decode_chunk_simd(scheme, &bytes), hops, "{scheme:?} SIMD");
    }

    fn both(hops: &[Hop]) {
        for scheme in Scheme::ALL {
            roundtrip(scheme, hops);
        }
    }

    #[test]
    fn simd_matches_scalar_across_widths() {
        // Pseudo-random ids and timestamps spanning every bit-width bucket,
        // including the >57-bit blocks that drop to the scalar reader.
        let mut state = 0xdead_beef_cafe_f00du64;
        let mut rand = move || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            state >> 11
        };
        for width_bits in [1u32, 3, 7, 8, 9, 16, 24, 31, 32, 33, 40, 56, 57, 58, 63, 64] {
            let span = if width_bits == 64 {
                i64::MAX
            } else {
                (1i64 << width_bits.min(62)) - 1
            };
            let hops: Vec<Hop> = (0..300)
                .map(|i| Hop {
                    ts: if i % 3 == 0 {
                        None
                    } else {
                        Some(i64::MAX - (rand() % 1000) as i64 * i)
                    },
                    id: (rand() % (span as u64).max(1)) as i64,
                })
                .collect();
            both(&hops);
        }
    }

    #[test]
    fn simd_odd_block_tail() {
        // Odd counts exercise the unpacker's single-value tail.
        for n in [1usize, 3, 63, 65, 127, 129] {
            let hops: Vec<Hop> = (0..n as i64)
                .map(|i| Hop {
                    ts: Some(9_999 - i),
                    id: i * 7,
                })
                .collect();
            both(&hops);
        }
    }

    #[test]
    fn simd_availability_matches_target() {
        assert_eq!(simd_available(), cfg!(target_arch = "aarch64"));
    }

    #[test]
    fn empty_chunk() {
        both(&[]);
    }

    #[test]
    fn single_hop_with_and_without_ts() {
        both(&[Hop {
            ts: Some(42),
            id: 7,
        }]);
        both(&[Hop { ts: None, id: 7 }]);
    }

    #[test]
    fn all_missing_ts() {
        let hops: Vec<Hop> = (0..200).map(|id| Hop { ts: None, id }).collect();
        both(&hops);
    }

    #[test]
    fn ts_desc_with_duplicates_and_ties() {
        // Missing ts first, then DESC with equal-ts ties broken by id DESC.
        let mut hops = vec![
            Hop { ts: None, id: 9 },
            Hop { ts: None, id: 3 },
            Hop {
                ts: Some(100),
                id: 50,
            },
            Hop {
                ts: Some(100),
                id: 12,
            },
            Hop {
                ts: Some(99),
                id: 77,
            },
            Hop {
                ts: Some(99),
                id: 1,
            },
            Hop {
                ts: Some(5),
                id: 1000,
            },
        ];
        hops.sort_unstable_by(cmp_ts_desc);
        both(&hops);
    }

    #[test]
    fn ids_beyond_32_bits() {
        let hops: Vec<Hop> = (0..300)
            .map(|i| Hop {
                ts: Some(1_000_000 - i),
                id: (1u64 << 40) as i64 + i * 9973,
            })
            .collect();
        both(&hops);
    }

    #[test]
    fn max_width_blocks() {
        // FOR: a block spanning min 0 .. i64::MAX needs 63 bits; the hybrid
        // zigzag delta from i64::MAX down to 0 needs the full 64 bits.
        let mut hops = vec![Hop {
            ts: Some(i64::MAX),
            id: i64::MAX,
        }];
        hops.extend((1..200).map(|i| Hop {
            ts: Some(i64::MAX - i),
            id: (i % 2) * i64::MAX,
        }));
        both(&hops);
    }

    #[test]
    fn ts_full_i64_span() {
        let hops = vec![
            Hop {
                ts: Some(i64::MAX),
                id: 1,
            },
            Hop {
                ts: Some(i64::MIN),
                id: 2,
            },
        ];
        both(&hops);
    }

    #[test]
    fn multi_block_unsorted_ids() {
        // Pseudo-random unsorted ids over several blocks.
        let mut state = 0x1234_5678_9abc_def0u64;
        let hops: Vec<Hop> = (0..1000)
            .map(|i| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                Hop {
                    ts: Some(-(i as i64)),
                    id: (state >> 11) as i64,
                }
            })
            .collect();
        both(&hops);
    }

    #[test]
    fn chunk_order_matches_engine_comparator() {
        // The extraction comparator must reproduce the store's insert order:
        // missing-ts first (id DESC among themselves), then ts DESC, id DESC.
        let mut hops = [
            Hop {
                ts: Some(10),
                id: 1,
            },
            Hop { ts: None, id: 2 },
            Hop {
                ts: Some(10),
                id: 5,
            },
            Hop { ts: None, id: 8 },
        ];
        hops.sort_unstable_by(cmp_ts_desc);
        let ids: Vec<i64> = hops.iter().map(|h| h.id).collect();
        assert_eq!(ids, vec![8, 2, 5, 1]);
    }
}
