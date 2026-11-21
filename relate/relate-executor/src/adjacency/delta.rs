//! The live delta tier of the adjacency store (ADR-0031 / ADR-0033): the
//! per-node typed hop chunks in `ts`-DESC order, and the fold that moves
//! them into the node's sealed region of the graph-level arena
//! ([`super::SealedArena`], ADR-0551 / ADR-0599).

use std::mem::size_of;

use relate_core::RelId;

use crate::UsedSet;

use super::{
    merge_chunk, reclaim, SealedRegion, Supersede, Tombstone, COMPACTION_DELTA_FLOOR,
    PHYSICAL_READ_TS, RESTORE_SEAL_HOPS, STABILITY_SEAL_FLOOR,
};

/// One hop in a typed chunk, as the chunk stores it: the rel id and its
/// raw integer `ts` — 16 bytes (the `Option<i64>` ts had no niche and
/// alone cost the other 8). Whether a hop carries a ts at all is the
/// chunk's presence boundary ([`DeltaChunk::absent`]), never the value:
/// `ts` is user-settable, so no value is provably reserved, and the raw
/// `ts` of an absent hop is dead payload, filled with `0` so the stored
/// bytes stay deterministic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AdjHop {
    pub ts: i64,
    pub id: RelId,
}

/// A hop with its ts presence resolved — the shape every presence-aware
/// boundary sees: the write input, the read output, the batch sorts and
/// merges. [`DeltaChunk`] converts to and from [`AdjHop`] at its edge,
/// the one place presence is packed or unpacked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Hop {
    pub ts: Option<i64>,
    pub id: RelId,
}

/// Resolve hop `i` of a raw hop slice whose first `absent` entries carry
/// no ts — the one decode of the chunk's presence encoding, shared by the
/// read paths ([`merge_chunk`], `TypedAdj`'s iteration) and the tests.
pub(crate) fn resolve(hops: &[AdjHop], absent: u32, i: usize) -> Hop {
    Hop {
        ts: (i >= absent as usize).then_some(hops[i].ts),
        id: hops[i].id,
    }
}

/// One typed chunk of the delta tier: the header — `type_id` plus the
/// presence boundary in the 4 bytes of padding the old `(u32, Vec)`
/// tuple always carried — and the raw hop vector.
///
/// `absent` counts the leading hops with no ts: `cmp_ts_desc` orders
/// missing-ts hops first, so absence is always a prefix and the boundary
/// is the chunk's whole presence state — `0` is all-present,
/// `hops.len()` is all-absent, and between them the mixed chunk (a
/// per-hop bitmap would spill a `Vec<u64>` — one word even for a
/// ≤64-hop chunk — to say strictly less).
///
/// `stable_len` is the chunk's hop count as the previous fold pass noted
/// it — the stability baseline the adaptive seal floor reads; a chunk at
/// the same length two passes running has stopped growing, so the
/// [`STABILITY_SEAL_FLOOR`] arm seals it below the knee. The stride is
/// 40 B, the entry size the floor arithmetic charts.
#[derive(Clone, Debug, Default)]
pub(crate) struct DeltaChunk {
    pub type_id: u32,
    pub absent: u32,
    /// Hop count at the previous fold pass — 0 until first noted, which
    /// no non-empty chunk equals, so an unnoted chunk is never
    /// stability-sealed.
    pub stable_len: usize,
    pub hops: Vec<AdjHop>,
}

impl DeltaChunk {
    /// Whether the chunk is stability-eligible: at or above the stability
    /// floor and at the length the previous pass noted. The due rule's
    /// below-knee arm and the fold's below-floor guard read this one fact,
    /// so a chunk the pass marked due for its held length is never turned
    /// back at the guard.
    fn stability_due(&self) -> bool {
        self.hops.len() >= STABILITY_SEAL_FLOOR && self.stable_len == self.hops.len()
    }

    /// Whether this chunk is in the adaptive floor's band: the floor gates
    /// it (no sealed hop stream to ratio against), and it sits at or above
    /// the stability floor — a shorter chunk's baseline could never be
    /// read, so it takes none — and below the knee.
    /// [`TypedAdj::has_fold_work`] selects a node by this predicate and
    /// [`TypedAdj::note_stable`] notes by it — one spelling, so the pass's
    /// selector and the set it notes cannot drift apart.
    fn in_stability_band(&self, sealed_len: usize) -> bool {
        sealed_len == 0
            && self.hops.len() >= STABILITY_SEAL_FLOOR
            && self.hops.len() < COMPACTION_DELTA_FLOOR
    }

    /// Whether the floor gate admits this chunk's fold — the chunk has no
    /// sealed hop stream to ratio against, and it folds rather than waits:
    /// at the knee's [`COMPACTION_DELTA_FLOOR`], or at the adaptive
    /// [`STABILITY_SEAL_FLOOR`] because its length held since the previous
    /// pass. The one spelling of the floor's admission: the due predicate's
    /// floor arm reads it as the reason a chunk is due, and the fold's
    /// action as the reason the fold merges instead of reclaiming
    /// tombstones.
    fn floor_admits_fold(&self) -> bool {
        self.hops.len() >= COMPACTION_DELTA_FLOOR || self.stability_due()
    }

    /// The chunk's hops with ts presence resolved, in chunk order.
    fn to_hops(&self) -> Vec<Hop> {
        (0..self.hops.len())
            .map(|i| resolve(&self.hops, self.absent, i))
            .collect()
    }

    /// Take the chunk's hops as presence-resolved values, resetting it to
    /// empty — the fold's move of the delta out of the chunk. The
    /// stability baseline stays: a fold the walls refuse hands the hops
    /// back and the next pass compares the same one.
    fn take_hops(&mut self) -> Vec<Hop> {
        let hops = self.to_hops();
        self.absent = 0;
        self.hops = Vec::new();
        hops
    }

    /// Install `hops` (which must be in `cmp_ts_desc` order, so the
    /// absent ones lead) as the chunk's storage, packing the presence
    /// boundary — the one place absence is encoded.
    fn set_hops(&mut self, hops: Vec<Hop>) {
        let absent = hops.iter().take_while(|h| h.ts.is_none()).count();
        debug_assert!(
            !hops.iter().skip(absent).any(|h| h.ts.is_none()),
            "absent hops are the chunk's head"
        );
        self.absent = u32::try_from(absent).expect("adjacency chunk longer than u32::MAX hops");
        self.hops = hops
            .into_iter()
            .map(|h| AdjHop {
                ts: h.ts.unwrap_or(0),
                id: h.id,
            })
            .collect();
    }
}

/// What a fold pass does with one selected chunk — the decision the
/// selection makes, handed to the fold so the two never re-derive it.
#[derive(Clone, Copy, Debug)]
enum FoldAction {
    /// Merge the chunk's delta into its sealed stream, reclaiming what the
    /// horizon frees.
    Fold,
    /// The below-floor zero-hop region: reclaim only the tombstones that
    /// name no position any more, the delta hops stay.
    Reclaim,
}

/// One chunk a fold pass takes: its type, and the action the fold takes on
/// it — the pair [`TypedAdj::due_chunks`] and [`TypedAdj::seal_whole`]
/// hand to [`TypedAdj::fold_due`]. The fields are private: a candidate is a
/// decision token, read only by the selection that builds it and the fold
/// that consumes it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FoldCandidate {
    type_id: u32,
    action: FoldAction,
}

/// The action the fold takes on `chunk` at `horizon`, or `None` when the
/// chunk is not due (ADR-0551): a never-sealed chunk at the floor — the
/// knee's 32, or the adaptive [`STABILITY_SEAL_FLOOR`] for a chunk whose
/// length held since the previous pass — a delta past 25% of the sealed
/// hops, or a tombstone reclaimable at the GC horizon. One walk over the
/// chunk's sealed facts decides both, so due-ness and the action cannot
/// drift apart. A free function because it reads the chunk and the sealed
/// region together: the chunk's own policies stay self-only
/// ([`DeltaChunk::floor_admits_fold`]), the region-aware ones live with
/// [`TypedAdj`]'s.
fn due_action(
    chunk: &DeltaChunk,
    sealed: Option<SealedRegion<'_>>,
    horizon: u64,
) -> Option<FoldAction> {
    let type_id = chunk.type_id;
    let delta = chunk.hops.len();
    if let Some(region) = sealed {
        if region.has_chunk(type_id) {
            // A tombstone reclaimable at the horizon folds, a zero-hop
            // chunk's too: nothing else would ever reclaim it. A zero-hop
            // region the floor does not admit has no stream to merge, so
            // its reclaim is the fold's only work — the one Reclaim.
            if region.reclaimable(type_id, horizon) {
                let sealed_len = region.chunk_len(type_id);
                return Some(if sealed_len == 0 && !chunk.floor_admits_fold() {
                    FoldAction::Reclaim
                } else {
                    FoldAction::Fold
                });
            }
            let len = region.chunk_len(type_id);
            if len > 0 {
                return (delta * 4 > len).then_some(FoldAction::Fold);
            }
        }
    }
    // No chunk, or a zero-hop one (a never-sealed type that only recorded
    // a tombstone): the floor still gates the first seal (the ratio
    // against 0 sealed hops would seal every later hop on its own).
    chunk.floor_admits_fold().then_some(FoldAction::Fold)
}

/// The tombstones that still name a position, as the fold's two arms read
/// them: one whose id is in the sealed stream, or a delete's whose live
/// delta hop it will hide once that hop folds in. The reclaim arm passes an
/// empty sealed set — nothing is sealed for a type it reclaims — which
/// leaves exactly its own rule.
fn live_tombstones(
    tombstones: Vec<Tombstone>,
    sealed_ids: &UsedSet,
    delta_ids: &UsedSet,
) -> Vec<Tombstone> {
    tombstones
        .into_iter()
        .filter(|t| {
            sealed_ids.contains(&t.id) || (t.deleted.is_some() && delta_ids.contains(&t.id))
        })
        .collect()
}

/// Heap of one node's adjacency chunks and hop buffers — beside the two
/// strides it prices. The chunk-vector header is the id slot (ADR-0532),
/// so it is not charged again here. The chunk stride is the delta chunk
/// header — `type_id` plus the ts-presence boundary in the old
/// `(u32, Vec)` tuple's padding — so presence costs no entry bytes; the
/// hop stride is the raw 16-byte [`AdjHop`].
pub(crate) fn adjacency_bytes(chunk_hops: impl Iterator<Item = usize>) -> u64 {
    let chunk_stride = size_of::<DeltaChunk>() as u64;
    let hop_stride = size_of::<AdjHop>() as u64;
    let mut chunks = 0usize;
    let mut hop_bytes = 0u64;
    for n in chunk_hops {
        chunks += 1;
        hop_bytes += crate::memory::vec_heap(n, hop_stride);
    }
    crate::memory::vec_heap(chunks, chunk_stride) + hop_bytes
}

/// Per-node typed adjacency chunks (ADR-0031 / ADR-0033). Superset lists.
/// Each chunk is `ts` DESC with missing integer `ts` first (ADR-0035);
/// equal-`ts` hops are newest relationship first (ADR-0203 — see
/// `cmp_ts_desc`).
///
/// `chunks` is the live delta: inserts and repositions touch only it, at the
/// sorted-insert cost. A folded chunk's compressed immutable hop stream and
/// versioned tombstones live in the `Graph`-level [`SealedRegion`] side
/// table (ADR-0551 / ADR-0599), keyed by the same node and direction; the
/// delta entry stays (an empty hop vector) so the type keeps its first-seen
/// iteration slot. A read merges delta first, then the sealed stream, both
/// in `cmp_ts_desc` order.
#[derive(Clone, Debug, Default)]
pub(crate) struct TypedAdj {
    pub chunks: Vec<DeltaChunk>,
}

impl TypedAdj {
    /// Whether this delta holds no chunk at all — one spelling of the
    /// per-node emptiness predicate that decides whether a node keeps an
    /// entry in a table (the sealed half is the arena span's `len == 0`).
    pub(crate) fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    pub(crate) fn insert(&mut self, type_id: u32, rid: RelId, ts: Option<i64>) {
        let hop = Hop { ts, id: rid };
        if let Some(c) = self.chunk_mut(type_id) {
            insert_hop(c, hop);
        } else {
            self.push_chunk(type_id, vec![hop]);
        }
    }

    /// Append a whole chunk for `type_id`, fitting its hop vector and the
    /// chunk vector to their reserved capacities. The one place a chunk is
    /// installed as a unit: [`Self::insert`]'s create arm and
    /// [`Self::from_runs`] both land here, so the chunk shape — appended in
    /// first-seen order, hops reserved — has one owner.
    fn push_chunk(&mut self, type_id: u32, hops: Vec<Hop>) {
        let mut chunk = DeltaChunk {
            type_id,
            ..DeltaChunk::default()
        };
        chunk.set_hops(hops);
        crate::memory::fit_reserved(&mut chunk.hops);
        self.chunks.push(chunk);
        crate::memory::fit_reserved(&mut self.chunks);
    }

    /// Assemble a node's chunks wholesale from one run per type: each run
    /// carries the type id and its hops — at least one, the precondition
    /// the key derivation below asserts — in any order. The chunk-order
    /// key — the rel id that first created that type's chunk — is derived
    /// here from the hops, never trusted from the caller; each run's hops
    /// are sorted `cmp_ts_desc` and the chunks follow that first rel id,
    /// the shape the incremental [`Self::insert`] path leaves for the same
    /// hops, so a graph rebuilt wholesale and one built by inserts are the
    /// same adjacency.
    pub(crate) fn from_runs(runs: impl IntoIterator<Item = (u32, Vec<Hop>)>) -> Self {
        let mut runs: Vec<(RelId, u32, Vec<Hop>)> = runs
            .into_iter()
            .map(|(type_id, hops)| {
                let first = hops.iter().map(|hop| hop.id).min().expect("a run has hops");
                (first, type_id, hops)
            })
            .collect();
        runs.sort_unstable_by_key(|&(first, _, _)| first);
        let mut typed = Self::default();
        for (_, type_id, mut hops) in runs {
            debug_assert!(
                !typed.chunks.iter().any(|c| c.type_id == type_id),
                "one run per type"
            );
            hops.sort_unstable_by(cmp_ts_desc);
            typed.push_chunk(type_id, hops);
        }
        typed
    }

    pub(crate) fn floor_bytes(&self) -> u64 {
        adjacency_bytes(self.chunks.iter().map(|c| c.hops.len()))
    }

    pub(crate) fn reposition(
        &mut self,
        sealed: SealedRegion<'_>,
        type_id: u32,
        rid: RelId,
        ts: Option<i64>,
        xmax: u64,
    ) {
        // A sealed hop's old position is superseded first: it stays visible
        // to older snapshots through the versioned tombstone (ADR-0551).
        sealed.tombstone_deleted(type_id, rid, xmax, Supersede::Repositioned);
        // The remove-then-reinsert through the two hop policies: a chunk
        // the removal emptied is recreated by the insert below, so the
        // hop lands in a freshly ordered chunk either way.
        self.remove(Some(sealed), type_id, rid);
        self.insert(type_id, rid, ts);
    }

    /// Remove `rid`'s hop from `type_id`'s delta chunk, keeping the chunk's
    /// order and presence boundary, and drop the chunk when the hop was its
    /// last and no sealed stream covers the type — no phantom empty chunks
    /// for `out_chunks` / `hop_counts` to report (F-1-23). The GC's pruning
    /// arm (LABS-1116): a chain the version map no longer holds must leave
    /// adjacency too, or every later scan pays a `rels` map miss on the
    /// ghost hop. A missing hop is a no-op (the removal is idempotent by
    /// construction).
    pub(crate) fn remove(&mut self, sealed: Option<SealedRegion<'_>>, type_id: u32, rid: RelId) {
        let Some(ci) = self.chunks.iter().position(|c| c.type_id == type_id) else {
            return;
        };
        let chunk = &mut self.chunks[ci];
        let Some(i) = chunk.hops.iter().position(|h| h.id == rid) else {
            return;
        };
        chunk.hops.remove(i);
        // A removed absent hop shrinks the presence prefix by one.
        if i < chunk.absent as usize {
            chunk.absent -= 1;
        }
        let emptied = chunk.hops.is_empty();
        // The sealed *stream* holds the slot, not the sealed region: a
        // zero-hop chunk (a never-sealed type that only recorded a tombstone)
        // covers no hop, so the type's slot goes as it did before the
        // tombstone existed — and its empty region goes with it.
        if emptied && sealed.map_or(0, |s| s.chunk_len(type_id)) == 0 {
            self.chunks.remove(ci);
            if let Some(sealed) = sealed {
                // The drop arm appends no stream: the walls never refuse it.
                sealed.put_chunk(type_id, &[], Vec::new());
            }
        }
    }

    pub(crate) fn flatten(&self, sealed: Option<SealedRegion<'_>>) -> Vec<RelId> {
        // PHYSICAL_READ_TS (0) hides nothing: a tombstone's xmax is always
        // ≥ 1, so this is the raw physical adjacency in merged order.
        let mut ids = Vec::new();
        self.for_each_hop(sealed, PHYSICAL_READ_TS, |hop| ids.push(hop.id));
        ids
    }

    /// `type_id`'s delta hops with ts presence resolved — the read shape
    /// the assertions compare against; iteration proper goes through
    /// [`Self::for_each_hop_of`], which merges the sealed tier.
    #[cfg(test)]
    pub(crate) fn chunk_hops(&self, type_id: u32) -> Vec<Hop> {
        self.chunk_ref(type_id)
            .map_or_else(Vec::new, DeltaChunk::to_hops)
    }

    fn chunk_ref(&self, type_id: u32) -> Option<&DeltaChunk> {
        self.chunks.iter().find(|c| c.type_id == type_id)
    }

    /// Delta plus sealed hops of `type_id` — the degree/estimate length.
    pub(crate) fn total_len(&self, sealed: Option<SealedRegion<'_>>, type_id: u32) -> usize {
        self.chunk_ref(type_id).map_or(0, |c| c.hops.len())
            + sealed.map_or(0, |s| s.chunk_len(type_id))
    }

    /// Every chunk's type id with its live delta and sealed hop counts, in
    /// first-seen type order: the one per-chunk length pass. A folded chunk
    /// reports its delta (empty) and sealed tier separately; [`Self::total_len`]
    /// is the per-type twin.
    pub(crate) fn chunk_lens<'a>(
        &'a self,
        sealed: Option<SealedRegion<'a>>,
    ) -> impl Iterator<Item = (u32, usize, usize)> + 'a {
        self.chunks.iter().map(move |c| {
            (
                c.type_id,
                c.hops.len(),
                sealed.map_or(0, |s| s.chunk_len(c.type_id)),
            )
        })
    }

    /// Visit `type_id`'s merged hop stream at read timestamp `ts`: delta
    /// first, then the sealed stream, in `cmp_ts_desc` order, with the sealed
    /// hops a tombstone hides at `ts` dropped.
    pub(crate) fn for_each_hop_of(
        &self,
        sealed: Option<SealedRegion<'_>>,
        type_id: u32,
        ts: u64,
        mut f: impl FnMut(Hop),
    ) {
        visit_chunk(self.chunk_ref(type_id), sealed, type_id, ts, &mut f);
    }

    /// Visit every chunk's merged hop stream in first-seen type order.
    pub(crate) fn for_each_hop(
        &self,
        sealed: Option<SealedRegion<'_>>,
        ts: u64,
        mut f: impl FnMut(Hop),
    ) {
        for chunk in &self.chunks {
            visit_chunk(Some(chunk), sealed, chunk.type_id, ts, &mut f);
        }
    }

    /// The chunks the pass folds at `horizon`, each with the action the
    /// fold takes on it. The caller takes the node's sealed region only
    /// when this is non-empty, so a node that never folds has no entry.
    pub(crate) fn due_chunks(
        &self,
        sealed: Option<SealedRegion<'_>>,
        horizon: u64,
    ) -> Vec<FoldCandidate> {
        self.chunks
            .iter()
            .filter_map(|c| {
                due_action(c, sealed, horizon).map(|action| FoldCandidate {
                    type_id: c.type_id,
                    action,
                })
            })
            .collect()
    }

    /// Note every banded chunk's current length as its stability baseline —
    /// the value the next pass's [`due_action`] compares against. A chunk
    /// outside the band (ratio-gated, empty, or at the knee) needs no
    /// baseline.
    pub(crate) fn note_stable(&mut self, sealed: Option<SealedRegion<'_>>) {
        for chunk in &mut self.chunks {
            let sealed_len = sealed.map_or(0, |s| s.chunk_len(chunk.type_id));
            if chunk.in_stability_band(sealed_len) {
                chunk.stable_len = chunk.hops.len();
            }
        }
    }

    /// Seal a wholesale build's node in its one pass (ADR-0599 / ADR-0608)
    /// — the build's own gate, whose horizon is 0: a rebuild reclaims no
    /// tombstone the write path's horizon would have freed. The selection
    /// is the build's own: every chunk whose delta reaches
    /// [`RESTORE_SEAL_HOPS`], and all of them fold — a fresh build is
    /// all-delta, so the threshold alone decides, with no sealed-length
    /// ratio, tombstone horizon, or stability baseline, the write path's
    /// other gates.
    pub(crate) fn seal_whole(&mut self, sealed: SealedRegion<'_>) {
        let candidates: Vec<FoldCandidate> = self
            .chunks
            .iter()
            .filter(|c| c.hops.len() >= RESTORE_SEAL_HOPS)
            .map(|c| FoldCandidate {
                type_id: c.type_id,
                action: FoldAction::Fold,
            })
            .collect();
        self.fold_due(sealed, &candidates, 0);
    }

    /// Whether this node has anything to do at `horizon`: chunks the fold
    /// would take, or stability baselines to note — the one spelling of the
    /// pass's visit rule, so the graph side only asks and walks. One
    /// non-allocating walk: the candidates themselves are produced once,
    /// under the write guard, by [`Self::fold_at`].
    pub(crate) fn has_fold_work(&self, sealed: Option<SealedRegion<'_>>, horizon: u64) -> bool {
        self.chunks.iter().any(|c| {
            due_action(c, sealed, horizon).is_some()
                || c.in_stability_band(sealed.map_or(0, |s| s.chunk_len(c.type_id)))
        })
    }

    /// The node's whole fold pass at `horizon`, under the caller's write
    /// guard: select, fold, then note the survivors' lengths — the value
    /// the next pass's adaptive floor reads.
    pub(crate) fn fold_at(&mut self, sealed: SealedRegion<'_>, horizon: u64) {
        let candidates = self.due_chunks(Some(sealed), horizon);
        self.fold_due(sealed, &candidates, horizon);
        self.note_stable(Some(sealed));
    }

    /// Fold the selected chunks. Returns whether anything was folded.
    fn fold_due(
        &mut self,
        sealed: SealedRegion<'_>,
        candidates: &[FoldCandidate],
        horizon: u64,
    ) -> bool {
        let mut changed = false;
        for candidate in candidates {
            // A selected chunk is still there when its turn comes:
            // `fold_chunk` removes only the chunk it just folded, and the
            // selection holds each type id once.
            let ci = self
                .chunks
                .iter()
                .position(|c| c.type_id == candidate.type_id)
                .expect("a selected chunk is still present");
            changed |= self.fold_chunk(sealed, ci, horizon, candidate.action);
        }
        changed
    }

    /// Fold the chunk at `ci` by `action`. An empty delta with no
    /// reclaimable tombstone is a true no-op (no re-encode, ADR-0551).
    fn fold_chunk(
        &mut self,
        sealed: SealedRegion<'_>,
        ci: usize,
        horizon: u64,
        action: FoldAction,
    ) -> bool {
        // The chunk's own header, read once: the delta writes go through
        // `ci` and the sealed ones through this type id, so the two can
        // never name different types.
        let type_id = self.chunks[ci].type_id;
        match action {
            // The reclaim changes nothing in the delta — its hops name the
            // tombstones to keep — so the arm runs before any take.
            FoldAction::Reclaim => return self.reclaim_orphans(sealed, ci),
            FoldAction::Fold => {}
        }
        let mut delta = self.chunks[ci].take_hops();
        // `decoded` and `tombstones` answer empty for a type with no sealed
        // entry, so the absence contract needs no branch here.
        let (mut hops, tombstones) = (sealed.decoded(type_id), sealed.tombstones(type_id));

        if hops.is_empty() && delta.is_empty() && tombstones.is_empty() {
            return false;
        }
        let (sealed_hops, sealed_tombstones) = (hops.len(), tombstones.len());
        let kept = reclaim(&mut hops, &mut delta, tombstones, horizon);
        let sealed_ids: UsedSet = hops.iter().map(|h| h.id).collect();
        let delta_ids: UsedSet = delta.iter().map(|h| h.id).collect();
        let kept = live_tombstones(kept, &sealed_ids, &delta_ids);
        // A tombstone whose sealed stamp is below its delete stamp cannot
        // fold with the id's live delta hop: it would hide the hop from
        // every reader at or after the sealed stamp, and a reader between
        // the two stamps still needs it. That is a reposition (no delete
        // stamp), or a delete that followed one. A delete that superseded
        // the sealed position itself hides the hop no earlier than its own
        // delete, so the hop folds in under its tombstone (and the reclaim
        // drops both at the horizon).
        let reclaimed = hops.len() != sealed_hops || kept.len() != sealed_tombstones;
        if delta.iter().any(|h| {
            kept.iter()
                .any(|t| t.id == h.id && t.deleted != Some(t.xmax))
        }) {
            // The reclaim still applies to the sealed stream; only the delta
            // hop waits for its own delete stamp, so its hops stay delta.
            if !reclaimed {
                self.chunks[ci].set_hops(delta);
                return false;
            }
            let written = sealed.put_chunk(type_id, &hops, kept);
            self.chunks[ci].set_hops(delta);
            return written;
        }
        hops.extend_from_slice(&delta);
        hops.sort_unstable_by(cmp_ts_desc);
        if !sealed.put_chunk(type_id, &hops, kept) {
            // The walls refused the append (ADR-0599): the sealed stream is
            // untouched, and `hops` carries its old hops merged in, so the
            // delta goes back exactly as it came — never re-derived from
            // the merged list, which would duplicate the sealed hops.
            self.chunks[ci].set_hops(delta);
            return false;
        }
        if hops.is_empty() {
            // Fully reclaimed: the layout does not depend on when the fold
            // fired, and the type's iteration slot goes with it.
            self.chunks.remove(ci);
        } else {
            // The fold consumed the chunk's hops into the sealed stream, so
            // its stability baseline goes with them: a chunk that grows
            // again starts unnoted and must hold its new length before it
            // seals on stability.
            self.chunks[ci].stable_len = 0;
        }
        true
    }

    /// The reclaim arm's work ([`FoldAction::Reclaim`]): a zero-hop region
    /// is a never-sealed type that only recorded tombstones, so below the
    /// floor the fold is the reclaim of the tombstones that name no
    /// position any more — a hop the chain GC already pruned — and the
    /// region goes with the last one. The delta hops stay in the chunk,
    /// read in place: this arm never takes them.
    fn reclaim_orphans(&mut self, sealed: SealedRegion<'_>, ci: usize) -> bool {
        let type_id = self.chunks[ci].type_id;
        let tombstones = sealed.tombstones(type_id);
        let sealed_tombstones = tombstones.len();
        let delta_ids: UsedSet = self.chunks[ci].hops.iter().map(|h| h.id).collect();
        // Nothing is sealed for this type, so only a delete's tombstone can
        // still name a position.
        let kept = live_tombstones(tombstones, &UsedSet::default(), &delta_ids);
        if kept.len() == sealed_tombstones {
            // Nothing to reclaim: the region stays exactly as it was.
            return false;
        }
        sealed.put_chunk(type_id, &[], kept)
    }

    /// The mut twin of [`Self::chunk_ref`] — the one find-chunk spelling the
    /// in-place hop policies (`insert`, `reposition`) route through.
    fn chunk_mut(&mut self, type_id: u32) -> Option<&mut DeltaChunk> {
        self.chunks.iter_mut().find(|c| c.type_id == type_id)
    }
}

fn visit_chunk(
    delta: Option<&DeltaChunk>,
    sealed: Option<SealedRegion<'_>>,
    type_id: u32,
    ts: u64,
    f: &mut impl FnMut(Hop),
) {
    // A missing delta chunk (a type whose hops all sealed away) is the
    // empty hop slice: the sealed stream still reads, merged against
    // nothing.
    let (raw, absent): (&[AdjHop], u32) = delta.map_or((&[], 0), |c| (&c.hops, c.absent));
    match sealed {
        Some(region) => merge_chunk(raw, absent, &region.visible_hops(type_id, ts), &mut *f),
        None => {
            for i in 0..raw.len() {
                f(resolve(raw, absent, i));
            }
        }
    }
}

/// OpenCypher: NULL ranks after NUMBER in ASC, so first in DESC.
/// The `RelId` tie-break is DESCENDING (newest first): Neo4j inserts a new
/// relationship at the head of the node's relationship chain, so reference
/// expansions — and therefore its shortestPath tie-breaks and the row order
/// behind aggregation ties — see the newest relationship first (ADR-0203,
/// LABS-255/LABS-258).
pub(crate) fn cmp_ts_desc(a: &Hop, b: &Hop) -> std::cmp::Ordering {
    match (a.ts, b.ts) {
        (None, None) => b.id.cmp(&a.id),
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(ta), Some(tb)) => tb.cmp(&ta).then(b.id.cmp(&a.id)),
    }
}

/// Sorted insert of one hop, maintaining `cmp_ts_desc` order and the
/// chunk's presence boundary. The chunk's two sorted ranges — the absent
/// head by id DESC, the present tail by (ts, id) DESC — are `cmp_ts_desc`
/// partitioned by presence, so the probe searches the one range the hop
/// belongs to: an absent hop can only land inside the head (it orders
/// before every present hop) and grows the boundary by one; a present hop
/// lands in the tail and leaves it alone.
fn insert_hop(chunk: &mut DeltaChunk, hop: Hop) {
    let absent = chunk.absent as usize;
    let pos = match hop.ts {
        None => chunk.hops[..absent]
            .binary_search_by(|h| cmp_ts_desc(&Hop { ts: None, id: h.id }, &hop)),
        Some(_) => chunk.hops[absent..]
            .binary_search_by(|h| {
                cmp_ts_desc(
                    &Hop {
                        ts: Some(h.ts),
                        id: h.id,
                    },
                    &hop,
                )
            })
            .map(|i| i + absent)
            .map_err(|i| i + absent),
    };
    match pos {
        Ok(_) => {}
        Err(pos) => {
            chunk.hops.insert(
                pos,
                AdjHop {
                    ts: hop.ts.unwrap_or(0),
                    id: hop.id,
                },
            );
            chunk.absent += hop.ts.is_none() as u32;
            crate::memory::fit_reserved(&mut chunk.hops);
        }
    }
}

#[cfg(test)]
mod tests;
