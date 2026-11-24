//! The comparison seek's posting ranges: the primary bound algebra, the
//! int/float cross-type sweep, and the one direction pair they share
//! (REL8-2030 split this cluster out of `order`).

use std::collections::BTreeMap;
use std::ops::Bound;

use relate_core::Value;
use relate_ir::BinaryOp;

use crate::eval::{self, OrderingSide};

use super::{ord_f64, order_key, variant_lower, variant_upper, OrderKey, Postings};

/// Whether an order key's order equals the predicate's ordering. A zoned
/// `Time`/`DateTime` key carries the instant (or UTC nanos) alone, while
/// `Temporal::compare` breaks the tie on the Time offset and the DateTime
/// zone region (ADR-0446), so its tie group can hold values the strict
/// predicate accepts; every other key's order is its predicate's order.
#[derive(Clone, Copy, PartialEq, Eq)]
enum KeyOrder {
    Crisp,
    Coarse,
}

impl KeyOrder {
    /// The classification, decided from the key's own variant — the one
    /// place the coarse kinds are named.
    fn of(key: &OrderKey) -> Self {
        match key {
            OrderKey::Time(_) | OrderKey::DateTime(_) => KeyOrder::Coarse,
            _ => KeyOrder::Crisp,
        }
    }
}

/// The side of `key`'s own variant in the seek's direction: the variant
/// edge follows from the direction alone, and `key_bound` is the caller's
/// one inclusive/exclusive choice.
fn side_range(
    key: &OrderKey,
    key_bound: Bound<OrderKey>,
    side: OrderingSide,
) -> (Bound<OrderKey>, Bound<OrderKey>) {
    if side.is_greater() {
        (key_bound, variant_upper(key))
    } else {
        (Bound::Included(variant_lower(key)), key_bound)
    }
}

/// One comparison seek's whole answer: the primary range over the key's own
/// variant, plus the other numeric variant's edge when the bound is a
/// number. The caller scans the ranges in order; the predicate re-checks
/// the superset.
pub(super) struct SeekRanges {
    primary: (Bound<OrderKey>, Bound<OrderKey>),
    cross: Option<(Bound<OrderKey>, Bound<OrderKey>)>,
}

impl SeekRanges {
    /// The ranges to scan, the primary first.
    pub(super) fn ranges(&self) -> impl Iterator<Item = (Bound<OrderKey>, Bound<OrderKey>)> + '_ {
        std::iter::once(self.primary.clone()).chain(self.cross.clone())
    }
}

/// The seek's ranges for `op` over `bound`, or `None` when the bound has no
/// order key or the op is not a comparison. The key is derived here once and
/// the cross sweep reads it, so callers never pair a key with its bound. The
/// strict ops bound exclusively, unless the key is coarse (its tie group
/// must be scanned) or the op is non-strict; both facts come from the one
/// ordering-operator table.
pub(super) fn seek_ranges(op: BinaryOp, bound: &Value) -> Option<SeekRanges> {
    let key = order_key(bound)?;
    let ordering = eval::ordering_operator(op)?;
    let inclusive = KeyOrder::of(&key) == KeyOrder::Coarse || ordering.equality_allowed;
    let key_bound = if inclusive {
        Bound::Included(key.clone())
    } else {
        Bound::Excluded(key.clone())
    };
    Some(SeekRanges {
        primary: side_range(&key, key_bound, ordering.side),
        cross: cross_range(&key, bound, ordering.side),
    })
}

/// The other numeric variant's side of the bound, or `None` for a
/// non-numeric key. The key's own variant decides the side (an exact
/// integral float shares the Int key, so a `2.0` bound sweeps the float
/// side); `as_number` reads the number the key was built from.
fn cross_range(
    key: &OrderKey,
    bound: &Value,
    side: OrderingSide,
) -> Option<(Bound<OrderKey>, Bound<OrderKey>)> {
    let number = bound.as_number()?;
    let start = match key {
        OrderKey::Int(_) => numeric_cross_start(number, side),
        OrderKey::Float(_) => int_cross_start(number, side),
        _ => return None,
    };
    Some(side_range(&start, Bound::Included(start.clone()), side))
}

/// The float-side edge of a numeric cross sweep: the adjacent float past
/// the bound (infinite edges clamp to the bound itself).
fn numeric_cross_start(bound: f64, side: OrderingSide) -> OrderKey {
    let edge = if side.is_greater() {
        bound.next_down()
    } else {
        bound.next_up()
    };
    OrderKey::Float(ord_f64(if edge.is_finite() { edge } else { bound }))
}

/// The int-side edge of a float bound's cross sweep. The ±1 margin
/// over-scans the exact adjacency by one int (the predicate re-checks it);
/// the saturating cast keeps a ±inf bound total.
fn int_cross_start(f: f64, side: OrderingSide) -> OrderKey {
    OrderKey::Int(if side.is_greater() {
        (f.floor() as i64).saturating_sub(1)
    } else {
        (f.ceil() as i64).saturating_add(1)
    })
}

/// Every posting under `start..end`, through the one range walk.
pub(super) fn visit_range(
    order: &BTreeMap<OrderKey, Postings>,
    start: Bound<OrderKey>,
    end: Bound<OrderKey>,
    mut visit: impl FnMut(&Postings),
) {
    for (_, ids) in order.range((start, end)) {
        visit(ids);
    }
}

pub(super) fn visit_range_postings(
    order: &BTreeMap<OrderKey, Postings>,
    op: BinaryOp,
    bound: &Value,
    mut visit: impl FnMut(&Postings),
) {
    let Some(seek) = seek_ranges(op, bound) else {
        return;
    };
    for (start, end) in seek.ranges() {
        visit_range(order, start, end, &mut visit);
    }
}
