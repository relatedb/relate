//! The shared list folds — the three-valued quantifier fold, the membership
//! predicate, and the extreme fold — the policies the list-quantifier
//! expressions (`quantifier.rs`), the `coll.*` functions (`collection.rs`),
//! the aggregates (`aggregate/`), and the `IN` operator (`expr.rs`) all
//! reduce onto. Sibling feature modules import these downward from the eval
//! layer, never sideways from each other.

use std::cmp::Ordering;

use relate_core::{Result, Value};
use relate_ir::QuantifierKind;

/// The one three-valued quantifier fold — the `(kind, truthiness)` state
/// machine the list quantifiers and the `coll.has*Of` functions (CIP-286)
/// share: an early exit per kind (All on the first false, Any on the first
/// true, None on the first true, Single on the second true), a `None`
/// truthiness records the unknown, and the final resolution is
/// three-valued. `pred` receives each item — borrowed or owned, whichever
/// the caller's iteration yields — and yields its truthiness.
pub(crate) fn fold_quantifier<V>(
    kind: QuantifierKind,
    items: impl IntoIterator<Item = V>,
    mut pred: impl FnMut(V) -> Result<Option<bool>>,
) -> Result<Value> {
    let mut saw_null = false;
    let mut true_count = 0usize;
    for item in items.into_iter() {
        let truth = pred(item)?;
        match (kind, truth) {
            (QuantifierKind::All, Some(false)) => return Ok(Value::Bool(false)),
            (QuantifierKind::Any, Some(true)) => return Ok(Value::Bool(true)),
            (QuantifierKind::None, Some(true)) => return Ok(Value::Bool(false)),
            (QuantifierKind::Single, Some(true)) => {
                true_count += 1;
                if true_count >= 2 {
                    return Ok(Value::Bool(false));
                }
            }
            (_, None) => saw_null = true,
            _ => {}
        }
    }
    // A scan that ends undecided is null if any truthiness was unknown,
    // otherwise the kind's default; Single's `>= 2` early exit guarantees
    // true_count <= 1 here.
    Ok(if saw_null {
        Value::Null
    } else {
        Value::Bool(match kind {
            QuantifierKind::All | QuantifierKind::None => true,
            QuantifierKind::Any => false,
            QuantifierKind::Single => true_count == 1,
        })
    })
}

/// Membership of `x` in `xs` under Cypher's equals semantics, three-valued:
/// `true` on the first equal element, `null` when no element is equal but one
/// is null (an unknown value), `false` otherwise — and `false` for a null `x`
/// in an empty list, `null` in a non-empty one: a null `x` decides through
/// the same equals relation (every comparison is unknown), never a special
/// case. The one membership policy of the `IN` operator and the
/// `coll.has*Of` functions (CIP-286), which are quantifiers over exactly
/// this predicate.
pub(crate) fn membership(x: &Value, xs: &[Value]) -> Value {
    // The Any column of the one shared fold: the first true decides, an
    // unknown comparison stays unknown, no member is false. The predicate
    // is infallible, so the fold's error channel cannot fire.
    fold_quantifier(QuantifierKind::Any, xs.iter(), |item| {
        Ok(x.cypher_eq(item).truthy())
    })
    .expect("the membership predicate is infallible")
}

/// The direction of an extreme fold, spelled as a typed discriminator so a
/// call site cannot pass a blind `true`/`false`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Extreme {
    Min,
    Max,
}

impl Extreme {
    /// Whether a value comparing `ordering` against the current best
    /// displaces it — the strict comparison that keeps the FIRST of equal
    /// extremes.
    fn picks(self, ordering: Ordering) -> bool {
        match self {
            Extreme::Min => ordering.is_lt(),
            Extreme::Max => ordering.is_gt(),
        }
    }

    /// The `coll.*` spelling the list-argument error names.
    pub(super) fn op(self) -> &'static str {
        match self {
            Extreme::Min => "coll.min()",
            Extreme::Max => "coll.max()",
        }
    }

    /// Whether the `coll.*` spelling of this extreme poisons on a null
    /// element (live Neo4j 2026.10.1, probed 2026-11-07: `coll.max`
    /// returns null when ANY element is null, `coll.min` skips nulls) —
    /// a coll-surface fact only; the `min()`/`max()` aggregates keep the
    /// fold's null-skipping for both spellings.
    pub(super) fn coll_null_poison(self) -> bool {
        match self {
            Extreme::Min => false,
            Extreme::Max => true,
        }
    }
}

/// The one extreme fold shared by the `min()`/`max()` aggregates (through
/// [`crate::eval::fold_aggregate`]) and `coll.min()`/`coll.max()`:
/// it keeps the FIRST of equal extremes. Neo4j updates only on a strict
/// comparison — `MaxAggregator.shouldUpdate` is `compare(max, value) < 0`
/// and `MinAggregator.shouldUpdate` is `compare(min, value) > 0` (the legacy
/// `MaxFunction`/`MinFunction` keep the same sign) — so a later equal value
/// never displaces the first and `max([1, 1.0])` stays the Int. `cmp` is the
/// caller's ordering policy — the aggregates and the `coll.*` functions
/// both pass the one openCypher order rank, `Value::order_by_cmp`. Nulls are
/// skipped,
/// and the empty extreme is null. One caller spells a further domain rule
/// on top: the `coll.*` layer poisons max on any null element
/// ([`Extreme::coll_null_poison`]) — a live-Neo4j divergence from the
/// aggregates this fold must not absorb.
pub(crate) fn fold_extreme(
    vals: impl IntoIterator<Item = Value>,
    extreme: Extreme,
    cmp: impl Fn(&Value, &Value) -> Ordering,
) -> Value {
    let mut best: Option<Value> = None;
    for value in vals.into_iter().filter(|value| !value.is_null()) {
        if best
            .as_ref()
            .is_none_or(|current| extreme.picks(cmp(&value, current)))
        {
            best = Some(value);
        }
    }
    best.unwrap_or(Value::Null)
}
