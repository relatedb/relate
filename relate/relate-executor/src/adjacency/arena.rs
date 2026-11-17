//! The sealed tier's arena (ADR-0599): one graph-level, node-major `Vec<u8>`
//! of every folded chunk's encoded stream, addressed by per-node spans of
//! 12-byte entries, with the versioned tombstones that filter the streams.
//! Split out of `adjacency/mod.rs` (the fold constants, the stream merge and
//! the reclaim stay there with the delta's vocabulary).
//!
//! The tier sits behind one lock; a read is a shared lock, a write an
//! exclusive one. A fold appends the new encoding and marks the replaced
//! range dead; compaction re-lays the live streams and re-indexes the spans.

use std::collections::{HashMap, HashSet};
use std::mem::size_of;
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use relate_core::{NodeId, RelId};

use crate::memory::{fit_exact, fit_reserved, vec_growth, vec_heap, vec_heap_exact};

use super::codec;
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

/// One node's entry range in the arena's entry array (ADR-0599): a dense,
/// id-indexed span table gives each node the `[start, start + len)` slice of
/// [`SealedArena`]'s entries its chunks occupy. Six bytes; `len == 0` is a
/// node with no sealed chunk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C, packed)]
pub(crate) struct Span {
    pub start: u32,
    pub len: u16,
}

/// The span length's structural bound: a node holds at most this many
/// distinct rel types in one direction. Admission enforces it before the
/// redo is logged (ADR-0599); the arena's own arithmetic states it as the
/// invariant that enforcement guarantees.
pub(crate) const MAX_SPAN_TYPES: usize = u16::MAX as usize;

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
        let nodes = s.spans.iter().filter(|sp| sp.len > 0).count();
        f.debug_struct("SealedArena")
            .field("nodes", &nodes)
            .field("entries", &s.entries.len())
            .field("arena_bytes", &s.bytes.len())
            .finish()
    }
}

/// The sealed tier's floor split (ADR-0599): the arena byte buffer, the
/// addressing structures around it, and the dead arena ranges awaiting
/// compaction. The heap census's per-side sealed terms; `arena + index` is
/// the tier's whole floor, `dead` a subset of `arena`.
#[cfg(feature = "research")]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SealedTierBytes {
    pub arena: u64,
    pub index: u64,
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
    /// Dense, id-indexed span table: `spans[node]` is the node's live entry
    /// range. A node with `len == 0` has no sealed chunk.
    spans: Vec<Span>,
    /// The side map for an id whose dense span array does not fit
    /// (ADR-0086/0532), so one wide id cannot reserve gigabytes.
    overflow: HashMap<NodeId, Span>,
    /// The arena ranges a fold replaced, awaiting compaction (ADR-0599).
    dead: Vec<(u32, u32)>,
    /// High-water mark of every live span's start, never decreased (a
    /// stale-high mark only costs one shift scan). A mid-array entry
    /// insert/remove whose node-span ends past it cannot shift any other
    /// span, so the scan is skipped — the wholesale build's every insert.
    max_start: usize,
}

/// Whether `idx` can have a dense span slot — inside the reserved cap.
fn dense_slot(idx: usize) -> bool {
    crate::memory::reservable_span_bytes(idx + 1, size_of::<Span>() as u64).is_some()
}

/// A span start from an entry-array index, at the structural boundary the
/// span arithmetic relies on: the shift pass moves starts through `i32`,
/// so the entry array stays under 2^31 entries — 24 GiB of 12-byte
/// entries, unreachable before any real store's memory. Checked, never
/// truncated: a silent wrap here would strand a node's whole span.
fn span_start(idx: usize) -> u32 {
    i32::try_from(idx).expect("the entry array stays under 2^31 entries") as u32
}

/// A span length from an entry count, at the chunk-type cap the u16 field
/// states: a node's span counts its distinct rel types, at most 65535.
fn span_len(len: usize) -> u16 {
    u16::try_from(len).expect("a node holds at most 65535 rel types")
}

/// A chunk's hop count at the `u32` field's structural boundary — checked,
/// never truncated. The one spelling the arena entry and the page header
/// both route through.
pub(crate) fn hop_count(len: usize) -> u32 {
    u32::try_from(len).expect("an adjacency chunk holds at most u32::MAX hops")
}

/// A span start shifted by `delta`, at the same entry-array ceiling
/// [`span_start`] states: the shift pass moves starts through `i32`, so the
/// value stays inside `i32` — checked, never wrapped.
fn shifted_start(start: u32, delta: i32) -> u32 {
    let shifted = i32::try_from(start).expect("the entry array stays under 2^31 entries") + delta;
    u32::try_from(shifted).expect("a shifted span start stays non-negative")
}

impl ArenaStore {
    fn span_of(&self, node: NodeId) -> Span {
        if let Ok(i) = usize::try_from(node) {
            if i < self.spans.len() {
                let s = self.spans[i];
                if s.len > 0 {
                    return s;
                }
            }
        }
        self.overflow.get(&node).copied().unwrap_or_default()
    }

    fn range_of(&self, node: NodeId) -> std::ops::Range<usize> {
        let s = self.span_of(node);
        let start = s.start as usize;
        start..start + s.len as usize
    }

    /// The entry index of `type_id` in `node`'s span, or `None`.
    fn entry_index(&self, node: NodeId, type_id: u32) -> Option<usize> {
        let r = self.range_of(node);
        self.entries[r.clone()]
            .binary_search_by_key(&type_id, |e| e.type_id)
            .ok()
            .map(|i| r.start + i)
    }

    /// The bytes of entry `i`'s stream.
    fn stream_of(&self, i: usize) -> &[u8] {
        let e = self.entries[i];
        let off = e.off as usize;
        &self.bytes[off..off + codec::stream_len(&self.bytes[off..], e.hops as usize)]
    }

    /// Mark entry `e`'s stream dead: its arena range is reclaimed at the
    /// next compaction.
    fn mark_dead(&mut self, e: Entry) {
        if e.hops > 0 {
            let off = e.off as usize;
            let len = codec::stream_len(&self.bytes[off..], e.hops as usize);
            // The end stays inside the arena's 4 GiB because both ends
            // derive from its length — checked, never wrapped.
            let end = u32::try_from(off + len).expect("the arena stays under 4 GiB");
            self.dead.push((e.off, end));
            fit_reserved(&mut self.dead);
        }
    }

    /// Set `node`'s span to `(start, len)`, in the dense array when it fits
    /// and the side map otherwise. `len == 0` clears it.
    fn set_span(&mut self, node: NodeId, start: u32, len: u16) {
        let Ok(idx) = usize::try_from(node) else {
            self.overflow.remove(&node);
            return;
        };
        if len == 0 {
            if idx < self.spans.len() {
                self.spans[idx] = Span::default();
            }
            self.overflow.remove(&node);
            return;
        }
        if dense_slot(idx) {
            if idx >= self.spans.len() {
                self.spans.resize(idx + 1, Span::default());
                fit_reserved(&mut self.spans);
            }
            self.spans[idx] = Span { start, len };
            self.overflow.remove(&node);
        } else {
            self.overflow.insert(node, Span { start, len });
        }
        // The high-water mark of live span starts, for the shift scans an
        // insert or remove can skip (see [`ArenaStore::shift_spans`]'s
        // callers): never decreased, so a stale high only costs one scan.
        self.max_start = self.max_start.max(start as usize);
    }

    /// Shift every span whose start is at or after `pos` by `delta` (+1 on an
    /// insert, -1 on a remove), skipping `node`'s own span. The dense and
    /// overflow loops share one checked shift, and the high-water start
    /// follows a shifted span up — a stale-low mark would skip a needed
    /// scan.
    fn shift_spans(&mut self, skip: NodeId, pos: usize, delta: i32) {
        for i in 0..self.spans.len() {
            if i as NodeId == skip {
                continue;
            }
            let mut s = self.spans[i];
            if s.len > 0 && s.start as usize >= pos {
                s.start = shifted_start(s.start, delta);
                self.max_start = self.max_start.max(s.start as usize);
                self.spans[i] = s;
            }
        }
        for (node, s) in self.overflow.iter_mut() {
            if *node != skip && s.len > 0 && s.start as usize >= pos {
                s.start = shifted_start(s.start, delta);
                self.max_start = self.max_start.max(s.start as usize);
            }
        }
    }

    /// Whether any live span other than `skip`'s starts at or after `pos` —
    /// false means the shift scan would move nothing and is skipped. Spans
    /// are disjoint, so a mid-span insert/remove at `pos` can only shift
    /// spans starting at or past the node's own span end; the high-water
    /// start below that end says none exist.
    fn shift_needed(&self, skip_end: usize) -> bool {
        self.max_start >= skip_end
    }

    /// Insert a new zero-hop entry into `node`'s span, keeping it
    /// type-sorted and shifting every later node's span. Returns the index.
    fn insert_entry(&mut self, node: NodeId, type_id: u32) -> usize {
        let r = self.range_of(node);
        if r.is_empty() {
            let idx = self.entries.len();
            self.entries.push(Entry {
                type_id,
                off: 0,
                hops: 0,
            });
            self.tombstones.push(Vec::new());
            fit_reserved(&mut self.entries);
            fit_reserved(&mut self.tombstones);
            // A fresh span at the array's tail: no live span starts at or
            // after it, so there is nothing to shift.
            self.set_span(node, span_start(idx), 1);
            return idx;
        }
        let pos = r.start
            + self.entries[r.clone()]
                .binary_search_by_key(&type_id, |e| e.type_id)
                .unwrap_err();
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
        // Only spans at or past this node's span end can shift; past the
        // high-water start there are none, and the wholesale build — whose
        // every insert extends the newest span — skips every scan.
        if self.shift_needed(r.end) {
            self.shift_spans(node, pos, 1);
        }
        let s = self.span_of(node);
        let len = s
            .len
            .checked_add(1)
            .expect("a node holds at most 65535 rel types");
        self.set_span(node, s.start, len);
        pos
    }

    /// Remove entry `i` from `node`'s span, shifting every later node's span.
    fn remove_entry(&mut self, node: NodeId, i: usize) {
        self.entries.remove(i);
        self.tombstones.remove(i);
        fit_reserved(&mut self.entries);
        fit_reserved(&mut self.tombstones);
        // The insert-side guard's mirror: only spans past this node's span
        // end can shift, and past the high-water start there are none.
        let end = {
            let s = self.span_of(node);
            s.start as usize + s.len as usize
        };
        if self.shift_needed(end) {
            self.shift_spans(node, i + 1, -1);
        }
        let s = self.span_of(node);
        let len = s
            .len
            .checked_sub(1)
            .expect("a removed entry's span held at least one entry");
        self.set_span(node, s.start, len);
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
        if hops.is_empty() && tombstones.is_empty() {
            if let Some(i) = self.entry_index(node, type_id) {
                // The stream's bytes die with the entry: once it is gone
                // nothing names the range, so it is marked dead here or
                // compaction's exact-reclaim invariant trips (the bytes
                // would vanish from the arena without ever being dead).
                self.mark_dead(self.entries[i]);
                self.remove_entry(node, i);
            }
            return true;
        }
        let stream = codec::encode_chunk(hops);
        // The walls under this lock, in the same spelling the fold's
        // pre-check uses: a founding needs a span slot, and the stream —
        // at the codec's own bound — must keep the arena under the offset.
        let founding = self.entry_index(node, type_id).is_none();
        if founding && self.span_of(node).len as usize >= MAX_SPAN_TYPES {
            return false;
        }
        if self.bytes.len() as u64 + stream.len() as u64 > u32::MAX as u64 {
            return false;
        }
        // The arena offset and the hop count, at the u32 fields' structural
        // boundaries — checked, never truncated.
        let off = u32::try_from(self.bytes.len()).expect("the arena stays under 4 GiB");
        self.bytes.extend_from_slice(&stream);
        let entry = Entry {
            type_id,
            off,
            hops: hop_count(hops.len()),
        };
        let i = match self.entry_index(node, type_id) {
            Some(i) => {
                self.mark_dead(self.entries[i]);
                i
            }
            None => self.insert_entry(node, type_id),
        };
        self.entries[i] = entry;
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
            None if self.span_of(node).len as usize >= MAX_SPAN_TYPES => return,
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

    /// Rebuild the arena compactly, dropping dead ranges and re-laying the
    /// bytes node-major, then re-index the spans (ADR-0599).
    fn compact(&mut self) {
        // Nothing dead means nothing to reclaim and the layout is already
        // the compact one — a store that never folded past a replacement
        // keeps the node-major layout its build left, so the re-layout
        // would copy every live stream for nothing. The fit still runs:
        // the arena is charged at its exact length, and a push-grown
        // buffer left unfitted would be charged less than it holds.
        if self.dead.is_empty() {
            self.fit();
            return;
        }
        let dead_bytes = self.dead_bytes();
        let before = self.bytes.len() as u64;
        let mut bytes = Vec::with_capacity(self.bytes.len());
        let mut entries = Vec::with_capacity(self.entries.len());
        let mut tombstones: Vec<Vec<Tombstone>> = Vec::with_capacity(self.tombstones.len());
        let mut nodes: Vec<NodeId> = (0..self.spans.len())
            .filter(|i| self.spans[*i].len > 0)
            .map(|i| i as NodeId)
            .collect();
        nodes.extend(
            self.overflow
                .iter()
                .filter(|(_, sp)| sp.len > 0)
                .map(|(n, _)| *n),
        );
        for node in nodes {
            let s = self.span_of(node);
            // `nodes` is every node with a live span, so this always holds;
            // the assert guards the span arithmetic below in debug builds.
            debug_assert!(s.len > 0, "a compaction node holds a live span");
            let start = entries.len();
            for i in s.start as usize..s.start as usize + s.len as usize {
                let e = self.entries[i];
                let off = u32::try_from(bytes.len()).expect("the arena stays under 4 GiB");
                if e.hops > 0 {
                    let off_old = e.off as usize;
                    let len = codec::stream_len(&self.bytes[off_old..], e.hops as usize);
                    bytes.extend_from_slice(&self.bytes[off_old..off_old + len]);
                }
                entries.push(Entry {
                    type_id: e.type_id,
                    off,
                    hops: e.hops,
                });
                tombstones.push(std::mem::take(&mut self.tombstones[i]));
            }
            self.set_span(node, span_start(start), span_len(entries.len() - start));
        }
        debug_assert_eq!(
            before - bytes.len() as u64,
            dead_bytes,
            "compaction reclaims exactly the dead ranges"
        );
        self.bytes = bytes;
        self.entries = entries;
        self.tombstones = tombstones;
        self.dead.clear();
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
        fit_reserved(&mut self.spans);
        fit_reserved(&mut self.dead);
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

    /// The dense span table and the wide-id side map.
    fn span_bytes(&self) -> u64 {
        vec_heap(self.spans.len(), size_of::<Span>() as u64)
            + self.overflow.len() as u64
                * crate::memory::overflow_entry_bytes(size_of::<Span>() as u64)
    }

    /// The dead-range bookkeeping vec.
    fn dead_heap(&self) -> u64 {
        vec_heap(self.dead.len(), size_of::<(u32, u32)>() as u64)
    }

    /// Bytes of the dead arena ranges awaiting compaction.
    fn dead_bytes(&self) -> u64 {
        self.dead.iter().map(|(a, b)| (b - a) as u64).sum()
    }

    /// The whole tier's reserved heap minus the span table: the arena byte
    /// buffer, the entry array, the tombstone column, and the dead-range
    /// bookkeeping.
    fn floor_bytes(&self) -> u64 {
        self.arena_bytes() + self.entry_bytes() + self.dead_heap()
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
        if self.read().span_of(node).len == 0 {
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

    /// The tier's reserved heap minus the span table — the arena bytes, the
    /// entry column and the dead-range bookkeeping. [`Self::array_bytes`]
    /// carries the span table beside it, so the two compose the whole tier
    /// (the heap census's `arena` and `index` split).
    pub(crate) fn floor_bytes(&self) -> u64 {
        self.read().floor_bytes()
    }

    /// Reserved bytes of the span table — the dense array plus one side-map
    /// entry per wide id (the span-table share of the heap census's sealed
    /// `index_bytes`).
    pub(crate) fn array_bytes(&self) -> u64 {
        self.read().span_bytes()
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

    /// The tier's floor split (ADR-0599), the heap census's per-side view:
    /// the arena byte buffer charged at its exact encoded length, the
    /// addressing structures around it — the entry array, the tombstone
    /// column, the span table, the side map, and the dead-range bookkeeping —
    /// and the dead arena ranges awaiting compaction, the share of `arena`
    /// the next checkpoint fold reclaims.
    #[cfg(feature = "research")]
    pub(crate) fn tier_bytes(&self) -> SealedTierBytes {
        let s = self.read();
        SealedTierBytes {
            arena: s.arena_bytes(),
            index: s.entry_bytes() + s.dead_heap() + s.span_bytes(),
            dead: s.dead_bytes(),
        }
    }

    pub(crate) fn clear(&self) {
        let mut s = self.write();
        s.bytes = Vec::new();
        s.entries = Vec::new();
        s.tombstones = Vec::new();
        s.spans = Vec::new();
        s.overflow = HashMap::new();
        s.dead = Vec::new();
        // The span table is empty: a stale high-water start would make the
        // next build's inserts scan the (still empty) table for nothing.
        s.max_start = 0;
    }

    /// The arena's live byte length and its dead ranges' byte sum — the
    /// before/after pair the fully-reclaimed-chunk test asserts around
    /// compaction (ADR-0599's exact-reclaim invariant).
    #[cfg(test)]
    pub(crate) fn reclaim_pair(&self) -> (u64, u64) {
        let s = self.read();
        (s.bytes.len() as u64, s.dead_bytes())
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
        let s = self.read();
        assert_eq!(
            s.bytes.capacity(),
            s.bytes.len(),
            "the arena byte buffer is sized exactly"
        );
        assert_eq!(
            s.entries.capacity(),
            reserved_len(s.entries.len()),
            "the entry array holds its reservation"
        );
        assert_eq!(
            s.tombstones.capacity(),
            reserved_len(s.tombstones.len()),
            "the tombstone column holds its reservation"
        );
        for t in &s.tombstones {
            assert_eq!(
                t.capacity(),
                reserved_len(t.len()),
                "a tombstone vec holds its reservation"
            );
        }
        assert_eq!(
            s.spans.capacity(),
            reserved_len(s.spans.len()),
            "the span table holds its reservation"
        );
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
