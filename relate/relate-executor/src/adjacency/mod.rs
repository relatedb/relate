//! The adjacency store tier (ADR-0031 / ADR-0033 / ADR-0551 / ADR-0599):
//! the live delta and its hop order in [`delta`](self::delta), and the
//! sealed tier here — compressed immutable hop chunks plus versioned
//! tombstones, held in one graph-level, node-major [`SealedArena`].
//!
//! The sealed tier is a `Graph`-level structure keyed by node and direction
//! ([`SealedArena`], one per direction); a node that never folds has no
//! entry in it. The chunk bytes live in the arena's single `Vec<u8>` — a
//! node's chunks adjacent — addressed by a per-node span of 12-byte
//! [`Entry`]s (`type_id`, arena `off`, hop count), replacing the old
//! per-chunk `Vec<u8>`. The codec's leading `u32` hop-count header is gone:
//! the count rides the entry, and the first raw id leads the id stream the
//! entry points at.
//!
//! Not to be confused with [`crate::graph::adjacency`], the graph's
//! adjacency accessors (degree, neighbor iteration, snapshot-visible
//! traversal) that read and write this tier.
//!
//! A chunk is folded — delta and reclaimed tombstones merged into the sealed
//! stream — at the delta-size watermark (a never-sealed chunk reaching
//! [`COMPACTION_DELTA_FLOOR`] hops, or a delta past 25% of the sealed hops,
//! or a tombstone reclaimable at the GC horizon). A due chunk never drags its
//! siblings. See ADR-0551 for the gate and the confluence argument.
//!
//! The fold appends the new encoding to the arena and marks the replaced
//! range dead; arena compaction rides the checkpoint fold
//! ([`SealedArena::compact`]).

mod bitpack;
mod codec;
mod delta;
#[cfg(test)]
mod pages;

pub(crate) use self::delta::{
    adjacency_bytes, cmp_ts_desc, resolve, AdjHop, DeltaChunk, Hop, TypedAdj,
};

use std::cmp::Ordering;
use std::collections::HashSet;
use std::mem::size_of;

use relate_core::RelId;

/// The paging format's reader surface, for the correctness suite's
/// pruning proofs outside this module (the format stays test-gated until
/// a consumer engages it).
#[cfg(test)]
pub(crate) use self::pages::{decode_block, decode_ranged, encode_block, zone_maps};
use crate::memory::{vec_growth, vec_heap};

/// A never-sealed chunk seals only when its delta reaches this hop count
/// (ADR-0551: the measured knee of the cutoff sweep).
pub(crate) const COMPACTION_DELTA_FLOOR: usize = 32;

/// A never-sealed chunk whose delta length held across two fold passes
/// seals at this hop count instead of [`COMPACTION_DELTA_FLOOR`]: a chunk
/// that stopped growing is at its steady state, so the knee's reason for
/// waiting — a still-growing chunk would only re-fold later — is gone. The
/// floor is the seal's break-even at the tier's own strides: sealing
/// retires a [`AdjHop`]'s 16 B per hop against a 56 B sealed entry plus the
/// arena stream's ~1.1 B/hop, while the 40 B delta entry is retained either
/// way and cancels — net positive at 4 hops, a wash at 3, a loss below
/// (ADR-0655). The wholesale build keeps its own [`RESTORE_SEAL_HOPS`] gate.
pub(crate) const STABILITY_SEAL_FLOOR: usize = 4;

/// A rebuilt chunk whose delta reaches this hop count is sealed by the
/// wholesale adjacency build's own pass (ADR-0599 / ADR-0608). The measured
/// break-even from the seal-policy record: sealing swaps a chunk's 16 B/hop
/// delta heap for a 56 B sealed entry plus a ~1.1 B/hop stream while its
/// 40 B delta entry is retained either way — net positive at 4 hops, a wash
/// at 3, a loss below. Deliberately below [`COMPACTION_DELTA_FLOOR`] (32):
/// the build folds short chunks the write path leaves delta.
pub(crate) const RESTORE_SEAL_HOPS: usize = 4;

/// The read timestamp that hides nothing: a tombstone's `xmax` is always
/// at least 1 (the write path stamps from the commit clock, which starts
/// there), so a read at 0 walks the raw physical adjacency in merged order.
pub(crate) const PHYSICAL_READ_TS: u64 = 0;
mod arena;
mod span_table;
mod wall;

pub(crate) use self::arena::{
    hop_count, tombstone_stride, Entry, SealedArena, SealedRegion, SealedStream, Supersede,
    Tombstone,
};
pub(crate) use self::span_table::{span_stride, MAX_SPAN_TYPES};
pub(crate) use self::wall::arena_admits;
#[cfg(test)]
pub(crate) use self::wall::with_arena_wall;

/// Bytes the entry array and its tombstone column gain from `added` new
/// entries, at the lengths `entries`/`tombstones` hold now.
pub(crate) fn entry_column_growth(entries: usize, tombstones: usize, added: usize) -> u64 {
    vec_growth(entries, added, size_of::<Entry>() as u64)
        + vec_growth(tombstones, added, size_of::<Vec<Tombstone>>() as u64)
}

/// Floor bytes one fresh joining chunk brings besides the entry-column growth:
/// its tombstone slots (the zero-hop stream appends no arena bytes, ADR-0599).
/// The apply path is `tombstone_deleted` on a never-sealed type, so the
/// admission estimate needs the same term.
pub(crate) fn fresh_sealed_chunk_floor_bytes(tombstone_count: usize) -> u64 {
    vec_heap(tombstone_count, tombstone_stride())
}

/// Merge a delta chunk — its raw hop slice and presence boundary derived
/// inside, so each hop resolves through [`resolve`] and the pair never
/// travels split — and a decoded sealed chunk in `cmp_ts_desc` order. Both
/// inputs are already sorted; on a tie the delta hop goes first.
pub(crate) fn merge_chunk(delta: Option<&DeltaChunk>, sealed: &[Hop], mut f: impl FnMut(Hop)) {
    // A missing delta chunk (a type whose hops all sealed away) is the
    // empty hop slice: the sealed stream still reads, merged against
    // nothing.
    let (delta, delta_absent): (&[AdjHop], u32) =
        delta.map_or((&[], 0), |c| (&c.hops, c.absent_len()));
    let (mut i, mut j) = (0, 0);
    while i < delta.len() && j < sealed.len() {
        if cmp_ts_desc(&sealed[j], &resolve(delta, delta_absent, i)) == Ordering::Less {
            f(sealed[j]);
            j += 1;
        } else {
            f(resolve(delta, delta_absent, i));
            i += 1;
        }
    }
    while i < delta.len() {
        f(resolve(delta, delta_absent, i));
        i += 1;
    }
    for hop in &sealed[j..] {
        f(*hop);
    }
}

/// Drop the hops whose tombstone stamp is at or below `horizon`, from the
/// sealed stream and the delta, returning the tombstones that still apply.
/// The sealed position goes on its supersede stamp, the delta hop on its
/// delete stamp, and the tombstone stays while either stamp is above the
/// horizon — a delete after a reposition reclaims the sealed position while
/// its relocated hop stays readable. A deleted row's delta hop dies with its
/// sealed position: once the fold seals it, the chain GC can no longer prune
/// it and it would be a ghost. A reposition's delta hop is the id's live
/// position and stays.
pub(crate) fn reclaim(
    hops: &mut Vec<Hop>,
    delta: &mut Vec<Hop>,
    tombstones: Vec<Tombstone>,
    horizon: u64,
) -> Vec<Tombstone> {
    let mut sealed_dead: HashSet<RelId> = HashSet::new();
    let mut deleted_dead: HashSet<RelId> = HashSet::new();
    let mut kept = Vec::new();
    for t in tombstones {
        if t.xmax <= horizon {
            sealed_dead.insert(t.id);
        }
        if t.deleted.is_some_and(|d| d <= horizon) {
            deleted_dead.insert(t.id);
        }
        if t.xmax > horizon || t.deleted.is_some_and(|d| d > horizon) {
            kept.push(t);
        }
    }
    if !sealed_dead.is_empty() {
        hops.retain(|h| !sealed_dead.contains(&h.id));
    }
    if !deleted_dead.is_empty() {
        delta.retain(|h| !deleted_dead.contains(&h.id));
    }
    kept
}
