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
use std::collections::HashSet;

use relate_core::Result;
use relate_ir::{Expr, MatchMode, PathPattern};

use crate::ctx::ExecCtx;
use crate::eval::expr_vars;
use crate::exec::pushdown::Residual;
use crate::row::Row;

mod guard;
mod route;

use super::sink::MatchedSink;
use super::GpmFilters;
use guard::{route_shape, Spelling};
use route::{PathFilter, Route};

/// The predicates a routed shortest path must satisfy **during** the
/// search: the pattern's own inline `WHERE`, plus the MATCH-level `WHERE`
/// conjuncts that read the *path itself* — the named path variable or the
/// relationship variable. A route that only filtered the selected shortest
/// path afterwards answered zero rows whenever that shortest path violated
/// such a predicate, instead of the shortest path that satisfies it.
///
/// Endpoint-only conjuncts are deliberately **not** a filter: the route
/// answers one shortest path per `(start, end)` partition, so a predicate
/// on the endpoints selects whole partitions and post-filtering it is
/// exact — keeping those queries on the BFS search instead of the
/// enumerated predicate walk.
fn path_filter<'p>(path: &'p PathPattern, residual: &'p Residual<'_>) -> Option<PathFilter<'p>> {
    // Text render: the carrier set compares against the String variable
    // sets of the filter walk.
    let mut carriers: HashSet<String> = path
        .rels
        .iter()
        .filter_map(|rel| rel.variable.as_ref().map(|v| v.to_string()))
        .collect();
    for name in path.path_names() {
        carriers.insert(name.to_string());
    }
    let conjuncts: Vec<&Expr> = residual
        .conjuncts
        .iter()
        .filter(|conjunct| {
            let mut vars = HashSet::new();
            expr_vars(conjunct, &mut vars);
            vars.iter().any(|name| carriers.contains(name.as_str()))
        })
        .collect();
    (path.where_clause.is_some() || !conjuncts.is_empty()).then_some(PathFilter {
        conjuncts,
        inline: path.where_clause.as_deref(),
        scope: residual.scope,
    })
}

#[cfg(test)]
mod anchor_tests;
#[cfg(test)]
mod bounded_tests;
#[cfg(test)]
mod expr_parity_tests;
#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod parity_tests;
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
pub(super) fn match_shortest_route(
    mut sink: MatchedSink<'_, '_>,
    ctx: ExecCtx<'_>,
    path: &PathPattern,
    filters: &GpmFilters,
    row: &Row,
    used: &UsedSet,
    match_mode: MatchMode,
) -> Result<bool> {
    #[cfg(test)]
    if FORCE_GPM.with(|c| c.get()) {
        return Ok(false);
    }
    // The guard decodes the shape and carries its resolved filter from the
    // pattern-level table (LABS-801) — one pass, no second shape sniffing.
    let Some(shape) = route_shape(path, filters, match_mode, used, row, ctx.params) else {
        return Ok(false);
    };
    let residual = sink.residual();
    let halt = sink.halt_cell();
    let route = Route::new(ctx, path, row, shape, path_filter(path, residual), halt);
    if !route.run(&mut sink)? {
        return Ok(false);
    }
    note_route_run();
    Ok(true)
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
    #[cfg(test)]
    if FORCE_GPM.with(|c| c.get()) {
        return Ok(false);
    }
    let filters = GpmFilters::resolve(ctx.graph, path);
    let used = UsedSet::default();
    let Some(mut shape) = route_shape(
        path,
        &filters,
        MatchMode::DifferentRelationships,
        &used,
        row,
        ctx.params,
    ) else {
        return Ok(false);
    };
    shape.max = shape.max.or(Some(crate::pattern_walk::DEFAULT_MAX_HOPS));
    // The expression form is the function spelling: its lowered pattern
    // carries no function flag, so the caller stamps it here.
    shape.spelling = Spelling::Expression;
    let residual = Residual::empty();
    let halt = Cell::new(false);
    let mut sink = MatchedSink::fold(&residual, &halt, fold);
    let filter = path_filter(path, &residual);
    if !Route::new(ctx, path, row, shape, filter, &halt).run(&mut sink)? {
        return Ok(false);
    }
    note_route_run();
    Ok(true)
}
