//! The sealed tier's per-node span table (ADR-0599 / ADR-0617): the dense,
//! id-indexed table that gives each node its entry range in the arena's
//! shared entry array, the wide-id side map, and the high-water start the
//! shift scans consult. One table, one placement policy — the dense array
//! grows on the ladder through [`crate::memory::SpanPlacement`] and
//! [`crate::memory::grow_dense_reserved`], charges through
//! [`crate::memory::span_table_bytes`], and prices its own slot stride.

use std::collections::HashMap;
use std::mem::size_of;

use relate_core::NodeId;

use crate::memory::{dense_or_side, fit_dense_reserved, span_table_bytes};

/// One node's entry range in the arena's entry array (ADR-0599): a dense,
/// id-indexed span table gives each node the `[start, start + len)` slice of
/// [`super::SealedArena`]'s entries its chunks occupy. Six bytes; `len == 0` is a
/// node with no sealed chunk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C, packed)]
pub(crate) struct Span {
    pub start: u32,
    pub len: u16,
}

/// The span table's slot stride — the one spelling of the layout the
/// placement gate, the charge and the admission estimate all price.
pub(crate) fn span_stride() -> u64 {
    size_of::<Span>() as u64
}

impl Span {
    /// Whether the node holds a sealed chunk — `len == 0` is the hole a
    /// cleared span leaves. The one predicate every liveness test reads.
    pub(super) fn is_live(self) -> bool {
        self.len > 0
    }

    /// The node's entry range in the arena's entry array.
    pub(super) fn range(self) -> std::ops::Range<usize> {
        let start = self.start as usize;
        start..start + self.len as usize
    }
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

/// The span length's structural bound: a node holds at most this many
/// distinct rel types in one direction. Admission enforces it before the
/// redo is logged (ADR-0599); the table's own arithmetic states it as the
/// invariant that enforcement guarantees.
pub(crate) const MAX_SPAN_TYPES: usize = u16::MAX as usize;

/// The span table itself: the dense array, the wide-id side map, and the
/// high-water start. Extracted from the arena's state so the table's
/// placement, growth and charge live beside its own layout.
#[derive(Default)]
pub(super) struct SpanTable {
    /// `spans[node]` is the node's live entry range; `len == 0` is no
    /// sealed chunk.
    spans: Vec<Span>,
    /// The side map for an id whose dense span array does not fit
    /// (ADR-0086/0532), so one wide id cannot reserve gigabytes.
    overflow: HashMap<NodeId, Span>,
    /// High-water mark of every live span's start, never decreased (a
    /// stale-high mark only costs one shift scan). A mid-array entry
    /// insert/remove whose node-span ends past it cannot shift any other
    /// span, so the scan is skipped — the wholesale build's every insert.
    max_start: usize,
}

/// A span start shifted by `delta`, at the entry-array ceiling the span
/// arithmetic relies on: the shift pass moves starts through `i32`, so the
/// value stays inside `i32` — checked, never wrapped.
fn shifted_start(start: u32, delta: i32) -> u32 {
    let shifted = i32::try_from(start).expect("the entry array stays under 2^31 entries") + delta;
    u32::try_from(shifted).expect("a shifted span start stays non-negative")
}

impl SpanTable {
    /// The node's span — `Span::default()` when it holds none.
    pub(super) fn of(&self, node: NodeId) -> Span {
        if let Ok(i) = usize::try_from(node) {
            if i < self.spans.len() {
                let s = self.spans[i];
                if s.is_live() {
                    return s;
                }
            }
        }
        self.overflow.get(&node).copied().unwrap_or_default()
    }

    /// Set `node`'s span to `(start, len)`, in the dense array when it fits
    /// and the side map otherwise. `len == 0` clears it. The gate's three
    /// classes collapse to one dense-or-side decision — a growth that
    /// cannot allocate degrades to the side map, like the id stores.
    pub(super) fn set(&mut self, node: NodeId, start: usize, len: usize) {
        let idx = usize::try_from(node).expect("a node id indexes the span table");
        if len == 0 {
            if idx < self.spans.len() {
                self.spans[idx] = Span::default();
            }
            self.overflow.remove(&node);
            return;
        }
        let (start, len) = (span_start(start), span_len(len));
        let dense = dense_or_side(&mut self.spans, idx, span_stride(), Span::default);
        if dense {
            self.spans[idx] = Span { start, len };
            self.overflow.remove(&node);
        } else {
            self.overflow.insert(node, Span { start, len });
        }
        // The high-water mark of live span starts, for the shift scans an
        // insert or remove can skip (see [`SpanTable::shift`]'s callers):
        // never decreased, so a stale high only costs one scan.
        self.max_start = self.max_start.max(start as usize);
    }

    /// Grow the dense array once to cover `node` — a wholesale install's
    /// widest id — so the spans it then sets never grow it again. A wide id
    /// the array cannot hold is a side-map entry and reserves nothing.
    pub(super) fn reserve_for(&mut self, node: NodeId) {
        let idx = usize::try_from(node).expect("a node id indexes the span table");
        dense_or_side(&mut self.spans, idx, span_stride(), Span::default);
    }

    /// The dense array's address, for the test that pins a fit copying
    /// nothing.
    #[cfg(test)]
    pub(super) fn as_ptr(&self) -> *const Span {
        self.spans.as_ptr()
    }

    /// Shift every span starting at or after `end` — the inserting or
    /// removing node's own span end — by `delta` (+1 on an insert, -1 on a
    /// remove). Spans are disjoint, so the immediate successor, the span
    /// starting exactly at `end`, is the common first shift and no span
    /// starts inside the node's own range. The high-water start below
    /// `end` says none exist and the whole scan is skipped — the wholesale
    /// build, whose every insert extends the newest span, skips all of
    /// them.
    pub(super) fn shift(&mut self, end: usize, delta: i32) {
        if self.max_start < end {
            return;
        }
        for s in self.spans.iter_mut().chain(self.overflow.values_mut()) {
            if s.is_live() && s.start as usize >= end {
                s.start = shifted_start(s.start, delta);
                self.max_start = self.max_start.max(s.start as usize);
            }
        }
    }

    /// The node's live span — the precondition both length mutators
    /// state once, at the boundary. The founding case is the caller's
    /// `set`, per the table's contract.
    fn live_span(&self, node: NodeId) -> Span {
        let s = self.of(node);
        assert!(s.is_live(), "a length-mutated span holds a live entry");
        s
    }

    /// Whether the node's span can take one more rel type — the u16
    /// length's cap, the wall admission enforces before the redo is
    /// logged (ADR-0599).
    pub(super) fn has_room_for_type(&self, node: NodeId) -> bool {
        (self.of(node).len as usize) < MAX_SPAN_TYPES
    }

    /// The node's span grows by one entry, shifting every span at or past
    /// its end up — the end is the table's own bound, derived here. The
    /// u16 cap is enforced once, by `set`'s narrowing (`span_len`) — the
    /// wall admission also consults through [`SpanTable::has_room_for_type`]
    /// before the redo is logged.
    pub(super) fn grow(&mut self, node: NodeId) {
        let s = self.live_span(node);
        self.shift(s.range().end, 1);
        self.set(node, s.start as usize, usize::from(s.len) + 1);
    }

    /// The node's span loses one entry — it held at least one — shifting
    /// every span at or past its end down.
    pub(super) fn shrink(&mut self, node: NodeId) {
        let s = self.live_span(node);
        self.shift(s.range().end, -1);
        self.set(node, s.start as usize, usize::from(s.len) - 1);
    }

    /// Nodes holding a live span — the debug summary's count. The side
    /// map cannot hold a zero span (`set` removes its entry when a span
    /// clears), so its whole length counts.
    pub(super) fn live_node_count(&self) -> usize {
        self.spans.iter().filter(|sp| sp.is_live()).count() + self.overflow.len()
    }

    /// Trim the dense array's trailing zero spans — the holes a cleared
    /// span leaves at the array's top, the compaction's span-side
    /// reclaim. Interior holes stay until their ids' slots are reused.
    pub(super) fn trim_tail(&mut self) {
        let live = self.spans.iter().rposition(|sp| sp.is_live());
        self.spans.truncate(live.map_or(0, |i| i + 1));
    }

    /// Dense span length, holes included — the table's span accessor.
    pub(super) fn len(&self) -> usize {
        self.spans.len()
    }

    /// Re-fit the dense array to the ladder reservation the floor charges.
    pub(super) fn fit(&mut self) {
        fit_dense_reserved(&mut self.spans);
    }

    /// The dense array and the wide-id side map, at their reservations.
    pub(super) fn table_bytes(&self) -> u64 {
        span_table_bytes(
            self.spans.len(),
            span_stride(),
            self.overflow.len(),
            span_stride(),
        )
    }

    /// The dense array holds the ladder reservation the fit leaves.
    #[cfg(test)]
    pub(super) fn assert_fit(&self) {
        use crate::memory::dense_reserved;
        assert_eq!(
            self.spans.capacity(),
            dense_reserved(self.spans.len()).expect("a live span table's rung fits"),
            "the span table holds its ladder reservation"
        );
    }
}
