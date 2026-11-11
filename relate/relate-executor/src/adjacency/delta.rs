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
            let mut hops = vec![hop];
            crate::memory::fit_reserved(&mut hops);
            self.chunks.push((type_id, hops));
            crate::memory::fit_reserved(&mut self.chunks);
        }
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
mod tests {
    use super::*;

    /// The production fold's two steps: due-ness against the node's sealed
    /// entry, then the fold into it. Returns whether anything was folded.
    fn fold(a: &mut TypedAdj, sealed: &mut SealedAdj, horizon: u64) -> bool {
        let due = a.due_chunks(Some(sealed), horizon);
        if due.is_empty() {
            return false;
        }
        a.fold_due(sealed, &due, horizon)
    }

    #[test]
    fn typed_adj_chunks() {
        let mut a = TypedAdj::default();
        a.insert(1, 10, None);
        a.insert(2, 20, None);
        a.insert(1, 11, None);
        // Newest-first within a chunk (ADR-0203): the Neo4j reference
        // expands a node's relationship chain head-first, so the
        // highest-id relationship of a type comes first.
        assert_eq!(
            a.chunk(1).iter().map(|h| h.id).collect::<Vec<_>>(),
            vec![11, 10]
        );
        assert_eq!(a.flatten(None), vec![11, 10, 20]);
    }

    /// `TypedAdj` is the delta alone: the sealed tier lives in the Graph's
    /// side table, so the slot is back to the pre-feature 24-byte vector and
    /// a node without a fold carries no sealed pointer.
    #[test]
    fn typed_adj_slot_is_the_24_byte_delta() {
        use std::mem::size_of;
        assert_eq!(size_of::<TypedAdj>(), 24);
        assert_eq!(size_of::<Option<TypedAdj>>(), 24);
    }

    #[test]
    fn typed_adj_orders_ts_desc_nulls_first() {
        let mut a = TypedAdj::default();
        let mut sealed = SealedAdj::default();
        a.insert(1, 1, Some(10));
        a.insert(1, 2, Some(30));
        a.insert(1, 3, None);
        a.insert(1, 4, Some(20));
        let ids: Vec<_> = a.chunk(1).iter().map(|h| (h.ts, h.id)).collect();
        assert_eq!(
            ids,
            vec![(None, 3), (Some(30), 2), (Some(20), 4), (Some(10), 1)]
        );
        a.reposition(&mut sealed, 1, 1, Some(40), 0);
        let ids: Vec<_> = a.chunk(1).iter().map(|h| (h.ts, h.id)).collect();
        assert_eq!(
            ids,
            vec![(None, 3), (Some(40), 1), (Some(30), 2), (Some(20), 4)]
        );
    }

    /// The one per-chunk length pass reports each tier separately: a folded
    /// chunk shows an empty delta and its sealed length, and a hop inserted
    /// after the seal shows up in the delta.
    #[test]
    fn chunk_lens_yields_delta_and_sealed_per_chunk() {
        let mut a = TypedAdj::default();
        let mut sealed = SealedAdj::default();
        a.insert(1, 10, None);
        a.insert(2, 20, None);
        for i in 0..COMPACTION_DELTA_FLOOR as i64 {
            a.insert(3, 100 + i, Some(i));
        }
        assert!(fold(&mut a, &mut sealed, 0));

        let lens: Vec<_> = a.chunk_lens(Some(&sealed)).collect();
        assert_eq!(
            lens,
            vec![(1, 1, 0), (2, 1, 0), (3, 0, COMPACTION_DELTA_FLOOR)]
        );

        a.insert(3, 500, Some(500));
        assert_eq!(
            a.chunk_lens(Some(&sealed)).nth(2),
            Some((3, 1, COMPACTION_DELTA_FLOOR))
        );
    }

    /// The floor seals a never-sealed chunk, and a read merges the delta
    /// (an older-ts hop inserted after the seal) into its sorted slot ahead
    /// of the sealed stream (ADR-0551).
    #[test]
    fn sealed_and_delta_hops_merge_ts_desc_delta_first() {
        let mut a = TypedAdj::default();
        let mut sealed = SealedAdj::default();
        let type_id = 7;
        for i in 0..COMPACTION_DELTA_FLOOR as i64 {
            a.insert(type_id, i, Some(i));
        }
        assert!(a.chunk_due(Some(&sealed), type_id, u64::MAX));
        assert!(fold(&mut a, &mut sealed, u64::MAX));
        assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR);
        assert!(a.chunk(type_id).is_empty(), "the delta folded");

        // A hop older than every sealed hop must merge into its sorted slot,
        // not trail the sealed stream.
        a.insert(type_id, 100, Some(5));
        let mut seen = Vec::new();
        a.for_each_hop_of(Some(&sealed), type_id, u64::MAX, |hop| {
            seen.push((hop.ts, hop.id))
        });
        let mut expected: Vec<(Option<i64>, RelId)> = (0..COMPACTION_DELTA_FLOOR as i64)
            .map(|i| (Some(i), i))
            .collect();
        expected.push((Some(5), 100));
        expected.sort_unstable_by(|x, y| {
            cmp_ts_desc(&AdjHop { ts: x.0, id: x.1 }, &AdjHop { ts: y.0, id: y.1 })
        });
        assert_eq!(seen, expected);
    }

    /// A delete of a sealed hop records the versioned tombstone: the sealed
    /// stream keeps the physical hop, and a read before `xmax` still sees it
    /// (ADR-0551).
    #[test]
    fn delete_of_a_sealed_hop_records_a_versioned_tombstone() {
        let mut a = TypedAdj::default();
        let mut sealed = SealedAdj::default();
        let type_id = 3;
        for i in 0..COMPACTION_DELTA_FLOOR as i64 {
            a.insert(type_id, i, Some(i));
        }
        fold(&mut a, &mut sealed, 0);
        assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR);

        a.remove(Some(&mut sealed), type_id, 5);
        sealed.tombstone_deleted(type_id, 5, 40, Supersede::Deleted);
        let chunk = sealed.chunk(type_id).expect("sealed");
        assert_eq!(
            chunk.tombstones,
            [Tombstone {
                id: 5,
                xmax: 40,
                deleted: Some(40),
            }]
        );

        let mut before = Vec::new();
        a.for_each_hop_of(Some(&sealed), type_id, 39, |hop| before.push(hop.id));
        assert!(before.contains(&5), "a snapshot before xmax sees the hop");
        let mut after = Vec::new();
        a.for_each_hop_of(Some(&sealed), type_id, 40, |hop| after.push(hop.id));
        assert!(!after.contains(&5), "the tombstone hides it at xmax");
        assert_eq!(
            sealed.chunk_len(type_id),
            COMPACTION_DELTA_FLOOR,
            "still physical"
        );
    }

    /// The three fold conditions (ADR-0551), each on its own chunk: a
    /// never-sealed chunk at the floor, a delta past 25% of the sealed hops,
    /// and a tombstone reclaimable at the GC horizon. A chunk below its gate
    /// stays delta.
    #[test]
    fn compaction_folds_on_floor_ratio_and_reclaimable_tombstone() {
        let type_id = 9;
        // 1. Floor: a never-sealed chunk seals at `COMPACTION_DELTA_FLOOR`.
        let mut a = TypedAdj::default();
        let mut sealed = SealedAdj::default();
        for i in 0..(COMPACTION_DELTA_FLOOR - 1) as i64 {
            a.insert(type_id, i, Some(i));
        }
        assert!(
            !a.chunk_due(Some(&sealed), type_id, 0),
            "one hop below the floor"
        );
        a.insert(type_id, 1000, Some(1000));
        assert!(a.chunk_due(Some(&sealed), type_id, 0), "the floor");
        assert!(fold(&mut a, &mut sealed, 0));
        assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR);

        // 2. Ratio: a delta past 25% of the sealed hops folds.
        for i in 0..(COMPACTION_DELTA_FLOOR / 4) as i64 {
            a.insert(type_id, 2000 + i, Some(2000 + i));
        }
        assert!(
            !a.chunk_due(Some(&sealed), type_id, 0),
            "exactly 25% of the sealed hops is not past it"
        );
        a.insert(type_id, 3000, Some(3000));
        assert!(
            a.chunk_due(Some(&sealed), type_id, 0),
            "past 25% of the sealed hops"
        );
        assert!(fold(&mut a, &mut sealed, 0));
        assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR + 9);

        // 3. Tombstone: a sealed hop reclaimable at the horizon folds away
        // even with an empty delta.
        sealed.tombstone_deleted(type_id, 5, 40, Supersede::Deleted);
        assert!(
            !a.chunk_due(Some(&sealed), type_id, 39),
            "the tombstone is not yet at the horizon"
        );
        assert!(
            a.chunk_due(Some(&sealed), type_id, 40),
            "the tombstone is reclaimable"
        );
        assert!(fold(&mut a, &mut sealed, 40));
        assert!(sealed.chunk(type_id).unwrap().tombstones.is_empty());
        assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR + 8);
        assert!(
            !a.chunk_due(Some(&sealed), type_id, 41),
            "a clean sealed chunk is not due"
        );
    }

    /// The fold is idempotent and reclaims a tombstoned hop at the horizon.
    #[test]
    fn compaction_reclaims_at_the_horizon_and_is_idempotent() {
        let mut a = TypedAdj::default();
        let mut sealed = SealedAdj::default();
        let type_id = 2;
        for i in 0..COMPACTION_DELTA_FLOOR as i64 {
            a.insert(type_id, i, Some(i));
        }
        assert!(fold(&mut a, &mut sealed, 0));
        sealed.tombstone_deleted(type_id, 5, 40, Supersede::Deleted);
        // Below the horizon nothing is due again.
        assert!(!a.chunk_due(Some(&sealed), type_id, 39));
        assert!(!fold(&mut a, &mut sealed, 39));
        // At the horizon the tombstone is reclaimable and the fold drops it.
        assert!(fold(&mut a, &mut sealed, 40));
        let chunk = sealed.chunk(type_id).expect("still sealed");
        assert!(chunk.tombstones.is_empty());
        assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR - 1);
        assert!(
            !a.chunk_due(Some(&sealed), type_id, 41),
            "a clean sealed chunk is not due"
        );
    }

    /// The fold drops a deleted row's delta hop once its tombstone reaches
    /// the horizon, instead of sealing a hop the chain GC can no longer
    /// prune. Below the horizon it seals under its tombstone, so an older
    /// snapshot still sees it.
    #[test]
    fn the_fold_drops_a_deleted_rows_delta_hop_at_the_horizon() {
        let mut a = TypedAdj::default();
        let mut sealed = SealedAdj::default();
        let type_id = 4;
        for i in 0..COMPACTION_DELTA_FLOOR as i64 {
            a.insert(type_id, i, Some(i));
        }
        assert!(fold(&mut a, &mut sealed, 0));
        a.insert(type_id, 100, Some(100));
        sealed.tombstone_deleted(type_id, 100, 40, Supersede::Deleted);
        for i in 0..9 {
            a.insert(type_id, 200 + i, Some(200 + i));
        }
        assert!(
            fold(&mut a, &mut sealed, 0),
            "the delta is past 25% of the sealed hops"
        );
        assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR + 10);
        assert!(sealed
            .chunk(type_id)
            .unwrap()
            .decoded()
            .iter()
            .any(|h| h.id == 100));

        assert!(
            fold(&mut a, &mut sealed, 40),
            "the tombstone is at the horizon"
        );
        assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR + 9);
        assert!(!sealed
            .chunk(type_id)
            .unwrap()
            .decoded()
            .iter()
            .any(|h| h.id == 100));
        assert!(sealed.chunk(type_id).unwrap().tombstones.is_empty());
    }

    /// A delete on a never-sealed chunk records its tombstone against a
    /// zero-hop sealed stream created on demand, so the fold drops the dead
    /// delta hop instead of sealing it as a ghost (ADR-0551). The floor still
    /// gates that chunk's first seal, and the fold drops or keeps the delta
    /// hop per the tombstone's two version stamps.
    #[test]
    fn a_never_sealed_chunk_records_its_tombstone_and_folds_the_dead_hop_away() {
        // The tombstone does not seal a small chunk early: the floor gates a
        // never-sealed chunk's first seal.
        let mut small = TypedAdj::default();
        let mut small_sealed = SealedAdj::default();
        for i in 0..3 {
            small.insert(11, i, Some(i));
        }
        small_sealed.tombstone_deleted(11, 1, 40, Supersede::Deleted);
        assert!(
            !small.chunk_due(Some(&small_sealed), 11, 39),
            "the floor gates the first seal while the tombstone is live"
        );
        // At the horizon the fold is the reclaim alone: the tombstone still
        // names the delta hop, so nothing is reclaimed and nothing seals.
        assert!(!fold(&mut small, &mut small_sealed, u64::MAX));
        assert_eq!(small_sealed.chunk_len(11), 0, "no early seal");

        let mut a = TypedAdj::default();
        let mut sealed = SealedAdj::default();
        let type_id = 11;
        for i in 0..(COMPACTION_DELTA_FLOOR + 1) as i64 {
            a.insert(type_id, i, Some(i));
        }
        assert!(sealed.chunk(type_id).is_none(), "the chunk is still delta");

        sealed.tombstone_deleted(type_id, 5, 40, Supersede::Deleted);
        let chunk = sealed
            .chunk(type_id)
            .expect("the tombstone creates the sealed chunk");
        assert_eq!(
            chunk.bytes,
            crate::adjacency::encode_chunk(&[]),
            "the 4-byte empty stream"
        );
        assert_eq!(
            chunk.tombstones,
            [Tombstone {
                id: 5,
                xmax: 40,
                deleted: Some(40),
            }]
        );
        assert!(
            a.chunk_due(Some(&sealed), type_id, 39),
            "the delta reaches the floor"
        );

        // Below the horizon the fold seals the dead hop under its tombstone,
        // so an older snapshot still sees it.
        assert!(fold(&mut a, &mut sealed, 39));
        assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR + 1);
        let mut before = Vec::new();
        a.for_each_hop_of(Some(&sealed), type_id, 39, |hop| before.push(hop.id));
        assert!(before.contains(&5));

        assert!(
            a.chunk_due(Some(&sealed), type_id, 40),
            "the tombstone is reclaimable"
        );
        assert!(fold(&mut a, &mut sealed, 40));
        assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR);
        assert!(sealed.chunk(type_id).unwrap().tombstones.is_empty());
        assert!(!a.flatten(Some(&sealed)).contains(&5), "no ghost hop");
    }

    /// A never-sealed chunk's reposition, then delete, keeps the relocated
    /// delta hop at a horizon between the two stamps: the sealed position
    /// (none here) reclaims while the live hop waits for its delete stamp.
    #[test]
    fn a_never_sealed_reposition_then_delete_keeps_the_relocated_hop() {
        let mut a = TypedAdj::default();
        let mut sealed = SealedAdj::default();
        let type_id = 13;
        for i in 0..(COMPACTION_DELTA_FLOOR + 1) as i64 {
            a.insert(type_id, i, Some(i));
        }
        a.reposition(&mut sealed, type_id, 5, Some(40), 40);
        sealed.tombstone_deleted(type_id, 5, 60, Supersede::Deleted);

        // A horizon between the two stamps changes nothing: the relocated hop
        // is the id's live position, so the fold guard keeps it in the delta.
        fold(&mut a, &mut sealed, 40);
        assert_eq!(a.chunk(type_id).len(), COMPACTION_DELTA_FLOOR + 1);
        assert_eq!(sealed.chunk_len(type_id), 0);
        assert_eq!(
            sealed.chunk(type_id).unwrap().tombstones,
            [Tombstone {
                id: 5,
                xmax: 40,
                deleted: Some(60),
            }]
        );

        assert!(
            fold(&mut a, &mut sealed, 60),
            "the delete stamp reaches the horizon"
        );
        assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR);
        assert!(!a.flatten(Some(&sealed)).contains(&5));
    }

    /// The fold guard blocks only a reposition whose id still holds a sealed
    /// position. A reposition (or delete) of a delta-only hop records a
    /// tombstone the fold resolves, so it never wedges the chunk; a sealed
    /// hop's tombstone would hide the id's live delta hop and holds the fold
    /// until the horizon passes.
    #[test]
    fn the_fold_guard_only_blocks_a_reposition_of_a_sealed_hop() {
        let mut a = TypedAdj::default();
        let mut sealed = SealedAdj::default();
        let type_id = 8;
        for i in 0..COMPACTION_DELTA_FLOOR as i64 {
            a.insert(type_id, i, Some(i));
        }
        assert!(fold(&mut a, &mut sealed, 0));

        // A delta-only hop repositioned inside the sealed chunk: the
        // tombstone names no sealed position and is dropped, and the hop
        // folds in as the id's live position.
        a.insert(type_id, 100, Some(1));
        a.reposition(&mut sealed, type_id, 100, Some(2), 40);
        for i in 0..9 {
            a.insert(type_id, 200 + i, Some(200 + i));
        }
        assert!(fold(&mut a, &mut sealed, 0));
        assert!(sealed.chunk(type_id).unwrap().tombstones.is_empty());
        let mut seen = Vec::new();
        a.for_each_hop_of(Some(&sealed), type_id, u64::MAX, |hop| {
            seen.push((hop.ts, hop.id))
        });
        assert!(seen.contains(&(Some(2), 100)));

        // A sealed hop's reposition holds the fold while its tombstone is
        // below the horizon: a bare-id tombstone cannot name one position.
        a.reposition(&mut sealed, type_id, 3, Some(33), 40);
        for i in 0..10 {
            a.insert(type_id, 300 + i, Some(300 + i));
        }
        assert!(
            !fold(&mut a, &mut sealed, 0),
            "the reposition holds the fold"
        );
        assert!(
            fold(&mut a, &mut sealed, 40),
            "past the horizon the fold runs"
        );
        let mut seen = Vec::new();
        a.for_each_hop_of(Some(&sealed), type_id, u64::MAX, |hop| {
            seen.push((hop.ts, hop.id))
        });
        assert!(seen.contains(&(Some(33), 3)), "the live position folds in");
        assert!(!seen.contains(&(Some(3), 3)), "the sealed position is gone");
        assert!(sealed.chunk(type_id).unwrap().tombstones.is_empty());
    }

    /// The fold guard blocks a delete that followed a reposition while its
    /// delete stamp is above the horizon: folding the relocated hop under the
    /// tombstone would hide it from a reader between the two stamps. The
    /// sealed position still reclaims at the reposition's stamp.
    #[test]
    fn the_fold_guard_blocks_a_delete_above_the_horizon_after_a_reposition() {
        let mut a = TypedAdj::default();
        let mut sealed = SealedAdj::default();
        let type_id = 12;
        for i in 0..COMPACTION_DELTA_FLOOR as i64 {
            a.insert(type_id, i, Some(i));
        }
        assert!(fold(&mut a, &mut sealed, 0));
        // The sealed position is superseded at 40; the relocated hop is
        // deleted at 60.
        a.reposition(&mut sealed, type_id, 3, Some(40), 40);
        sealed.tombstone_deleted(type_id, 3, 60, Supersede::Deleted);
        for i in 0..10 {
            a.insert(type_id, 300 + i, Some(300 + i));
        }
        assert!(
            !fold(&mut a, &mut sealed, 0),
            "nothing reclaims below both stamps"
        );
        assert!(
            fold(&mut a, &mut sealed, 40),
            "the sealed position reclaims, the relocated hop stays"
        );
        assert!(
            !fold(&mut a, &mut sealed, 40),
            "the second pass has nothing left to do"
        );
        {
            let chunk = sealed.chunk(type_id).unwrap();
            assert_eq!(chunk.len(), COMPACTION_DELTA_FLOOR - 1);
            assert_eq!(a.chunk(type_id).len(), 11, "the relocated hop stays delta");
            assert_eq!(
                chunk.tombstones,
                [Tombstone {
                    id: 3,
                    xmax: 40,
                    deleted: Some(60),
                }]
            );
        }
        assert!(
            fold(&mut a, &mut sealed, 60),
            "the delete stamp reaches the horizon"
        );
        assert_eq!(sealed.chunk_len(type_id), COMPACTION_DELTA_FLOOR + 9);
        assert!(sealed.chunk(type_id).unwrap().tombstones.is_empty());
        let mut seen = Vec::new();
        a.for_each_hop_of(Some(&sealed), type_id, u64::MAX, |hop| seen.push(hop.id));
        assert!(!seen.contains(&3));
    }

    /// ADR-0551 accounting, pinned on a hand-built sealed chunk (no
    /// compaction): the floor charges the sealed stream at its compressed
    /// byte length plus its tombstone heap, and the delta at live `AdjHop`
    /// bytes — never the sealed hops at decoded hop size.
    #[test]
    fn floor_prices_sealed_at_stream_length_and_delta_at_live_hop_bytes() {
        use std::mem::size_of;

        let sealed_hops: Vec<AdjHop> = (0..COMPACTION_DELTA_FLOOR as i64)
            .map(|i| AdjHop {
                ts: Some(10_000 - i),
                id: i,
            })
            .collect();
        let bytes = crate::adjacency::encode_chunk(&sealed_hops);
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
        let sealed_floor = crate::memory::vec_heap(1, size_of::<(u32, SealedChunk)>() as u64)
            + crate::memory::vec_heap(bytes.len(), 1)
            + crate::memory::vec_heap(tombstones.len(), size_of::<Tombstone>() as u64);

        let delta = vec![AdjHop {
            ts: Some(20_000),
            id: 999,
        }];
        let adj = TypedAdj {
            chunks: vec![(9, delta.clone())],
        };
        let sealed = SealedAdj {
            chunks: vec![(
                9,
                SealedChunk {
                    bytes: bytes.clone(),
                    tombstones,
                },
            )],
        };

        assert_eq!(
            sealed.chunk(9).unwrap().floor_bytes(),
            crate::memory::vec_heap(bytes.len(), 1)
                + crate::memory::vec_heap(1, size_of::<Tombstone>() as u64)
        );
        assert_eq!(
            adj.floor_bytes() + sealed.floor_bytes(),
            crate::memory::adjacency_bytes(&[delta.len()]) + sealed_floor
        );
    }
}
