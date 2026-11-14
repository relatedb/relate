//! `MATCH`-clause shortest paths on breadth-first search (ADR-0186).
//!
//! A routed selector (`shortestPath`, `allShortestPaths`, `ANY SHORTEST`,
//! `SHORTEST 1`, `SHORTEST 1 GROUP`, `ALL SHORTEST`) over two node patterns
//! and one variable-length relationship is answered per (start, end)
//! partition by BFS instead of enumerate-then-select. `match_gpm_path`
//! tries the route first; [`route_shape`] is the single guard, and every
//! pattern it rejects falls through to enumeration unchanged. The searches
//! come from [`crate::shortest`] (ADR-0189). Rows are bound the way the GPM
//! walker binds them; `MATCH ... WHERE` still filters them afterwards.

use crate::UsedSet;
use std::cell::Cell;
#[cfg(test)]
use std::thread::LocalKey;

use relate_core::Result;
use relate_ir::{MatchMode, PathPattern};

use crate::ctx::ExecCtx;
use crate::exec::pushdown::Residual;
use crate::row::Row;

mod filters;
mod guard;
mod reach_cache;
mod route;
mod trail_walk;

pub(crate) use reach_cache::ReachCache;
use route::Route;

use super::sink::MatchedSink;
use super::GpmFilters;
use guard::{route_shape, Spelling};

#[cfg(test)]
/// Run `f` with the thread-local test override `slot` set to `value`,
/// restoring the previous setting afterwards (also on unwind) — the one
/// scoped-override helper the route's size limits share.
pub(super) fn with_override<V: Copy + 'static, T>(
    slot: &'static LocalKey<Cell<Option<V>>>,
    value: V,
    f: impl FnOnce() -> T,
) -> T {
    struct Restore<V: Copy + 'static>(&'static LocalKey<Cell<Option<V>>>, Option<V>);
    impl<V: Copy + 'static> Drop for Restore<V> {
        fn drop(&mut self) {
            self.0.with(|c| c.set(self.1));
        }
    }
    let _restore = Restore(slot, slot.with(|c| c.replace(Some(value))));
    f()
}

#[cfg(test)]
mod anchor_tests;
#[cfg(test)]
mod bounded_tests;
#[cfg(test)]
mod expr_parity_tests;
#[cfg(test)]
mod filters_tests;
#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod parity_tests;
#[cfg(test)]
mod postfilter_tests;
#[cfg(test)]
mod reach_tests;
#[cfg(test)]
mod reject_tests;
#[cfg(test)]
mod route_tests;

#[cfg(test)]
thread_local! {
    static FORCE_GPM: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static ROUTE_RUNS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Count one (pattern, input row) evaluation the route accepted. The
/// counter exists only in tests. Unlike the expansion counter it also moves
/// when a search needs no expansion (the zero-length path of
/// `(a)-[*0..n]-(a)`).
#[inline(always)]
fn note_route_run() {
    #[cfg(test)]
    ROUTE_RUNS.with(|c| c.set(c.get() + 1));
}

/// Take and reset this thread's count of route runs.
#[cfg(test)]
pub(crate) fn take_route_runs() -> u64 {
    ROUTE_RUNS.with(|c| c.replace(0))
}

/// Run `f` with every pattern the route would take sent to `match_gpm_path`
/// instead, for route-versus-GPM parity tests. Restores the previous
/// setting on return or unwind.
#[cfg(test)]
fn with_forced_gpm<T>(f: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            FORCE_GPM.with(|c| c.set(self.0));
        }
    }
    let _restore = Restore(FORCE_GPM.with(|c| c.replace(true)));
    f()
}

/// Match `path` on the BFS route, or decline (`Ok(false)`) when [`route_shape`] rejects
/// it or the route declines it: a predicate the search cannot decide against
/// the rows it binds falls back to enumeration, which applies it to the
/// complete row (`Route::predicates_decidable`). Each row's used set is its
/// trail's relationships, as GPM's would be: the guard admits only an empty
/// incoming set.
///
/// Rows are pushed into the consumer as the route binds them
/// ([`MatchedSink`], ADR-0555): an EXISTS probe riding the match answers
/// from each row and halts the search at its first accept (the verdict is
/// the probe's halt cell, no row is kept); anything else reduces each row
/// through the tail into the caller's vector. Returns whether the route
/// took the pattern (`false`: it declined before any row was pushed).
#[allow(clippy::too_many_arguments)]
pub(super) fn match_shortest_route<'a>(
    sink: MatchedSink<'a, '_>,
    ctx: ExecCtx<'a>,
    path: &'a PathPattern,
    filters: &'a GpmFilters,
    reach: &'a ReachCache,
    row: &'a Row,
    used: &UsedSet,
    match_mode: MatchMode,
) -> Result<bool> {
    run_route(
        sink,
        ctx,
        path,
        filters,
        reach,
        row,
        used,
        match_mode,
        Spelling::of(path),
    )
}

/// The expression form's entry (ADR-0568): run `path` (named, carrying the
/// `ANY SHORTEST` / `ALL SHORTEST` selector the front end lowered) on the
/// route for one input row, folding each bound row into `fold` as the
/// search finds it (ADR-0555) — the caller keeps what it needs, never the
/// rows. Returns whether the route took the pattern (`false`: it declined
/// before any row was folded, and the caller falls back to enumeration).
///
/// The route answers per (start, end) partition, exactly as the `MATCH`
/// clause does; collapsing the partitions to the expression's one value is
/// the fold's job. An unbounded relationship takes the expression form's
/// [`DEFAULT_MAX_HOPS`](crate::pattern_walk::DEFAULT_MAX_HOPS) ceiling.
pub(crate) fn fold_shortest_route(
    ctx: ExecCtx<'_>,
    path: &PathPattern,
    row: &Row,
    fold: &mut dyn FnMut(Row) -> Result<()>,
) -> Result<bool> {
    let filters = GpmFilters::resolve(ctx.graph, path);
    let reach = ReachCache::default();
    let residual = Residual::empty();
    let halt = Cell::new(false);
    // The expression form is the function spelling: its lowered pattern
    // carries no function flag, so the caller stamps it here.
    run_route(
        MatchedSink::fold(&residual, &halt, fold),
        ctx,
        path,
        &filters,
        &reach,
        row,
        &UsedSet::default(),
        MatchMode::DifferentRelationships,
        Spelling::Expression,
    )
}

/// The one driver both entries share: the guard decodes the shape and
/// carries its resolved filter from the pattern-level table (LABS-801) —
/// one pass, no second shape sniffing — then the route takes the pattern or
/// declines it before the sink is touched.
#[allow(clippy::too_many_arguments)]
fn run_route<'a>(
    mut sink: MatchedSink<'a, '_>,
    ctx: ExecCtx<'a>,
    path: &'a PathPattern,
    filters: &'a GpmFilters,
    reach: &'a ReachCache,
    row: &'a Row,
    used: &UsedSet,
    match_mode: MatchMode,
    spelling: Spelling,
) -> Result<bool> {
    #[cfg(test)]
    if FORCE_GPM.with(|c| c.get()) {
        return Ok(false);
    }
    let Some(shape) = route_shape(path, filters, match_mode, used, row, ctx.params, spelling)
    else {
        return Ok(false);
    };
    // The constructor declines (`None`) before the sink is touched, and
    // takes the sink itself — its halt cell and residual are the route's
    // by construction, not two arguments that must agree.
    let Some(route) = Route::try_new(ctx, path, row, shape, &sink, reach) else {
        return Ok(false);
    };
    route.run(&mut sink)?;
    note_route_run();
    Ok(true)
}
