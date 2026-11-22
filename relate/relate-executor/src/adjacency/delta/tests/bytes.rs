use super::*;
use crate::adjacency::SealedArena;

/// ADR-0599 accounting, pinned on a hand-built arena: the floor charges the
/// arena's byte buffer at its exact length plus its 12-byte entry and its
/// tombstone slot, and the delta at live `AdjHop` bytes — never the sealed
/// hops at decoded hop size.
#[test]
fn floor_prices_sealed_at_stream_length_and_delta_at_live_hop_bytes() {
    use std::mem::size_of;

    let sealed_hops: Vec<Hop> = (0..COMPACTION_DELTA_FLOOR as i64)
        .map(|i| Hop {
            ts: Some(10_000 - i),
            id: i,
        })
        .collect();
    let bytes = crate::adjacency::codec::encode_chunk(&sealed_hops);
    assert!(
        (bytes.len() as u64) < sealed_hops.len() as u64 * size_of::<AdjHop>() as u64,
        "the codec compresses: {} bytes for {} hops",
        bytes.len(),
        sealed_hops.len()
    );
    let tombstones = vec![Tombstone {
        id: 0,
        xmax: 7,
        deleted: Some(7),
    }];

    let arena = SealedArena::default();
    let sealed = arena.region_or_default(9);
    assert!(sealed.put_chunk(9, &sealed_hops, tombstones.clone()));

    // The arena floor: the byte buffer exact, one 12-byte entry, the
    // tombstone column header, and the tombstone slots.
    let expected = crate::memory::vec_heap_exact(bytes.len(), 1)
        + crate::memory::vec_heap(1, size_of::<crate::adjacency::Entry>() as u64)
        + crate::memory::vec_heap(1, size_of::<Vec<Tombstone>>() as u64)
        + crate::memory::vec_heap(tombstones.len(), size_of::<Tombstone>() as u64);
    assert_eq!(arena.floor_bytes(), expected);
    assert_eq!(sealed.chunk_len(9), COMPACTION_DELTA_FLOOR);

    let delta = [Hop {
        ts: Some(20_000),
        id: 999,
    }];
    let adj = TypedAdj {
        chunks: vec![DeltaChunk {
            type_id: 9,
            absent: 0,
            stable_len: 0,
            hops: vec![AdjHop {
                ts: 20_000,
                id: 999,
            }],
        }],
    };
    assert_eq!(
        adj.floor_bytes(),
        adjacency_bytes(std::iter::once(delta.len()))
    );
}

/// The 16-byte hop and the 40-byte chunk entry — presence in the old
/// `(u32, Vec)` tuple's padding, the stability baseline in the word the
/// adaptive seal floor adds — the two strides the adjacency floor
/// arithmetic charts, so neither can drift silently.
#[test]
fn adj_hop_is_sixteen_bytes() {
    use std::mem::size_of;
    assert_eq!(size_of::<AdjHop>(), 16);
    assert_eq!(
        size_of::<DeltaChunk>(),
        40,
        "presence and the stability baseline"
    );
    assert_eq!(size_of::<Option<i64>>(), 16, "the Option this layout drops");
}

/// The adjacency floor's two strides: the 16-byte hop (presence lives in
/// the chunk header's padding, not the hop) and the 40-byte chunk entry —
/// the padding keeps presence free of charge, the stability baseline pays
/// for its own word.
#[test]
fn adjacency_bytes_prices_the_hop_at_sixteen_bytes() {
    // One 3-hop chunk: the entry plus three hops, both at exact fit.
    assert_eq!(
        adjacency_bytes(std::iter::once(3)),
        crate::memory::vec_heap(1, 40) + crate::memory::vec_heap(3, 16),
        "the chunk entry at 40 B and the hop at 16 B"
    );
}
