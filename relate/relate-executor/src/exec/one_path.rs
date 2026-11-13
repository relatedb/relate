//! The one engine fork a per-path match routes through —
//! `match_one_path`: GPM shapes (element predicates, selectors,
//! quantified elements) and every REPEATABLE ELEMENTS match walk the
//! GPM engine, everything else the classic walk. Hoisted out of
//! `match_clause.rs` into its own module beside `expand.rs` so every
//! consumer imports it downward — the driver's pattern loop
//! (`match_clause::match_paths`), the LABS-348 hub join's plain-side
//! enumeration (`hub_join::plain_side_rows`), and the exists probe's
//! dispatch (`exists_probe::seek`) — instead of the join and the probe
//! reaching back up into the driver's module, a module two-cycle.

use crate::UsedSet;

use relate_core::Result;
use relate_ir::{MatchMode, PathPattern};

use crate::ctx::ExecCtx;
use crate::overlay::ExecGraph;
use crate::pattern_walk::RelFilter;
use crate::row::Row;

use super::expand::match_resolved_path;
use super::gpm::{match_gpm_path, GpmFilters, ReachCache};
use super::pattern_policies::{MatchTail, Matched};
use super::pushdown::Residual;
use super::rider::{PathSink, Rider};

/// A pattern's tier state, built once per pattern by the caller's widest
/// loop (the clause's pattern loop in `match_paths`, the join's one side-B
/// build, the probe's dispatch). The resolved rel-type filters (LABS-801):
/// the classic engine reads one per `path.rels` entry, the GPM engine one
/// per expansion element (a quantified path's compact `rels` projection is
/// empty — see [`GpmFilters`]). The variant is chosen by the one
/// engine-fork predicate, so the table and the walk below can never
/// disagree about which engine the pattern takes. The GPM variant also
/// carries the shortest-path route's [`ReachCache`]: a graph read, so its
/// life is the one `match_clause` call that built it (one input batch) and
/// never spans a write; only the GPM arm reads it.
pub(crate) enum PathFilters {
    /// One filter per `path.rels` entry, in order.
    Classic(Vec<RelFilter>),
    /// The GPM engine's element-order table and the route's reach cache.
    Gpm {
        table: GpmFilters,
        reach: ReachCache,
    },
}

impl PathFilters {
    pub(crate) fn resolve(
        graph: &ExecGraph<'_>,
        path: &PathPattern,
        match_mode: MatchMode,
    ) -> Self {
        if match_mode == MatchMode::RepeatableElements || path.needs_gpm() {
            PathFilters::Gpm {
                table: GpmFilters::resolve(graph, path),
                reach: ReachCache::default(),
            }
        } else {
            PathFilters::Classic(
                path.rels
                    .iter()
                    .map(|rp| RelFilter::resolve(graph, rp))
                    .collect(),
            )
        }
    }
}

/// One clause's patterns, each carried with its [`PathFilters`]: the
/// pairing is made once, here, so a caller cannot hand `match_paths` a
/// table list that is not the pattern list's.
pub(crate) struct ResolvedPatterns<'a> {
    paths: &'a [PathPattern],
    tiers: Vec<PathFilters>,
}

impl<'a> ResolvedPatterns<'a> {
    pub(crate) fn resolve(
        graph: &ExecGraph<'_>,
        paths: &'a [PathPattern],
        match_mode: MatchMode,
    ) -> Self {
        let tiers = paths
            .iter()
            .map(|path| PathFilters::resolve(graph, path, match_mode))
            .collect();
        ResolvedPatterns { paths, tiers }
    }

    /// The patterns, in clause order.
    pub(crate) fn paths(&self) -> &'a [PathPattern] {
        self.paths
    }

    /// Each pattern with its tier state, in clause order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&'a PathPattern, &PathFilters)> {
        self.paths.iter().zip(&self.tiers)
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn match_one_path(
    ctx: ExecCtx<'_>,
    path: &PathPattern,
    filters: &PathFilters,
    row: Row,
    used: UsedSet,
    match_mode: MatchMode,
    residual: &Residual<'_>,
    rider: Option<&mut Rider<'_, '_>>,
    tail: MatchTail<'_>,
) -> Result<Matched> {
    // The one engine fork: GPM shapes (and REPEATABLE ELEMENTS) walk the
    // GPM engine, everything else the classic walk — the single spelling
    // of the choice (exists_probe::seek routes through here too, with a
    // probe as its rider, instead of re-spelling the fork). The rider and
    // the tail both arrive assembled — built where their values are
    // decided (match_paths's last-pattern arm, the plain-side
    // enumeration, seek's probe dispatch) — and pass through unchanged:
    // every consumer past this fork, GPM and classic alike, reads the one
    // `MatchTail` state model, never a loose reduce/keep_used pair. The
    // filters arrive resolved beside them (LABS-801): the fork reads the
    // variant, the same fact [`PathFilters::resolve`] built it from.
    match filters {
        PathFilters::Gpm { table, reach } => {
            // A cap rider is the LIMIT pull-in (`gpm::emit`): every complete
            // state is offered as it emerges and the walk halts once the cap
            // is full, instead of materializing the whole enumeration for the
            // caller's projection to truncate. A probe rider is the early
            // exit: the first complete row the chain accepts halts the walk.
            match_gpm_path(
                ctx, path, table, reach, row, used, match_mode, tail, residual, rider,
            )
        }
        PathFilters::Classic(table) => {
            // The classic walk's sink follows the rider — the one rider→sink
            // decision (`PathSink::for_rider`, exec/rider.rs): a probe rides the
            // classic walk exactly as it rides the GPM one (the probe IS the
            // sink — the first accept halts the walk, the verdict is its halt
            // cell, and no rows are collected), while a cap (or no rider at all)
            // collects rows through the tail. A probe ride leaves `out` empty by
            // construction; the caller reads the verdict from the probe's cell.
            let mut out = Vec::new();
            let mut sink = PathSink::for_rider(rider, &mut out, tail);
            // LABS-161: an elementId()/id() equality (or a row-bound variable) on
            // the first rel seeks the rel directly instead of scanning every
            // node's adjacency; a Gone seek matches nothing (zero rows).
            match_resolved_path(ctx, path, table, residual, row, used, &mut sink)?;
            Ok(out)
        }
    }
}
