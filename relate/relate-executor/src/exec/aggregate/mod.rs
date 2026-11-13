//! Aggregation's grouped path (the file-size gate refused the single
//! 986-line file): the group path ([`aggregate_ex`] and its key
//! desugaring), the group table (`table`: the key index, `key`'s compact
//! key tuple, `seed`'s narrowed first-row columns, and the one bill for a
//! group's bytes), the streaming per-group accumulators (`stream` finds the
//! sites, `columns` holds their flat state, `pool` the percentile buffers)
//! and the recursive aggregate-expression evaluator ([`agg_eval`]) each
//! live in a sibling module; the shared folds and
//! the horizontal family live one layer down, in `crate::eval::aggregate`.
//! The re-exports below are this module's one public face, so every
//! caller keeps the `exec::aggregate::…` path.

mod columns;
mod eval;
mod group;
mod key;
mod pool;
mod seed;
mod stream;
mod table;

pub(crate) use group::{aggregate_ex, Aggregation, GroupSubclauses};
// The evaluator's scope types and the `*`-key desugaring are reached from
// the crate's own tests only; `super::eval`/`super::group` serve the
// runtime paths directly.
#[cfg(test)]
pub(crate) use eval::{agg_eval, AggScope, FoldedGroup};
#[cfg(test)]
pub(crate) use group::desugar_star_keys;
#[cfg(test)]
pub(crate) use seed::SeedPlan;
#[cfg(test)]
pub(crate) use stream::AggSites;
