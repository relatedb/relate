//! Logical live-graph byte account (ADR-0042, amended by ADR-0202;
//! string-posting formula ADR-0063).
//!
//! This module is the sizing policy: the reservation model a vector is
//! charged under ([`reserved_len`], [`vec_heap`]), the span placement one
//! id's slot array follows (ADR-0086), and the version-chain shapes. The
//! account halves that price whole records and batches against it — the
//! live-entity account and the redo estimate — live in
//! [`account`](self::account).
//!
//! The payload terms stay. On top of them the floor charges the containers
//! that still own the payload: dashmap slots, version lists, property
//! vectors, and adjacency chunks. A vector of length `n` is charged for
//! [`reserved_len`]`(n)` elements, not for whatever the allocator rounded
//! the allocation up to — except the sealed arena's immutable byte buffer,
//! sized once per compaction and charged for exactly its length
//! ([`vec_heap_exact`], ADR-0599).

mod account;

pub use account::{
    estimate_redo_delta, live_node_account, live_node_account_live, live_rel_account_live,
    IndexCharge, RedoDelta,
};

use std::mem::size_of;

pub const OVERHEAD_VERSION: u64 = 24;
pub const OVERHEAD_POSTING: u64 = 8;
pub const OVERHEAD_MAP_ENTRY: u64 = 24;
/// `Vec`'s three-word header. Inline in the parent value, so it is charged
/// once for the vector itself and not again for elements whose `size_of`
/// already includes an inner `Vec`.
pub const VEC_HEADER: u64 = 24;

/// The default per-allocation expression cap (ADR-0463). Lives in the
/// sizing-policy module both the execution view and the store services can
/// consult downward; a configured open option replaces it and `none`/`0`
/// disables it.
pub const EXPRESSION_BUDGET_BYTES: u64 = 256 << 20;

/// Capacity the floor charges for a vector of length `len`.
///
/// `0` reserves nothing. Every other length rounds up to the next power of
/// two. `usize::next_power_of_two` maps `0` to `1`, so zero is special-cased.
pub fn reserved_len(len: usize) -> usize {
    if len == 0 {
        0
    } else {
        len.next_power_of_two()
    }
}

/// Heap bytes of a `Vec<T>` of length `len` that holds exactly `len`
/// elements: `len * elem`, with no [`reserved_len`] rounding.
///
/// The sealed arena's byte buffer is exempt from the reservation model: it
/// is sized once at its encoded length by the checkpoint compaction, never
/// grown by `push` after that, so its capacity is its length — charging the
/// next power of two would bill slack the arena never holds (ADR-0599). The
/// delta tier and the sealed entry/tombstone vectors still grow by `push`
/// and keep [`vec_heap`]'s reservation.
pub fn vec_heap_exact(len: usize, elem: u64) -> u64 {
    len as u64 * elem
}

/// Saturating [`reserved_len`]: a length whose next power of two cannot be
/// represented saturates to [`u64::MAX`], which every charge check treats as
/// over any cap. The charge path's one spelling of the reservation model
/// under extreme lengths.
pub fn reserved_len_saturating(len: usize) -> u64 {
    if len == 0 {
        0
    } else {
        len.checked_next_power_of_two()
            .map_or(u64::MAX, |n| n as u64)
    }
}

/// Heap bytes of a `Vec<T>` of length `len`: `reserved_len(len) * elem`.
/// The header is not included; callers add [`VEC_HEADER`] when the header
/// is not already inside a parent `size_of`.
pub fn vec_heap(len: usize, elem: u64) -> u64 {
    reserved_len(len) as u64 * elem
}

/// Heap bytes a `Vec<T>` of length `len` gains from `added` more elements:
/// the reservation model's own difference — `reserved_len(len + added) *
/// elem - reserved_len(len) * elem` — the one spelling of the reserved-vec
/// growth the admission estimates and the sealed tier's cost helpers price
/// (a founding case is `vec_growth(0, added, elem)`). Both the length and
/// the byte product saturate, so the canonical growth helper cannot wrap
/// where the charge path's saturating spelling would not.
pub fn vec_growth(len: usize, added: usize, elem: u64) -> u64 {
    reserved_len_saturating(len.saturating_add(added))
        .saturating_mul(elem)
        .saturating_sub(reserved_len_saturating(len).saturating_mul(elem))
}

/// Where one id lands in a span array (ADR-0086) — the one statement of
/// the placement policy, whose [`classify`](SpanPlacement::classify) every
/// consumer consults and then applies its own effect to: the store's
/// `try_grow` reserves on it, the admission estimate's [`place_in_span`]
/// prices it, the redo estimator's `Shapes` books it, and the test
/// fixtures' boundary search advances on it. Never re-split the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanPlacement {
    /// Already inside the span: no dense growth, no side-map entry.
    Inside,
    /// The array grew to cover the id: the dense bytes it gained.
    Grew(u64),
    /// The id cannot be placed in the array: it takes a side-map entry.
    SideEntry,
}

impl SpanPlacement {
    /// Classify `idx` against a span array covering `span` ids at `slot`
    /// bytes per slot. Pure — no reservation, no mutation — so every
    /// consumer reads one policy; the price is the gate's own bytes, so no
    /// caller re-derives the reservation it just took.
    pub fn classify(span: usize, idx: usize, slot: u64) -> SpanPlacement {
        let need = idx.saturating_add(1);
        if need <= span {
            return SpanPlacement::Inside;
        }
        match reservable_span_bytes(need, slot) {
            Some(after) => SpanPlacement::Grew(after.saturating_sub(id_vec_bytes(span, slot))),
            None => SpanPlacement::SideEntry,
        }
    }
}

/// Place `id` in the span array `span` covers, returning what it cost.
///
/// `None` is an id the span array cannot index at all — negative, the range
/// the store refuses. The estimate runs on a batch's ids *before* the
/// commit validates them (the ordered preflight's `validate_ids`), so this
/// must price nothing rather than refuse: the caller's own validation
/// rejects such a batch with the documented storage error before anything
/// applies, and an estimate that panicked here would bypass it.
pub fn place_in_span(span: &mut usize, id: i64, slot: u64) -> Option<SpanPlacement> {
    let idx = usize::try_from(id).ok()?;
    let placement = SpanPlacement::classify(*span, idx, slot);
    if matches!(placement, SpanPlacement::Grew(_)) {
        *span = idx.saturating_add(1);
    }
    Some(placement)
}

/// Grow `v` so `capacity == want`.
fn fit_capacity<T>(v: &mut Vec<T>, want: usize) {
    if v.capacity() == want {
        return;
    }
    let mut fitted = Vec::with_capacity(want);
    fitted.append(v);
    if fitted.capacity() > want {
        fitted.shrink_to(want);
    }
    *v = fitted;
}

/// Grow `v` so `capacity == reserved_len(len)`.
pub(crate) fn fit_reserved<T>(v: &mut Vec<T>) {
    fit_capacity(v, reserved_len(v.len()));
}

/// Grow `v` so `capacity == len` — the exact-size counterpart of
/// [`fit_reserved`], for a vector sized once and never grown by `push`
/// (a sealed chunk's immutable stream). Its charge is
/// [`vec_heap_exact`], not [`vec_heap`].
pub(crate) fn fit_exact<T>(v: &mut Vec<T>) {
    fit_capacity(v, v.len());
}

/// The shape of one id's version slot (ADR-0529).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainShape {
    Absent,
    /// The live row and its birth timestamp, no version list.
    Inline,
    /// A version list of this many versions.
    Spilled(usize),
}

impl ChainShape {
    /// The shape after a copy-on-write that keeps the previous version.
    pub fn after_cow(self) -> Self {
        match self {
            Self::Absent | Self::Inline => Self::Spilled(2),
            Self::Spilled(n) => Self::Spilled(n + 1),
        }
    }

    /// The shape after a create installs a version on this id.
    pub fn after_install(self) -> Self {
        match self {
            Self::Absent => Self::Inline,
            other => other.after_cow(),
        }
    }

    /// The shape after a delete tombstones the live version.
    pub fn after_delete(self) -> Self {
        match self {
            Self::Inline => Self::Spilled(1),
            other => other,
        }
    }
}

/// Largest dense reservation a store will allocate. A bigger id is a
/// side-map entry, so one persisted id cannot reserve gigabytes before
/// admission runs (ADR-0086, ADR-0532).
pub const DENSE_SPAN_CAP_BYTES: u64 = 1 << 30;

/// Reserved bytes of a dense id span, or `None` when the power-of-two
/// reservation does not fit in `u64`. That id cannot have a slot array
/// (ADR-0532) and lives in the store's side map instead.
pub fn dense_span_bytes(span: usize, slot: u64) -> Option<u64> {
    if span == 0 {
        return Some(0);
    }
    let reserved = span.checked_next_power_of_two()?;
    (reserved as u64).checked_mul(slot)
}

/// Dense reservation a store will allocate for `span`. `None` when the
/// power-of-two product overflows `u64` or exceeds
/// [`DENSE_SPAN_CAP_BYTES`]: that id is a side-map entry, the same gate
/// the store uses when it grows.
pub fn reservable_span_bytes(span: usize, slot: u64) -> Option<u64> {
    match dense_span_bytes(span, slot) {
        Some(bytes) if bytes <= DENSE_SPAN_CAP_BYTES => Some(bytes),
        _ => None,
    }
}

/// Reserved bytes of an id-indexed slot array (ADR-0532). The row itself
/// sits in the slot; this is the vector, holes included. A span that does
/// not fit charges nothing here — the caller charges a side-map entry.
pub fn id_vec_bytes(span: usize, slot: u64) -> u64 {
    dense_span_bytes(span, slot).unwrap_or(0)
}

/// One side-map entry for an id whose slot array does not fit: the
/// hash-map overhead plus the value. The value's spilled heap is separate.
pub fn overflow_entry_bytes(value: u64) -> u64 {
    OVERHEAD_MAP_ENTRY + value
}

/// Heap of a `HashSet<i64>` that can hold `capacity` entries: hashbrown's
/// 9-byte buckets (8-byte key plus control byte), one slack bucket past
/// `capacity`, and the 16-byte hasher state. The census harness's one
/// spelling of a `HashSet<i64>` allocation (research-only, ADR-0476).
#[cfg(feature = "research")]
pub(crate) fn hashset_i64_bytes(capacity: usize) -> u64 {
    (capacity as u64 + 1) * 9 + 16
}

/// Heap of a spilled version chain. An inline row lives in the id slot, so
/// it adds nothing here, and the spilled vector's header does too.
pub fn chain_bytes(shape: ChainShape, stride: u64) -> u64 {
    match shape {
        ChainShape::Spilled(n) => vec_heap(n, stride),
        _ => 0,
    }
}

/// One id family's version-chain tally — the census harness's count shape
/// (research-only, ADR-0476). Beside [`chain_bytes`] so every
/// [`ChainShape`] arm is matched in one module.
#[cfg(feature = "research")]
#[derive(Debug, Default, Clone, Copy)]
pub struct ChainCensus {
    pub inline: u64,
    pub spilled_slots: u64,
    pub spilled_versions: u64,
}

#[cfg(feature = "research")]
impl ChainCensus {
    pub(crate) fn count(&mut self, shape: ChainShape) {
        match shape {
            ChainShape::Inline => self.inline += 1,
            ChainShape::Spilled(n) => {
                self.spilled_slots += 1;
                self.spilled_versions += n as u64;
            }
            ChainShape::Absent => {}
        }
    }
}

/// `to_id` slot, `to_str` slot, and one `Arc<str>` allocation. A second
/// `Arc` clone of the same string is not charged again.
pub fn interned_string_bytes(len: usize) -> u64 {
    2 * OVERHEAD_MAP_ENTRY + 16 + len as u64
}

/// One stub-label cache entry: the dashmap slot, the label-id key, the
/// `Vec<String>` header and its reserved `String` slots, and the copied
/// string heaps. Charged once per distinct label-id list. The interned
/// originals stay on [`interned_string_bytes`]; these are the cache's own
/// copies, and the entry is not evicted when the nodes go away.
pub fn label_vec_cache_bytes(label_count: usize, string_bytes: u64) -> u64 {
    OVERHEAD_MAP_ENTRY
        + VEC_HEADER
        + vec_heap(label_count, size_of::<u32>() as u64)
        + VEC_HEADER
        + vec_heap(label_count, size_of::<String>() as u64)
        + string_bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_len_is_zero_or_the_next_power_of_two() {
        assert_eq!(reserved_len(0), 0);
        assert_eq!(reserved_len(1), 1);
        assert_eq!(reserved_len(2), 2);
        assert_eq!(reserved_len(3), 4);
        assert_eq!(reserved_len(20), 32);
    }

    #[test]
    fn reserved_len_saturating_matches_reserved_len_and_saturates() {
        for len in [0, 1, 2, 3, 20, 1_000_000] {
            assert_eq!(reserved_len_saturating(len), reserved_len(len) as u64);
        }
        assert_eq!(reserved_len_saturating(usize::MAX), u64::MAX);
    }
}
