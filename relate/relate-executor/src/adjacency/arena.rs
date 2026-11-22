//! The sealed tier's arena (ADR-0599): one graph-level, node-major `Vec<u8>`
//! of every folded chunk's encoded stream, addressed by per-node spans of
//! 12-byte entries, with the versioned tombstones that filter the streams.
//! Split out of `adjacency/mod.rs` (the fold constants, the stream merge and
//! the reclaim stay there with the delta's vocabulary).
//!
//! The tier sits behind one lock; a read is a shared lock, a write an
//! exclusive one. A fold appends the new encoding and marks the replaced
//! range dead; compaction re-lays the live streams and re-indexes the spans.

use std::collections::HashSet;
use std::mem::size_of;
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use relate_core::{NodeId, RelId};

use crate::memory::{fit_exact, fit_reserved, vec_growth, vec_heap, vec_heap_exact};

use super::codec;
use super::span_table::SpanTable;
use super::Hop;

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

/// One sealed chunk's fixed header in the arena (ADR-0599): its rel type,
/// the offset of its encoded stream in [`SealedArena`]'s bytes, and its hop
/// count. Twelve bytes — the codec's own leading `u32` hop-count header is
/// gone (the count rides here), and the stream the `off` points at leads
/// with the first raw id. Entries sit type-sorted inside their node's span,
/// so a `(node, type)` lookup is a binary search, not a linear scan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub type_id: u32,
    pub off: u32,
    pub hops: u32,
}

impl Entry {
    /// Whether the entry holds a stream — the one spelling of the fact
    /// `mark_dead`, the compaction walk and `stream_of` all key on.
    pub(super) fn has_stream(self) -> bool {
        self.hops > 0
    }
}

/// One direction's sealed tier (ADR-0551 / ADR-0599): a graph-level,
/// node-major arena of every folded chunk's encoded stream, a dense
/// id-indexed span table into a shared 12-byte entry array, and the
/// versioned tombstones that filter the streams. A node that never folds has
/// no entry and pays nothing.
///
/// The whole tier sits behind one lock; a read is a shared lock, a write an
/// exclusive one. Callers reach a node's slice through [`SealedArena::region`],
/// a cheap `Copy` handle that re-locks per access — the lock is never held
/// across a hop callback, so a re-entrant walk ([`crate::graph::Graph`]'s
/// neighbor iteration) cannot deadlock on it.
pub(crate) struct SealedArena {
    store: RwLock<ArenaStore>,
}

impl Default for SealedArena {
    fn default() -> Self {
        Self {
            store: RwLock::new(ArenaStore::default()),
        }
    }
}

impl std::fmt::Debug for SealedArena {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = self.read();
        let nodes = s.spans.live_node_count();
        f.debug_struct("SealedArena")
            .field("nodes", &nodes)
            .field("entries", &s.entries.len())
            .field("arena_bytes", &s.bytes.len())
            .finish()
    }
}

/// The sealed tier's floor split (ADR-0599): the arena byte buffer, the
/// addressing structures around it, and the dead arena bytes awaiting
/// compaction. The heap census's per-side sealed terms; `arena + index` is
/// the tier's whole floor, `dead` a subset of `arena`.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SealedTierBytes {
    pub arena: u64,
    pub index: u64,
    /// The dead arena ranges' share — emitted in a research-feature
    /// build only, the heap census its one reader.
    #[cfg(feature = "research")]
    pub dead: u64,
}

/// The arena's inner state, behind [`SealedArena`]'s lock.
#[derive(Default)]
struct ArenaStore {
    /// Every folded chunk's encoded stream, node-major; a fold appends.
    bytes: Vec<u8>,
    /// The 12-byte entries, grouped into per-node spans (type-sorted inside).
    entries: Vec<Entry>,
    /// Each entry's tombstone list, a parallel column so the header stays
    /// exactly 12 bytes. Same length as `entries`, kept in lockstep.
    tombstones: Vec<Vec<Tombstone>>,
    /// The per-node span table (ADR-0599): each node's live entry range,
    /// the wide-id side map, and the shift scans' high-water start.
    spans: SpanTable,
    /// The dead arena bytes awaiting compaction (ADR-0599): the sum of
    /// every replaced stream's length. The ranges themselves are never
    /// read — the compaction walk re-derives the live layout — so only
    /// the sum is kept.
    dead_bytes: u64,
}

/// The arena's 4 GiB admission wall: whether `arena_bytes` plus a
/// write's `growth` still fits the `u32` offset space — the one
/// predicate the under-lock recheck and the admission pre-check both
/// consult.
pub(crate) fn arena_admits(arena_bytes: u64, growth: u64) -> bool {
    arena_bytes.saturating_add(growth) <= u32::MAX as u64
}

/// An arena byte length at the `u32` offset field's structural boundary —
/// the 4 GiB wall — checked, never truncated. The one spelling every
/// writer of an entry's stream offset routes through.
fn arena_offset(len: usize) -> u32 {
    u32::try_from(len).expect("the arena stays under 4 GiB")
}

/// A chunk's hop count at the `u32` field's structural boundary — checked,
/// never truncated. The one spelling the arena entry and the page header
/// both route through.
pub(crate) fn hop_count(len: usize) -> u32 {
    u32::try_from(len).expect("an adjacency chunk holds at most u32::MAX hops")
}

impl ArenaStore {
    fn range_of(&self, node: NodeId) -> std::ops::Range<usize> {
        self.spans.of(node).range()
    }

    /// The entry index of `type_id` in `node`'s span, or `None`.
    fn entry_index(&self, node: NodeId, type_id: u32) -> Option<usize> {
        self.span_search(node, type_id).ok()
    }

    /// Search `node`'s span for `type_id` — the one type-sorted lookup,
    /// returning the entry's absolute index or the position its type
    /// sorts at. A hole span's range is empty and its `Err` is never
    /// read: the founding arm computes its own tail position, and
    /// `entry_index` keeps only the `Ok`.
    fn span_search(&self, node: NodeId, type_id: u32) -> Result<usize, usize> {
        let r = self.range_of(node);
        match self.entries[r.clone()].binary_search_by_key(&type_id, |e| e.type_id) {
            Ok(i) => Ok(r.start + i),
            Err(i) => Err(r.start + i),
        }
    }

    /// The bytes of entry `i`'s stream.
    fn stream_of(&self, i: usize) -> &[u8] {
        let e = self.entries[i];
        let off = e.off as usize;
        &self.bytes[off..off + codec::stream_len(&self.bytes[off..], e.hops as usize)]
    }

    /// Mark entry `i`'s stream dead: its bytes are reclaimed at the next
    /// compaction. The stream length comes from the one reader,
    /// [`ArenaStore::stream_of`].
    fn mark_dead(&mut self, i: usize) {
        let e = self.entries[i];
        if !e.has_stream() {
            return;
        }
        let len = self.stream_of(i).len();
        debug_assert!(
            arena_admits(e.off as u64, len as u64),
            "a dead stream stays under the arena wall"
        );
        self.dead_bytes += len as u64;
    }

    /// Overwrite entry `i`, marking the entry it replaces dead first —
    /// a founded entry's zero-hop placeholder tallies nothing.
    fn replace_entry(&mut self, i: usize, entry: Entry) {
        self.mark_dead(i);
        self.entries[i] = entry;
    }

    /// Insert a new zero-hop entry into `node`'s span, keeping it
    /// type-sorted and shifting every later node's span. Returns the index.
    fn insert_entry(&mut self, node: NodeId, type_id: u32) -> usize {
        let s = self.spans.of(node);
        if !s.is_live() {
            // A fresh span at the array's tail: no live span starts at or
            // after it, so there is nothing to shift.
            let idx = self.entries.len();
            self.insert_columns(idx, type_id);
            self.spans.set(node, idx, 1);
            return idx;
        }
        let pos = self
            .span_search(node, type_id)
            .expect_err("a founded type sorts to a gap in its live span");
        self.insert_columns(pos, type_id);
        // The span grows by one entry, shifting what its own bound says.
        self.spans.grow(node);
        pos
    }

    /// Insert a zero-hop entry and its tombstone slot at `pos` — the
    /// columns stay in lockstep through this one writer, an append
    /// included (`pos == entries.len()`).
    fn insert_columns(&mut self, pos: usize, type_id: u32) {
        self.entries.insert(
            pos,
            Entry {
                type_id,
                off: 0,
                hops: 0,
            },
        );
        self.tombstones.insert(pos, Vec::new());
        fit_reserved(&mut self.entries);
        fit_reserved(&mut self.tombstones);
    }

    /// Drop the entry and its tombstone slot at `i` — the removal half of
    /// the columns' lockstep.
    fn remove_columns(&mut self, i: usize) {
        self.entries.remove(i);
        self.tombstones.remove(i);
        fit_reserved(&mut self.entries);
        fit_reserved(&mut self.tombstones);
    }

    /// Remove entry `i` from `node`'s span, shifting every later node's
    /// span. The removed entry's stream dies with it — the tally is
    /// taken here, under the same lock, so no removal path can drop
    /// bytes from the exact-reclaim accounting.
    fn remove_entry(&mut self, node: NodeId, i: usize) {
        self.mark_dead(i);
        self.remove_columns(i);
        // The span loses one entry, shifting what its own bound says.
        self.spans.shrink(node);
    }

    /// Store one type's stream and tombstones, or drop the type's entry when
    /// both are empty (a fully reclaimed chunk keeps no slot). A fold appends
    /// the new encoding and marks the replaced range dead. Returns whether
    /// it wrote: the field walls are checked here, under the store's one
    /// write lock — a founding needs a span slot under the `u16` length and
    /// the appended stream must keep the arena under the `u32` offset — so
    /// concurrent shard folds cannot pass a check and then cross the wall
    /// behind each other's backs. `false` leaves the tier exactly as it
    /// was; the caller keeps the chunk in the delta tier (ADR-0599).
    fn put_chunk(
        &mut self,
        node: NodeId,
        type_id: u32,
        hops: &[Hop],
        tombstones: Vec<Tombstone>,
    ) -> bool {
        let existing = self.entry_index(node, type_id);
        if hops.is_empty() && tombstones.is_empty() {
            if let Some(i) = existing {
                // The stream's bytes die with the entry — remove_entry
                // takes the tally, so nothing vanishes from the arena
                // without ever being dead.
                self.remove_entry(node, i);
            }
            return true;
        }
        let stream = codec::encode_chunk(hops);
        // The walls under this lock, in the same spelling the fold's
        // pre-check uses: a founding needs a span slot, and the stream —
        // at the codec's own bound — must keep the arena under the offset.
        if existing.is_none() && !self.spans.has_room_for_type(node) {
            return false;
        }
        if !arena_admits(self.bytes.len() as u64, stream.len() as u64) {
            return false;
        }
        // The stream offset at the u32 field's structural boundary; a
        // tombstone-only chunk holds no stream, so its entry's offset is
        // the zero it is defined to hold rather than a fabricated length.
        let off = if hops.is_empty() {
            0
        } else {
            arena_offset(self.bytes.len())
        };
        self.bytes.extend_from_slice(&stream);
        let entry = Entry {
            type_id,
            off,
            hops: hop_count(hops.len()),
        };
        let i = existing.unwrap_or_else(|| self.insert_entry(node, type_id));
        self.replace_entry(i, entry);
        self.tombstones[i] = tombstones;
        fit_reserved(&mut self.tombstones[i]);
        true
    }

    /// Record `id`'s sealed position as superseded at `ts`, creating a
    /// zero-hop entry on demand so a never-sealed type's tombstone has a
    /// home (ADR-0551).
    fn tombstone_deleted(
        &mut self,
        node: NodeId,
        type_id: u32,
        id: RelId,
        ts: u64,
        supersede: Supersede,
    ) {
        let deleted = match supersede {
            Supersede::Deleted => Some(ts),
            Supersede::Repositioned => None,
        };
        let i = match self.entry_index(node, type_id) {
            Some(i) => i,
            // A founding at the span wall leaves the tombstone out of the
            // tier (ADR-0599): a never-sealed type's hop is retired by the
            // version map, and a tombstone that names no sealed position is
            // dropped by the fold anyway (ADR-0551) — so a store whose span
            // the build clamped keeps answering its deletes, where the
            // founding would die in the write guard.
            None if !self.spans.has_room_for_type(node) => return,
            None => self.insert_entry(node, type_id),
        };
        let list = &mut self.tombstones[i];
        if let Some(t) = list.iter_mut().find(|t| t.id == id) {
            if deleted.is_some() {
                t.deleted = deleted;
            }
        } else {
            list.push(Tombstone {
                id,
                xmax: ts,
                deleted,
            });
            fit_reserved(list);
        }
    }

    /// Reclaim the arena's dead data (ADR-0599): drop the dead bytes by
    /// re-laying the live streams in entry order — each entry's new
    /// offset written back in place — and trim the dense span array's
    /// trailing holes.
    fn compact(&mut self) {
        // Nothing dead means nothing to reclaim and the layout is already
        // the compact one — a store that never folded past a replacement
        // keeps the node-major layout its build left, so the re-layout
        // would copy every live stream for nothing. The fit still runs:
        // the arena is charged at its exact length, and a push-grown
        // buffer left unfitted would be charged less than it holds.
        // Only the byte buffer accrues dead data — a replaced stream's
        // range — and the dense span array its trailing holes: the entry
        // column and the tombstones carry live slots only (a removal
        // deletes, a replacement overwrites), so the compaction re-lays
        // the buffer in place, writing each surviving stream's new offset
        // back into its entry, and trims the span tail.
        let dead_bytes = self.dead_bytes;
        if dead_bytes > 0 {
            let before = self.bytes.len() as u64;
            // The dead bytes are a share of the buffer — the tally stays at
            // or under its length, checked here so a corrupt tally refuses
            // rather than under-sizing the replacement.
            let live = self
                .bytes
                .len()
                .checked_sub(dead_bytes as usize)
                .expect("the dead tally stays at or under the arena's length");
            let mut bytes = Vec::with_capacity(live);
            for i in 0..self.entries.len() {
                let e = self.entries[i];
                if !e.has_stream() {
                    continue;
                }
                let off = arena_offset(bytes.len());
                bytes.extend_from_slice(self.stream_of(i));
                // The write-back lands after the copy, and every later entry
                // still holds the offset it was laid at.
                self.entries[i].off = off;
            }
            debug_assert_eq!(
                before - bytes.len() as u64,
                dead_bytes,
                "compaction reclaims exactly the dead bytes"
            );
            self.bytes = bytes;
            self.dead_bytes = 0;
        }
        self.spans.trim_tail();
        self.fit();
    }

    /// Re-fit every buffer to the size its floor charges: the arena's byte
    /// buffer exact ([`vec_heap_exact`]), the push-grown vectors at their
    /// reservation. After a compaction, and after a wholesale build's
    /// emission, which push-grows the byte buffer (ADR-0599 / ADR-0608).
    fn fit(&mut self) {
        fit_exact(&mut self.bytes);
        fit_reserved(&mut self.entries);
        fit_reserved(&mut self.tombstones);
        self.spans.fit();
    }

    /// The arena byte buffer charged at its exact encoded length (ADR-0599).
    fn arena_bytes(&self) -> u64 {
        vec_heap_exact(self.bytes.len(), 1)
    }

    /// The entry array and the tombstone column it indexes, the tombstone
    /// column's per-entry vecs included.
    fn entry_bytes(&self) -> u64 {
        vec_heap(self.entries.len(), size_of::<Entry>() as u64)
            + vec_heap(self.tombstones.len(), size_of::<Vec<Tombstone>>() as u64)
            + self
                .tombstones
                .iter()
                .map(|t| vec_heap(t.len(), tombstone_stride()))
                .sum::<u64>()
    }

    /// The whole tier's reserved heap minus the span table: the arena
    /// byte buffer, the entry array, and the tombstone column — the test
    /// oracle's split of [`SealedArena::tier_bytes`]; the dead bytes are
    /// the scalar [`ArenaStore::dead_bytes`], not a heap term.
    #[cfg(test)]
    fn floor_bytes(&self) -> u64 {
        self.arena_bytes() + self.entry_bytes()
    }
}

impl SealedArena {
    fn read(&self) -> RwLockReadGuard<'_, ArenaStore> {
        self.store.read().expect("sealed arena lock")
    }

    fn write(&self) -> RwLockWriteGuard<'_, ArenaStore> {
        self.store.write().expect("sealed arena lock")
    }

    /// The node's sealed region — a cheap handle that re-locks per access —
    /// or `None` when the node has no sealed chunk.
    pub(crate) fn region(&self, node: NodeId) -> Option<SealedRegion<'_>> {
        if self.read().spans.of(node).len == 0 {
            None
        } else {
            Some(SealedRegion { arena: self, node })
        }
    }

    /// The node's sealed region, always — the fold's and the delete's path,
    /// which creates the region when it is absent. The handle re-locks per
    /// access, so a no-entry node reads empty until a write lands.
    pub(crate) fn region_or_default(&self, node: NodeId) -> SealedRegion<'_> {
        SealedRegion { arena: self, node }
    }

    /// Record a versioned tombstone for a hop the write superseded (ADR-0551),
    /// creating the node's zero-hop entry on demand.
    pub(crate) fn tombstone_deleted(
        &self,
        node: NodeId,
        type_id: u32,
        id: RelId,
        ts: u64,
        supersede: Supersede,
    ) {
        self.write()
            .tombstone_deleted(node, type_id, id, ts, supersede);
    }

    /// The tier's reserved heap minus the span table — the arena bytes and
    /// the entry column. [`Self::array_bytes`] carries the span table
    /// beside it, so the two compose the whole tier (the heap census's
    /// `arena` and `index` split); the dead bytes are the scalar
    /// [`ArenaStore::dead_bytes`], not a heap term.
    #[cfg(test)]
    pub(crate) fn floor_bytes(&self) -> u64 {
        self.read().floor_bytes()
    }

    /// Reserved bytes of the span table — the dense array plus one side-map
    /// entry per wide id (the span-table share of the heap census's sealed
    /// `index_bytes`).
    #[cfg(test)]
    pub(crate) fn array_bytes(&self) -> u64 {
        self.read().spans.table_bytes()
    }

    /// The arena byte buffer at its exact encoded length — the admission
    /// term of the `u32` entry-offset bound (ADR-0599).
    pub(crate) fn arena_bytes(&self) -> u64 {
        self.read().arena_bytes()
    }

    /// Dense span length, holes included.
    pub(crate) fn span(&self) -> usize {
        self.read().spans.len()
    }

    /// `(entries, tombstones)` — the two lengths the entry-column growth term
    /// rides.
    pub(crate) fn entry_counts(&self) -> (usize, usize) {
        let s = self.read();
        (s.entries.len(), s.tombstones.len())
    }

    /// The tier's floor split (ADR-0599) — the one composition of the
    /// tier's whole floor, which the recount's floor and the heap census
    /// both read: the arena byte buffer charged at its exact encoded
    /// length, the addressing structures around it — the entry array, the
    /// tombstone column, the span table, and the side map — and, in a
    /// research-feature build, the dead arena bytes awaiting compaction
    /// ([`ArenaStore::dead_bytes`]), the share of `arena` the next
    /// checkpoint fold reclaims.
    pub(crate) fn tier_bytes(&self) -> SealedTierBytes {
        let s = self.read();
        SealedTierBytes {
            arena: s.arena_bytes(),
            index: s.entry_bytes() + s.spans.table_bytes(),
            #[cfg(feature = "research")]
            dead: s.dead_bytes,
        }
    }

    pub(crate) fn clear(&self) {
        // The whole state is replaced, not reset field by field: a
        // hand-listed reset already drifted once in this file's history
        // (it dropped the dead ranges).
        *self.write() = ArenaStore::default();
    }

    /// The arena's live byte length and its dead ranges' byte sum — the
    /// before/after pair the fully-reclaimed-chunk test asserts around
    /// compaction (ADR-0599's exact-reclaim invariant).
    #[cfg(test)]
    pub(crate) fn reclaim_pair(&self) -> (u64, u64) {
        let s = self.read();
        (s.bytes.len() as u64, s.dead_bytes)
    }

    /// Compact the arena: drop the dead ranges and re-lay the live streams
    /// node-major. Rides the checkpoint fold (ADR-0599).
    pub(crate) fn compact(&self) {
        self.write().compact();
    }

    /// Fit the arena's buffers to the exact sizes its floor charges — the
    /// wholesale build's emission push-grows them (ADR-0608).
    pub(crate) fn fit(&self) {
        self.write().fit();
    }

    /// Assert every buffer holds exactly the capacity its floor charges — the
    /// byte buffer at its exact length ([`vec_heap_exact`]), the push-grown
    /// vectors at their reservation. Returns the count of entries with a
    /// non-empty tombstone list.
    #[cfg(test)]
    pub(crate) fn assert_fit(&self) -> usize {
        use crate::memory::reserved_len;
        fn assert_capacity<T>(v: &Vec<T>, want: usize, what: &str) {
            assert_eq!(v.capacity(), want, "{what} holds its reservation");
        }
        let s = self.read();
        assert_capacity(&s.bytes, s.bytes.len(), "the arena byte buffer (exact)");
        assert_capacity(&s.entries, reserved_len(s.entries.len()), "the entry array");
        assert_capacity(
            &s.tombstones,
            reserved_len(s.tombstones.len()),
            "the tombstone column",
        );
        for t in &s.tombstones {
            assert_capacity(t, reserved_len(t.len()), "a tombstone vec");
        }
        s.spans.assert_fit();
        s.tombstones.iter().filter(|t| !t.is_empty()).count()
    }
}

/// One node's view into a [`SealedArena`] (ADR-0599). `Copy` — it holds only
/// the arena and the node id, and re-locks on every access, so it is never
/// held across a hop callback.
#[derive(Clone, Copy)]
pub(crate) struct SealedRegion<'a> {
    arena: &'a SealedArena,
    node: NodeId,
}

impl SealedRegion<'_> {
    /// Physical hop count of `type_id`'s chunk — no decode.
    pub(crate) fn chunk_len(&self, type_id: u32) -> usize {
        let s = self.arena.read();
        s.entry_index(self.node, type_id)
            .map_or(0, |i| s.entries[i].hops as usize)
    }

    /// Whether the node holds a sealed chunk for `type_id`.
    pub(crate) fn has_chunk(&self, type_id: u32) -> bool {
        self.arena.read().entry_index(self.node, type_id).is_some()
    }

    /// The span's entry type ids — the span length's own vocabulary, one
    /// read under one lock. A type whose delta chunk a fold's
    /// fully-reclaimed arm removed keeps its sealed entry (the tombstones
    /// it still holds), so the delta's chunk list alone undercounts the
    /// span; the admission wall reads this, never the delta alone.
    pub(crate) fn type_ids(&self) -> Vec<u32> {
        let s = self.arena.read();
        s.entries[s.range_of(self.node)]
            .iter()
            .map(|e| e.type_id)
            .collect()
    }

    /// How many sealed chunks the node holds (one per type).
    #[cfg(any(test, feature = "research", feature = "test-hooks"))]
    pub(crate) fn chunk_count(&self) -> usize {
        self.arena.read().range_of(self.node).len()
    }

    /// Whether a tombstone can be reclaimed at `horizon` (ADR-0551): its
    /// sealed position or its delta hop is at or below it.
    pub(crate) fn reclaimable(&self, type_id: u32, horizon: u64) -> bool {
        let s = self.arena.read();
        s.entry_index(self.node, type_id).is_some_and(|i| {
            s.tombstones[i]
                .iter()
                .any(|t| t.xmax <= horizon || t.deleted.is_some_and(|d| d <= horizon))
        })
    }

    /// How many of `ids` the chunk holds no tombstone for yet — the in-place
    /// update arm of the delete path asked of a whole batch (ADR-0551).
    pub(crate) fn fresh_tombstone_count(&self, type_id: u32, ids: &HashSet<RelId>) -> usize {
        let s = self.arena.read();
        match s.entry_index(self.node, type_id) {
            Some(i) => {
                ids.len()
                    - s.tombstones[i]
                        .iter()
                        .filter(|t| ids.contains(&t.id))
                        .count()
            }
            None => ids.len(),
        }
    }

    /// Bytes the tombstone vec gains from `fresh` new slots.
    pub(crate) fn tombstone_growth_bytes(&self, type_id: u32, fresh: usize) -> u64 {
        let s = self.arena.read();
        let len = s
            .entry_index(self.node, type_id)
            .map_or(0, |i| s.tombstones[i].len());
        vec_growth(len, fresh, tombstone_stride())
    }

    /// The hop stream `type_id` holds, decoded, tombstones unfiltered.
    pub(crate) fn decoded(&self, type_id: u32) -> Vec<Hop> {
        let s = self.arena.read();
        match s.entry_index(self.node, type_id) {
            Some(i) => {
                let e = s.entries[i];
                codec::decode_chunk(s.stream_of(i), e.hops as usize)
            }
            None => Vec::new(),
        }
    }

    /// The hop stream `type_id` holds, decoded and with the hops a tombstone
    /// hides at `ts` dropped.
    pub(crate) fn visible_hops(&self, type_id: u32, ts: u64) -> Vec<Hop> {
        let s = self.arena.read();
        let Some(i) = s.entry_index(self.node, type_id) else {
            return Vec::new();
        };
        let e = s.entries[i];
        let mut hops = codec::decode_chunk(s.stream_of(i), e.hops as usize);
        let tombstones = &s.tombstones[i];
        if !tombstones.is_empty() {
            hops.retain(|h| {
                tombstones
                    .iter()
                    .find(|t| t.id == h.id)
                    .is_none_or(|t| t.xmax > ts)
            });
        }
        hops
    }

    /// A copy of `type_id`'s tombstones (tests and the fold's clone).
    pub(crate) fn tombstones(&self, type_id: u32) -> Vec<Tombstone> {
        let s = self.arena.read();
        s.entry_index(self.node, type_id)
            .map_or_else(Vec::new, |i| s.tombstones[i].clone())
    }

    /// Record a versioned tombstone for a hop the write superseded (ADR-0551).
    pub(crate) fn tombstone_deleted(
        &self,
        type_id: u32,
        rid: RelId,
        ts: u64,
        supersede: Supersede,
    ) {
        self.arena
            .write()
            .tombstone_deleted(self.node, type_id, rid, ts, supersede);
    }

    /// Store one type's sealed hop stream and tombstones, or drop the type's
    /// entry when both are empty.
    pub(crate) fn put_chunk(&self, type_id: u32, hops: &[Hop], tombstones: Vec<Tombstone>) -> bool {
        self.arena
            .write()
            .put_chunk(self.node, type_id, hops, tombstones)
    }
}
