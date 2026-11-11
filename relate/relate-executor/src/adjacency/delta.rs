//! The live delta tier of the adjacency store (ADR-0031 / ADR-0033): the
//! per-node typed hop chunks in `ts`-DESC order, and the fold that moves
//! them into the node's sealed side-table region of [`super`] (ADR-0551).

use relate_core::RelId;

use crate::UsedSet;

use super::{
    merge_chunk, reclaim, SealedAdj, SealedChunk, Supersede, Tombstone, COMPACTION_DELTA_FLOOR,
};

/// One hop in a typed chunk. Ordered for `ORDER BY ts DESC` (ADR-0035).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AdjHop {
    pub ts: Option<i64>,
    pub id: RelId,
}

/// Per-node typed adjacency chunks (ADR-0031 / ADR-0033). Superset lists.
/// Each chunk is `ts` DESC with missing integer `ts` first (ADR-0035);
/// equal-`ts` hops are newest relationship first (ADR-0203 — see
/// `cmp_ts_desc`).
///
/// `chunks` is the live delta: inserts and repositions touch only it, at the
/// sorted-insert cost. A folded chunk's compressed immutable hop stream and
/// versioned tombstones live in the `Graph`-level [`SealedAdj`] side table
/// (ADR-0551), keyed by the same node and direction; the delta entry stays
/// (an empty hop vector) so the type keeps its first-seen iteration slot. A
/// read merges delta first, then the sealed stream, both in `cmp_ts_desc`
/// order.
#[derive(Clone, Debug, Default)]
pub(crate) struct TypedAdj {
    pub chunks: Vec<(u32, Vec<AdjHop>)>,
}

impl TypedAdj {
    pub(crate) fn insert(&mut self, type_id: u32, rid: RelId, ts: Option<i64>) {
        let hop = AdjHop { ts, id: rid };
        if let Some(v) = self.chunk_mut(type_id) {
            insert_hop(v, hop);
        } else {
            self.push_chunk(type_id, vec![hop]);
        }
    }

    /// Append a whole chunk for `type_id`, fitting its hop vector and the
    /// chunk vector to their reserved capacities. The one place a chunk is
    /// installed as a unit: [`Self::insert`]'s create arm and
    /// [`Self::from_runs`] both land here, so the chunk shape — appended in
    /// first-seen order, hops reserved — has one owner.
    fn push_chunk(&mut self, type_id: u32, mut hops: Vec<AdjHop>) {
        crate::memory::fit_reserved(&mut hops);
        self.chunks.push((type_id, hops));
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
    pub(crate) fn from_runs(runs: impl IntoIterator<Item = (u32, Vec<AdjHop>)>) -> Self {
        let mut runs: Vec<(RelId, u32, Vec<AdjHop>)> = runs
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
                !typed.chunks.iter().any(|(t, _)| *t == type_id),
                "one run per type"
            );
            hops.sort_unstable_by(cmp_ts_desc);
            typed.push_chunk(type_id, hops);
        }
        typed
    }

    pub(crate) fn floor_bytes(&self) -> u64 {
        let hops: Vec<usize> = self.chunks.iter().map(|(_, v)| v.len()).collect();
        crate::memory::adjacency_bytes(&hops)
    }

    pub(crate) fn reposition(
        &mut self,
        sealed: &mut SealedAdj,
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
        if let Some(v) = self.chunk_mut(type_id) {
            insert_hop(v, AdjHop { ts, id: rid });
        } else {
            self.insert(type_id, rid, ts);
        }
    }

    /// Remove `rid`'s hop from `type_id`'s delta chunk, keeping the chunk's
    /// order, and drop the chunk when the hop was its last and no sealed
    /// stream covers the type — no phantom empty chunks for `out_chunks` /
    /// `hop_counts` to report (F-1-23). The GC's pruning arm (LABS-1116): a
    /// chain the version map no longer holds must leave adjacency too, or
    /// every later scan pays a `rels` map miss on the ghost hop. A missing
    /// hop is a no-op (the removal is idempotent by construction).
    pub(crate) fn remove(&mut self, sealed: Option<&mut SealedAdj>, type_id: u32, rid: RelId) {
        let Some(ci) = self.chunks.iter().position(|(t, _)| *t == type_id) else {
            return;
        };
        let v = &mut self.chunks[ci].1;
        let Some(i) = v.iter().position(|h| h.id == rid) else {
            return;
        };
        v.remove(i);
        let emptied = v.is_empty();
        // The sealed *stream* holds the slot, not the sealed region: a
        // zero-hop chunk (a never-sealed type that only recorded a tombstone)
        // covers no hop, so the type's slot goes as it did before the
        // tombstone existed — and its empty region goes with it.
        if emptied && sealed.as_ref().map_or(0, |s| s.chunk_len(type_id)) == 0 {
            self.chunks.remove(ci);
            if let Some(sealed) = sealed {
                sealed.put_chunk(type_id, &[], Vec::new());
            }
        }
    }

    pub(crate) fn flatten(&self, sealed: Option<&SealedAdj>) -> Vec<RelId> {
        // ts 0 hides nothing: a tombstone's xmax is always ≥ 1, so this is
        // the raw physical adjacency in merged order.
        let mut ids = Vec::new();
        self.for_each_hop(sealed, 0, |hop| ids.push(hop.id));
        ids
    }

    pub(crate) fn chunk(&self, type_id: u32) -> &[AdjHop] {
        self.chunks
            .iter()
            .find(|(t, _)| *t == type_id)
            .map(|(_, v)| v.as_slice())
            .unwrap_or(&[])
    }

    /// Delta plus sealed hops of `type_id` — the degree/estimate length.
    pub(crate) fn total_len(&self, sealed: Option<&SealedAdj>, type_id: u32) -> usize {
        self.chunk(type_id).len() + sealed.map_or(0, |s| s.chunk_len(type_id))
    }

    /// Every chunk's type id with its live delta and sealed hop counts, in
    /// first-seen type order: the one per-chunk length pass. A folded chunk
    /// reports its delta (empty) and sealed tier separately; [`Self::total_len`]
    /// is the per-type twin.
    pub(crate) fn chunk_lens<'a>(
        &'a self,
        sealed: Option<&'a SealedAdj>,
    ) -> impl Iterator<Item = (u32, usize, usize)> + 'a {
        self.chunks.iter().map(move |(type_id, delta)| {
            (
                *type_id,
                delta.len(),
                sealed.map_or(0, |s| s.chunk_len(*type_id)),
            )
        })
    }

    /// Visit `type_id`'s merged hop stream at read timestamp `ts`: delta
    /// first, then the sealed stream, in `cmp_ts_desc` order, with the sealed
    /// hops a tombstone hides at `ts` dropped.
    pub(crate) fn for_each_hop_of(
        &self,
        sealed: Option<&SealedAdj>,
        type_id: u32,
        ts: u64,
        mut f: impl FnMut(AdjHop),
    ) {
        visit_chunk(
            self.chunk(type_id),
            sealed.and_then(|s| s.chunk(type_id)),
            ts,
            &mut f,
        );
    }

    /// Visit every chunk's merged hop stream in first-seen type order.
    pub(crate) fn for_each_hop(
        &self,
        sealed: Option<&SealedAdj>,
        ts: u64,
        mut f: impl FnMut(AdjHop),
    ) {
        for (type_id, delta) in &self.chunks {
            visit_chunk(delta, sealed.and_then(|s| s.chunk(*type_id)), ts, &mut f);
        }
    }

    /// Whether a chunk is due for its fold at `horizon` (ADR-0551): a
    /// never-sealed chunk at the floor, a delta past 25% of the sealed hops,
    /// or a tombstone reclaimable at the GC horizon.
    pub(crate) fn chunk_due(&self, sealed: Option<&SealedAdj>, type_id: u32, horizon: u64) -> bool {
        let delta = self.chunk(type_id).len();
        match sealed.and_then(|s| s.chunk(type_id)) {
            // A tombstone reclaimable at the horizon folds, a zero-hop
            // chunk's too: nothing else would ever reclaim it.
            Some(chunk) if chunk.reclaimable(horizon) => true,
            Some(chunk) if chunk.len() > 0 => delta * 4 > chunk.len(),
            // No chunk, or a zero-hop one (a never-sealed type that only
            // recorded a tombstone): the floor still gates the first seal
            // (the ratio against 0 sealed hops would seal every later hop
            // on its own).
            _ => delta >= COMPACTION_DELTA_FLOOR,
        }
    }

    /// The type ids whose chunks are due at `horizon`. The caller takes the
    /// node's sealed side-table entry only when this is non-empty, so a node
    /// that never folds has no entry.
    pub(crate) fn due_chunks(&self, sealed: Option<&SealedAdj>, horizon: u64) -> Vec<u32> {
        self.chunks
            .iter()
            .map(|(type_id, _)| *type_id)
            .filter(|type_id| self.chunk_due(sealed, *type_id, horizon))
            .collect()
    }

    /// Fold `due`'s chunks. Returns whether anything was folded.
    pub(crate) fn fold_due(&mut self, sealed: &mut SealedAdj, due: &[u32], horizon: u64) -> bool {
        let mut changed = false;
        for type_id in due {
            if let Some(ci) = self.chunks.iter().position(|(t, _)| t == type_id) {
                changed |= self.fold_chunk(sealed, ci, *type_id, horizon);
            }
        }
        changed
    }

    /// Fold `type_id`'s delta and reclaimed tombstones into its sealed
    /// stream. An empty delta with no reclaimable tombstone is a true no-op
    /// (no re-encode, ADR-0551).
    fn fold_chunk(
        &mut self,
        sealed: &mut SealedAdj,
        ci: usize,
        type_id: u32,
        horizon: u64,
    ) -> bool {
        let mut delta = std::mem::take(&mut self.chunks[ci].1);
        let (mut hops, tombstones) = match sealed.chunk(type_id) {
            Some(chunk) => (chunk.decoded(), chunk.tombstones.clone()),
            None => (Vec::new(), Vec::new()),
        };

        if hops.is_empty() && delta.is_empty() && tombstones.is_empty() {
            return false;
        }
        let (sealed_hops, sealed_tombstones) = (hops.len(), tombstones.len());
        // A zero-hop region is a never-sealed type that only recorded
        // tombstones: there is no sealed stream to fold into, so below the
        // floor the fold is the reclaim of the tombstones that name no
        // position any more — a hop the chain GC already pruned — and the
        // region goes with the last one. The delta hops stay as they are.
        if hops.is_empty() && delta.len() < COMPACTION_DELTA_FLOOR {
            let delta_ids: UsedSet = delta.iter().map(|h| h.id).collect();
            let kept: Vec<Tombstone> = tombstones
                .into_iter()
                .filter(|t| t.deleted.is_some() && delta_ids.contains(&t.id))
                .collect();
            let reclaimed = kept.len() != sealed_tombstones;
            if reclaimed {
                sealed.put_chunk(type_id, &[], kept);
            }
            self.chunks[ci].1 = delta;
            return reclaimed;
        }
        let kept = reclaim(&mut hops, &mut delta, tombstones, horizon);
        // A tombstone that names no position any more is dropped: a
        // reposition's whose id never sealed (it would hide the live delta
        // hop once that folds in), and a delete's whose sealed position and
        // delta hop are both gone.
        let sealed_ids: UsedSet = hops.iter().map(|h| h.id).collect();
        let delta_ids: UsedSet = delta.iter().map(|h| h.id).collect();
        let kept: Vec<Tombstone> = kept
            .into_iter()
            .filter(|t| {
                sealed_ids.contains(&t.id) || (t.deleted.is_some() && delta_ids.contains(&t.id))
            })
            .collect();
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
            // hop waits for its own delete stamp.
            if reclaimed {
                sealed.put_chunk(type_id, &hops, kept);
            }
            self.chunks[ci].1 = delta;
            return reclaimed;
        }
        hops.extend(delta);
        hops.sort_unstable_by(cmp_ts_desc);
        if hops.is_empty() {
            // Fully reclaimed: the layout does not depend on when the fold
            // fired, and the type's iteration slot goes with it.
            sealed.put_chunk(type_id, &hops, kept);
            self.chunks.remove(ci);
            return true;
        }
        sealed.put_chunk(type_id, &hops, kept);
        true
    }

    /// The mut twin of [`Self::chunk`] — the one find-chunk spelling the
    /// in-place hop policies (`insert`, `reposition`) route through.
    fn chunk_mut(&mut self, type_id: u32) -> Option<&mut Vec<AdjHop>> {
        self.chunks
            .iter_mut()
            .find(|(t, _)| *t == type_id)
            .map(|(_, v)| v)
    }
}

fn visit_chunk(
    delta: &[AdjHop],
    sealed: Option<&SealedChunk>,
    ts: u64,
    f: &mut impl FnMut(AdjHop),
) {
    match sealed {
        Some(sealed) => merge_chunk(delta, &sealed.visible_hops(ts), &mut *f),
        None => {
            for hop in delta {
                f(*hop);
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
pub(crate) fn cmp_ts_desc(a: &AdjHop, b: &AdjHop) -> std::cmp::Ordering {
    match (a.ts, b.ts) {
        (None, None) => b.id.cmp(&a.id),
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(ta), Some(tb)) => tb.cmp(&ta).then(b.id.cmp(&a.id)),
    }
}

fn insert_hop(v: &mut Vec<AdjHop>, hop: AdjHop) {
    match v.binary_search_by(|h| cmp_ts_desc(h, &hop)) {
        Ok(_) => {}
        Err(pos) => {
            v.insert(pos, hop);
            crate::memory::fit_reserved(v);
        }
    }
}

#[cfg(test)]
mod tests;
