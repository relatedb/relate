//! The aggregate-fold leaves, at the value layer: the shared folds
//! ([`SumAcc`], [`fold_aggregate`]) and the horizontal family
//! ([`rewrite_horizontal_aggregates`] — the subset as one type, the
//! row-bound-list unpivot, and the fold). Both modules read only the
//! value layer below them; the grouped path (`exec::aggregate`) and the
//! row-predicate family read them through the `crate::eval` re-export
//! face, so every caller keeps the `crate::eval::…` path.

mod accumulator;
mod horizontal;

pub(crate) use accumulator::{
    fold_aggregate, needs_numbers, FoldInput, Percentile, PercentileSample, StreamFold, SumAcc,
};
pub(crate) use horizontal::rewrite_horizontal_aggregates;
