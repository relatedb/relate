//! The sorted side-file's one home: the per-key `(value, row)` index the
//! gather builds, and the run-search algorithms the seek and member arms
//! read it with (ADR-0665).

use rustc_hash::FxHashMap;

use super::{Column, ColumnState, NodeId, Val};

/// Move the gathered values into per-key `(value, row)` entries in value
/// order. The column arrays are already built, so the values have no other
/// reader.
pub(super) fn side_index(
    columns: &mut FxHashMap<u32, Column>,
) -> FxHashMap<u32, Vec<(Val, NodeId)>> {
    let mut side: FxHashMap<u32, Vec<(Val, NodeId)>> = FxHashMap::default();
    for (kid, column) in columns.iter_mut() {
        let mut entries: Vec<(Val, NodeId)> =
            match std::mem::replace(&mut column.state, ColumnState::Empty) {
                ColumnState::Int(values) => values
                    .into_iter()
                    .enumerate()
                    .filter_map(|(row, value)| value.map(|value| (Val::Int(value), row as NodeId)))
                    .collect(),
                ColumnState::Str(values) => values
                    .into_iter()
                    .enumerate()
                    .filter_map(|(row, value)| value.map(|value| (Val::Str(value), row as NodeId)))
                    .collect(),
                ColumnState::Empty | ColumnState::Poisoned => Vec::new(),
            };
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        side.insert(*kid, entries);
    }
    side
}

/// The sorted-run resolution the side-file and the seek share — the one
/// algorithm (the lab seam's `equal_range`, ported with the store):
/// the run of `val` in ascending `entries`, starting at `from`.
pub(super) fn equal_range<K, Q, V>(entries: &[(K, V)], from: usize, val: &Q) -> (usize, usize)
where
    K: std::borrow::Borrow<Q>,
    Q: Ord + ?Sized,
{
    let start = from + entries[from..].partition_point(|(k, _)| k.borrow() < val);
    let len = entries[start..].partition_point(|(k, _)| k.borrow() <= val);
    (start, len)
}

/// Galloping sorted batch over the side-file: every row of every key in
/// `sorted_vals`, one pass — O(vals * log(entries/vals)). Both sortedness
/// preconditions (entries ascending by construction, `sorted_vals` the
/// caller's contract) are checked in debug builds only; the release hot
/// loop stays clean.
pub(super) fn member_rows<K, Q, R>(entries: &[(K, R)], sorted_vals: &[Q]) -> Vec<R>
where
    K: std::borrow::Borrow<Q>,
    Q: Ord,
    R: Copy,
{
    debug_assert!(
        sorted_vals.is_sorted(),
        "member_rows: sorted_vals must be ascending"
    );
    let mut out = Vec::new();
    let mut pos = 0usize;
    for val in sorted_vals {
        let (start, len) = equal_range(entries, pos, val);
        out.extend(entries[start..start + len].iter().map(|(_, row)| *row));
        pos = start + len;
    }
    out
}
