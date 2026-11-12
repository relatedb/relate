//! The adjacency store tier (ADR-0031 / ADR-0033 / ADR-0551): the live
//! delta and its hop order in [`delta`](self::delta), and the sealed tier
//! here — compressed immutable hop chunks plus versioned tombstones. The
//! sealed tier is a `Graph`-level side table keyed by node and direction
//! ([`SealedAdj`]); a node that never folds has no entry in it.
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

mod bitpack;
mod codec;
mod delta;

pub(crate) use self::delta::{cmp_ts_desc, AdjHop, TypedAdj};

use std::cmp::Ordering;
use std::collections::HashSet;
use std::mem::size_of;

use relate_core::RelId;

use crate::memory::{vec_growth, vec_heap};

/// A never-sealed chunk seals only when its delta reaches this hop count
/// (ADR-0551: the measured knee of the cutoff sweep).
pub(crate) const COMPACTION_DELTA_FLOOR: usize = 32;

/// What superseded a sealed position. The fold needs it: a deleted row has
/// no successor hop, a repositioned one is still live at its delta hop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Supersede {
    /// The row was deleted.
    Deleted,
    /// A ts update moved the row to a new hop.
    Repositioned,
}

/// One superseded or deleted sealed position: the hop id and two version
/// stamps. `xmax` is when the sealed position was superseded; `deleted` is
/// when the id's delta hop was deleted. A delete after a reposition needs
/// both: the sealed position is gone from the reposition on, while the
/// relocated delta hop stays readable until the delete. A snapshot older
/// than a stamp still sees what it hides, so the tombstone is versioned like
/// a version chain's `xmax`. Ids are monotonic and never reused, so the id
/// alone names the hop (ADR-0551 §Invariants).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Tombstone {
    pub id: RelId,
    /// When the sealed position was superseded (a reposition or a delete);
    /// `hidden_at` filters the sealed stream on it.
    pub xmax: u64,
    /// When the id's delta hop was deleted; `None` while a reposition keeps
    /// it live. The reclaim drops the delta hop on it.
    pub deleted: Option<u64>,
}

/// One tombstone slot's stride — owned beside the type, so the layout the
/// chunk's floor and growth terms price is stated once.
pub(crate) fn tombstone_stride() -> u64 {
    size_of::<Tombstone>() as u64
}

/// One compressed, immutable hop chunk and the tombstones that filter it.
#[derive(Clone, Debug)]
pub(crate) struct SealedChunk {
    /// The hybrid-codec hop stream, in `cmp_ts_desc` order.
    pub bytes: Vec<u8>,
    pub tombstones: Vec<Tombstone>,
}

impl SealedChunk {
    /// Physical hop count from the encoded header — no decode.
    pub(crate) fn len(&self) -> usize {
        codec::chunk_len(&self.bytes)
    }

    /// Whether a tombstone can be reclaimed at `horizon` (ADR-0551): its
    /// sealed position or its delta hop is at or below it.
    pub(crate) fn reclaimable(&self, horizon: u64) -> bool {
        self.tombstones
            .iter()
            .any(|t| t.xmax <= horizon || t.deleted.is_some_and(|d| d <= horizon))
    }

    /// The slot owning `id`'s tombstone, if the chunk holds one — the one
    /// statement of tombstone id ownership for the per-id readers: the read
    /// filter and the in-place update derive from it. The admission
    /// predicate's batch form ([`fresh_tombstone_count`](Self::fresh_tombstone_count))
    /// answers a whole batch in one pass over the vec instead, because a
    /// per-id probe per batch id would be quadratic in the chunk's
    /// tombstone count.
    fn tombstone_slot(&self, id: RelId) -> Option<usize> {
        self.tombstones.iter().position(|t| t.id == id)
    }

    /// Whether the sealed hop `id` is hidden at read timestamp `ts` — the
    /// sealed position's supersede stamp only. The delta hop has its own
    /// delete stamp and folds only once no reader needs it.
    fn hidden_at(&self, id: RelId, ts: u64) -> bool {
        self.tombstone_slot(id)
            .is_some_and(|i| self.tombstones[i].xmax <= ts)
    }

    /// How many of `ids` the chunk holds no tombstone for yet — the
    /// in-place-update arm of [`tombstone`](Self::tombstone) asked of a
    /// whole batch: a delete after a reposition updates the existing stamp
    /// and adds no slot. One pass over the tombstones answers every id, so
    /// a delete-heavy batch's admission never scans the vec once per id
    /// (quadratic in the chunk's tombstone count). Tombstone ids are
    /// unique — the push path runs only where `tombstone_slot` found none —
    /// so the held count is the count of `ids` present.
    pub(crate) fn fresh_tombstone_count(&self, ids: &HashSet<RelId>) -> usize {
        ids.len()
            - self
                .tombstones
                .iter()
                .filter(|t| ids.contains(&t.id))
                .count()
    }

    /// Bytes the tombstone vec gains from `fresh` new slots — the canonical
    /// [`vec_growth`] the sealed tier's other growth terms ride, so the
    /// reserved-vec algebra is spelled once for the whole tier.
    pub(crate) fn tombstone_growth_bytes(&self, fresh: usize) -> u64 {
        vec_growth(self.tombstones.len(), fresh, tombstone_stride())
    }

    /// The zero-hop chunk a never-sealed type's first tombstone lands in:
    /// the empty encoding — not an empty `Vec`, so [`len`](Self::len) reads
    /// its header — and no tombstones.
    fn zero_hop() -> Self {
        Self {
            bytes: encode_chunk(&[]),
            tombstones: Vec::new(),
        }
    }

    /// Record `id`'s sealed position as superseded at `ts`. Never decodes
    /// the stream: a hop that only lives in the delta records a tombstone too
    /// (the write path cannot tell without a decode), and the fold resolves
    /// it — a spurious reposition tombstone is dropped there, a deleted hop
    /// folds in under its tombstone (ADR-0551). A delete after a
    /// reposition keeps the sealed stamp and adds the delete stamp: the
    /// sealed position was superseded at the reposition, the relocated delta
    /// hop dies at the delete.
    pub(crate) fn tombstone(&mut self, id: RelId, ts: u64, supersede: Supersede) {
        let deleted = match supersede {
            Supersede::Deleted => Some(ts),
            Supersede::Repositioned => None,
        };
        if let Some(i) = self.tombstone_slot(id) {
            if deleted.is_some() {
                self.tombstones[i].deleted = deleted;
            }
            return;
        }
        self.tombstones.push(Tombstone {
            id,
            xmax: ts,
            deleted,
        });
        crate::memory::fit_reserved(&mut self.tombstones);
    }

    /// The hop stream this chunk holds, decoded, tombstones unfiltered.
    pub(crate) fn decoded(&self) -> Vec<AdjHop> {
        codec::decode_chunk(&self.bytes)
    }

    /// The hop stream this chunk holds, decoded and with the hops a
    /// tombstone hides at `ts` dropped.
    pub(crate) fn visible_hops(&self, ts: u64) -> Vec<AdjHop> {
        let mut hops = codec::decode_chunk(&self.bytes);
        if !self.tombstones.is_empty() {
            hops.retain(|h| !self.hidden_at(h.id, ts));
        }
        hops
    }

    /// Hold exactly the capacity [`floor_bytes`](Self::floor_bytes) charges,
    /// so the charge is the vectors' real heap.
    fn fit_reserved(&mut self) {
        crate::memory::fit_reserved(&mut self.bytes);
        crate::memory::fit_reserved(&mut self.tombstones);
    }

    /// Heap of one chunk's stream bytes and `tombstone_count` tombstone
    /// slots — the one statement of the chunk floor's two terms, which both
    /// the chunk's own count ([`floor_bytes`](Self::floor_bytes)) and the
    /// founding case ([`fresh_sealed_chunk_floor_bytes`], a hypothetical
    /// count and the codec's empty stream) delegate to. The chunk struct
    /// itself is inline in [`SealedAdj::chunks`], whose stride owns its
    /// `Vec` headers.
    pub(crate) fn floor_bytes_at(stream_len: usize, tombstone_count: usize) -> u64 {
        vec_heap(stream_len, 1) + vec_heap(tombstone_count, tombstone_stride())
    }

    /// Heap of one chunk's bytes and tombstones —
    /// [`floor_bytes_at`](Self::floor_bytes_at) at the chunk's own stream
    /// length and tombstone count.
    pub(crate) fn floor_bytes(&self) -> u64 {
        Self::floor_bytes_at(self.bytes.len(), self.tombstones.len())
    }
}

/// One node's sealed region — the value of the `Graph`-level side table
/// (ADR-0551): compressed chunks keyed by the same type ids as the delta's
/// `TypedAdj::chunks`, in the same first-seen type order.
#[derive(Clone, Debug, Default)]
pub(crate) struct SealedAdj {
    pub chunks: Vec<(u32, SealedChunk)>,
}

impl SealedAdj {
    pub(crate) fn chunk(&self, type_id: u32) -> Option<&SealedChunk> {
        self.chunks
            .iter()
            .find(|(t, _)| *t == type_id)
            .map(|(_, c)| c)
    }

    pub(crate) fn chunk_mut(&mut self, type_id: u32) -> Option<&mut SealedChunk> {
        self.chunks
            .iter_mut()
            .find(|(t, _)| *t == type_id)
            .map(|(_, c)| c)
    }

    /// Physical hop count of `type_id`'s chunk — no decode.
    pub(crate) fn chunk_len(&self, type_id: u32) -> usize {
        self.chunk(type_id).map_or(0, SealedChunk::len)
    }

    /// Record a versioned tombstone for a hop the write superseded (ADR-0551).
    /// No sealed-stream decode: a hop that only lives in the delta records one
    /// too, and the fold resolves it. A never-sealed chunk gets a zero-hop
    /// sealed stream on demand — without it the delete would escape the fold
    /// and seal its delta hop as a ghost.
    pub(crate) fn tombstone_deleted(
        &mut self,
        type_id: u32,
        rid: RelId,
        xmax: u64,
        supersede: Supersede,
    ) {
        self.chunk_or_insert(type_id)
            .tombstone(rid, xmax, supersede);
    }

    /// Bytes the chunks vec gains from `added` new chunk entries — the
    /// reserved-vec model [`floor_bytes`](Self::floor_bytes) charges, owned
    /// beside it so the admission estimate never restates the stride. A
    /// region being founded prices from
    /// [`fresh_sealed_chunk_entry_bytes`] instead.
    pub(crate) fn chunk_entry_growth_bytes(&self, added: usize) -> u64 {
        vec_growth(
            self.chunks.len(),
            added,
            size_of::<(u32, SealedChunk)>() as u64,
        )
    }

    /// `type_id`'s chunk, creating a zero-hop one when the type never sealed:
    /// the tombstone a delete records still needs a home. The stream is the
    /// empty encoding, not an empty `Vec`, so `chunk_len` reads its header.
    pub(crate) fn chunk_or_insert(&mut self, type_id: u32) -> &mut SealedChunk {
        let i = match self.chunks.iter().position(|(t, _)| *t == type_id) {
            Some(i) => i,
            None => {
                let mut chunk = SealedChunk::zero_hop();
                chunk.fit_reserved();
                self.chunks.push((type_id, chunk));
                crate::memory::fit_reserved(&mut self.chunks);
                self.chunks.len() - 1
            }
        };
        &mut self.chunks[i].1
    }

    /// Store one type's sealed hop stream and tombstones, or drop the type's
    /// chunk when both are empty (a fully reclaimed chunk keeps no slot). A
    /// zero-hop stream with a tombstone left keeps its chunk: the tombstone
    /// still owns the delta hop's reclaim.
    pub(crate) fn put_chunk(&mut self, type_id: u32, hops: &[AdjHop], tombstones: Vec<Tombstone>) {
        if hops.is_empty() && tombstones.is_empty() {
            self.chunks.retain(|(t, _)| *t != type_id);
            crate::memory::fit_reserved(&mut self.chunks);
            return;
        }
        let mut chunk = SealedChunk {
            bytes: encode_chunk(hops),
            tombstones,
        };
        chunk.fit_reserved();
        match self.chunk_mut(type_id) {
            Some(slot) => *slot = chunk,
            None => {
                self.chunks.push((type_id, chunk));
                crate::memory::fit_reserved(&mut self.chunks);
            }
        }
    }

    pub(crate) fn floor_bytes(&self) -> u64 {
        vec_heap(self.chunks.len(), size_of::<(u32, SealedChunk)>() as u64)
            + self
                .chunks
                .iter()
                .map(|(_, c)| c.floor_bytes())
                .sum::<u64>()
    }
}

/// Encode one sealed chunk's hop stream. `hops` must be in `cmp_ts_desc`
/// order.
pub(crate) fn encode_chunk(hops: &[AdjHop]) -> Vec<u8> {
    codec::encode_chunk(hops)
}

/// Bytes the chunks vec of a region being founded gains from `added` new
/// chunk entries — the founding case of [`SealedAdj::chunk_entry_growth_bytes`],
/// spelled as a sibling of the other fresh terms so no throwaway region is
/// constructed to price from empty.
pub(crate) fn fresh_sealed_chunk_entry_bytes(added: usize) -> u64 {
    vec_growth(0, added, size_of::<(u32, SealedChunk)>() as u64)
}

/// Floor bytes one fresh on-demand chunk brings besides its chunks-vec
/// entry (priced per region by [`SealedAdj::chunk_entry_growth_bytes`]): the
/// empty encoded stream and its first `tombstone_count` tombstone slots. The
/// apply path is `SealedAdj::chunk_or_insert` then `SealedChunk::tombstone`,
/// so the admission estimate needs the same terms — the chunk floor's own
/// formula ([`SealedChunk::floor_bytes_at`]) at the codec's empty-stream
/// length, never a chunk constructed to measure.
pub(crate) fn fresh_sealed_chunk_floor_bytes(tombstone_count: usize) -> u64 {
    SealedChunk::floor_bytes_at(codec::empty_chunk_bytes(), tombstone_count)
}

/// Merge a delta chunk and a decoded sealed chunk in `cmp_ts_desc` order.
/// Both inputs are already sorted; on a tie the delta hop goes first.
pub(crate) fn merge_chunk(delta: &[AdjHop], sealed: &[AdjHop], mut f: impl FnMut(AdjHop)) {
    let (mut i, mut j) = (0, 0);
    while i < delta.len() && j < sealed.len() {
        if cmp_ts_desc(&sealed[j], &delta[i]) == Ordering::Less {
            f(sealed[j]);
            j += 1;
        } else {
            f(delta[i]);
            i += 1;
        }
    }
    for hop in &delta[i..] {
        f(*hop);
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
    hops: &mut Vec<AdjHop>,
    delta: &mut Vec<AdjHop>,
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
