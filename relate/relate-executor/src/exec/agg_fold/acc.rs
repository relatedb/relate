//! The fold's streaming accumulators (ADR-0036): `StreamAgg` (shared by
//! the one-hop fold and the multi-hop path fold) and `DistinctAcc` (the
//! `count(DISTINCT …)` seat over the one distinct set) — extracted so mod.rs keeps
//! the fold entry points, seek policy, and output glue.
//!
//! Visibility policy: every item and method in this file is
//! `pub(in crate::exec)` — the consumers are the fold modules
//! (`agg_fold`, `agg_fold::path`) and the sibling `pushdown_tests.rs`,
//! all inside `crate::exec`.

use std::collections::BTreeMap;

use relate_core::{Result, Value};
use relate_ir::{classify_aggregate, AggregateKind, Expr};

use crate::distinct_set::DistinctSet;
use crate::eval::{eval, SumAcc};
use crate::overlay::ExecGraph;
use crate::row::Row;

/// One streaming aggregate, shared by both folds: the one-hop fold
/// classifies a projection expression once in [`StreamAgg::new`] and
/// offers rows through [`StreamAgg::offer`]; the path fold extracts the
/// value itself and offers it through [`StreamAgg::offer_value`]. All
/// drain through the single [`StreamAgg::finish`].
#[derive(Clone)]
pub(in crate::exec) enum StreamAgg {
    CountStar(i64),
    Count(i64),
    CountDistinct(DistinctAcc),
    Sum(SumAcc),
}

impl StreamAgg {
    /// Classify the expression once, consulting the engine's canonical
    /// aggregate contract (ADR-0217, [`classify_aggregate`]) and matching
    /// the typed kind instead of re-sniffing name strings: a call that
    /// misses the contract (including the zero-argument `count()` — the
    /// star form is the separate [`Expr::CountStar`] variant) declines
    /// the fold, so the general path states the contract's rejection.
    /// Returns the accumulator paired with its validated operand (`None`
    /// for `count(*)`).
    pub(in crate::exec) fn new(expr: &Expr) -> Option<(Self, Option<&Expr>)> {
        match expr {
            Expr::CountStar => Some((Self::count_star(), None)),
            Expr::Function {
                name,
                distinct,
                args,
            } => {
                let kind = name.with_str(classify_aggregate)?;
                if args.len() != kind.arity().required() {
                    return None;
                }
                let agg = match kind {
                    AggregateKind::Count => Self::count_of(*distinct),
                    AggregateKind::Sum if !*distinct => Self::Sum(SumAcc::new()),
                    _ => return None,
                };
                Some((agg, args.first()))
            }
            _ => None,
        }
    }

    /// The row-count prototype (`count(*)`): variant construction stays
    /// here so callers never reach into the accumulator's representation.
    pub(in crate::exec) fn count_star() -> Self {
        Self::CountStar(0)
    }

    /// The non-null-count prototype (`count(operand)`): variant construction
    /// stays here so callers never reach into the accumulator's
    /// representation.
    pub(in crate::exec) fn count() -> Self {
        Self::Count(0)
    }

    /// The distinct-count prototype (the path fold's `count(DISTINCT …)`
    /// seat): variant construction stays here so callers never reach into
    /// the accumulator's representation.
    pub(in crate::exec) fn count_distinct() -> Self {
        Self::CountDistinct(DistinctAcc::default())
    }

    /// The count prototype for a distinct flag — the one site stating the
    /// distinct-to-prototype policy (`distinct` ⇒ the distinct set,
    /// otherwise the non-null count), shared by [`StreamAgg::new`]'s
    /// classification and the path fold's seat.
    pub(in crate::exec) fn count_of(distinct: bool) -> Self {
        if distinct {
            Self::count_distinct()
        } else {
            Self::count()
        }
    }

    /// The one-hop fold's row channel: evaluate the operand captured at
    /// classification and feed the shared value channel. The variant
    /// already carries the classification — no per-row re-derivation.
    pub(in crate::exec) fn offer(
        &mut self,
        graph: &ExecGraph<'_>,
        row: &Row,
        operand: Option<&Expr>,
        params: &BTreeMap<String, Value>,
        retained: &mut crate::overlay::RetainedTotal,
    ) -> Result<()> {
        let value = operand.map(|e| eval(graph, row, e, params)).transpose()?;
        self.offer_value(value.as_ref(), graph, retained)
    }

    /// The shared value channel (the path fold offers through it
    /// directly): `None` offers no operand value — a `count(*)` row.
    /// Counts skip nulls; distinct and sum insert the value. Fallible
    /// because the `Sum` arm routes through [`SumAcc::offer`].
    pub(in crate::exec) fn offer_value(
        &mut self,
        value: Option<&Value>,
        graph: &ExecGraph<'_>,
        retained: &mut crate::overlay::RetainedTotal,
    ) -> Result<()> {
        match self {
            Self::CountStar(n) => *n += 1,
            Self::Count(n) => {
                if value.is_none_or(|v| !v.is_null()) {
                    *n += 1;
                }
            }
            Self::CountDistinct(acc) => {
                if let Some(v) = value {
                    // ADR-0463 (LABS-781): the set's structure rides its
                    // workspace reservation (the token ledger); the
                    // expression budget sees the payload the set retains
                    // — the offered value's own heap, read before the
                    // set's key clone shares its `Arc`, charged only
                    // when the offer grows the set.
                    let payload = crate::workspace::owned_heap(v);
                    if acc.insert(v)? {
                        retained.add_bytes(graph, payload)?;
                    }
                }
            }
            Self::Sum(acc) => {
                if let Some(v) = value {
                    acc.offer(v, "sum")?;
                }
            }
        }
        Ok(())
    }

    pub(in crate::exec) fn finish(&self) -> Value {
        match self {
            Self::CountStar(n) | Self::Count(n) => Value::Int(*n),
            Self::CountDistinct(acc) => Value::Int(acc.len()),
            Self::Sum(acc) => acc.clone().finish_sum(),
        }
    }
}

/// `count(DISTINCT …)`'s set: nulls are dropped here (aggregate null
/// policy), every other value goes to the one [`DistinctSet`], which owns
/// its workspace reservation. A duplicate offer does not grow the
/// reservation. Cloning an empty prototype does not take a second
/// reservation; cloning a filled set does.
#[derive(Default)]
pub(in crate::exec) struct DistinctAcc {
    set: DistinctSet,
}

impl Clone for DistinctAcc {
    /// A prototype clone must not fail the fold: the copy retains the
    /// set's bytes, and a refused retain falls back to the uncharged
    /// contents copy — the fold's pre-existing contract, decided here at
    /// the consumer rather than defaulted silently inside the set.
    fn clone(&self) -> Self {
        Self {
            set: self
                .set
                .try_clone()
                .unwrap_or_else(|_| self.set.clone_contents()),
        }
    }
}

impl DistinctAcc {
    /// Returns whether the offer grew the set (a duplicate does not —
    /// and does not grow the reservation).
    pub(in crate::exec) fn insert(&mut self, v: &Value) -> Result<bool> {
        if v.is_null() {
            return Ok(false);
        }
        self.set.insert_value(v)
    }

    pub(in crate::exec) fn len(&self) -> i64 {
        self.set.len() as i64
    }
}
