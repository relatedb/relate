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
            hops: vec![AdjHop {
                ts: 20_000,
                id: 999,
            }],
            ..DeltaChunk::default()
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

/// ADR-0658's reservation quantum: above the 256-hop boundary the hop
/// vector reserves 256-granularity, not the power-of-two ladder — a
/// 4097-hop chunk the ladder would reserve 8192 slots for (64 KB of dead
/// capacity for one hop) reserves 4352. Below the boundary the ladder
/// stays (5 hops reserve 8, the steady-state chunk's tight fit), and the
/// bands share the 256 boundary exactly. The charge and the fit are one
/// policy: a chunk grown by inserts holds the same capacity the census
/// and the admission estimate price.
#[test]
fn hop_vectors_reserve_the_256_quantum_above_the_ladder_band() {
    let hop_stride = std::mem::size_of::<AdjHop>() as u64;
    assert_eq!(
        crate::memory::hops_reserved_len(0),
        0,
        "an empty hop vector reserves nothing"
    );
    assert_eq!(
        crate::memory::hops_reserved_len(5),
        8,
        "the sub-quantum band keeps the ladder"
    );
    assert_eq!(
        crate::memory::hops_reserved_len(256),
        256,
        "the shared boundary"
    );
    // 4097: the ladder reserves 8192 slots, the quantum 4352 — it wins.
    assert_eq!(
        crate::memory::reserved_len(4097),
        8192,
        "the pure power-of-two ladder"
    );
    assert_eq!(crate::memory::hops_reserved_len(4097), 4352);
    let quantum = crate::memory::HOP_RESERVE_QUANTUM as u64;
    let chunk_stride = std::mem::size_of::<DeltaChunk>() as u64;
    assert_eq!(
        adjacency_bytes(std::iter::once(4097)),
        crate::memory::vec_heap(1, chunk_stride) + 4097u64.div_ceil(quantum) * quantum * hop_stride,
        "the charge follows the quantum, not the ladder"
    );
    assert_ne!(
        adjacency_bytes(std::iter::once(4097)),
        crate::memory::vec_heap(1, chunk_stride) + crate::memory::vec_heap(4097, hop_stride),
        "the ladder would overshoot by 64 KB here"
    );

    // The fit agrees on the insert path: 600 hops reserve the quantum's
    // 768 where the ladder says 1024 — a count on which the two differ.
    let mut adj = TypedAdj::default();
    for i in 0..600 {
        adj.insert(1, i, Some(600 - i));
    }
    assert_eq!(crate::memory::reserved_len(600), 1024, "the ladder rung");
    let chunk = &adj.chunks[0];
    assert_eq!(
        chunk.hops.capacity(),
        crate::memory::hops_reserved_len(600),
        "the insert path reserves at the quantum, not the ladder"
    );
}

/// The stability floor is the seal's break-even at the live strides: sealing
/// retires `AdjHop`'s 16 B per hop against the sealed entry's 56 B plus the
/// arena stream's ~1.1 B/hop, while the delta entry the chunk keeps is
/// retained either way and cancels. A stride change moves the break-even
/// and fails here (ADR-0655).
#[test]
fn the_stability_floor_is_the_seal_break_even() {
    use std::mem::size_of;
    // The sealed entry's fixed cost and the stream's arena charge per hop
    // in tenths of a byte (ADR-0599's measurement).
    const SEALED_ENTRY_BYTES: u64 = 56;
    const ARENA_TENTHS_PER_HOP: u64 = 11;
    // The delta entry (40 B) is priced on both sides — it is retained either
    // way; the hop is the 16 B the seal retires.
    let delta_entry = size_of::<DeltaChunk>() as u64;
    let hop = size_of::<AdjHop>() as u64;
    // Both sides in tenths of a byte.
    let delta_cost = |len: u64| (delta_entry + hop * len) * 10;
    let sealed_cost =
        |len: u64| delta_entry * 10 + SEALED_ENTRY_BYTES * 10 + ARENA_TENTHS_PER_HOP * len;
    let derived = (1..=COMPACTION_DELTA_FLOOR as u64)
        .find(|&len| sealed_cost(len) <= delta_cost(len))
        .expect("sealing pays below the knee");
    assert_eq!(STABILITY_SEAL_FLOOR as u64, derived);
    // The wholesale build's gate reads the same break-even (ADR-0599 /
    // ADR-0608); it is allowed to diverge later, but a stride change must
    // fail here for both, not leave one silently stale.
    assert_eq!(RESTORE_SEAL_HOPS as u64, derived);
}
