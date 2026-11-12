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
use super::gpm::{match_gpm_path, GpmFilters};
use super::pattern_policies::{MatchTail, Matched};
use super::pushdown::Residual;
use super::rider::{PathSink, Rider};

/// A pattern's resolved rel-type filters (LABS-801), built once per
/// pattern by the caller's widest loop (the clause's pattern loop in
/// `match_paths`, the join's one side-B build, the probe's dispatch):
/// the classic engine reads one per `path.rels` entry, the GPM engine
/// one per expansion element (a quantified path's compact `rels`
/// projection is empty — see [`GpmFilters`]). The arm is chosen by the
/// one engine-fork predicate, so the table and the walk below can never
/// disagree about which engine the pattern takes.
pub(crate) enum PathFilters {
    /// One filter per `path.rels` entry, in order.
    Classic(Vec<RelFilter>),
    /// The GPM engine's element-order table.
    Gpm(GpmFilters),
}

impl PathFilters {
    pub(crate) fn resolve(
        graph: &ExecGraph<'_>,
        path: &PathPattern,
        match_mode: MatchMode,
    ) -> Self {
        if match_mode == MatchMode::RepeatableElements || path.needs_gpm() {
            PathFilters::Gpm(GpmFilters::resolve(graph, path))
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

impl PathFilters {
    /// One table per pattern, in order — what [`match_paths`] indexes.
    ///
    /// [`match_paths`]: super::match_clause::match_paths
    pub(crate) fn resolve_all(
        graph: &ExecGraph<'_>,
        patterns: &[PathPattern],
        match_mode: MatchMode,
    ) -> Vec<Self> {
        patterns
            .iter()
            .map(|path| Self::resolve(graph, path, match_mode))
            .collect()
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
    // table's arm, the same fact [`PathFilters::resolve`] built it from.
    match filters {
        PathFilters::Gpm(filters) => {
            // A cap rider is the LIMIT pull-in (`gpm::emit`): every complete
            // state is offered as it emerges and the walk halts once the cap
            // is full, instead of materializing the whole enumeration for the
            // caller's projection to truncate. A probe rider is the early
            // exit: the first complete row the chain accepts halts the walk.
            match_gpm_path(
                ctx, path, filters, row, used, match_mode, tail, residual, rider,
            )
        }
        PathFilters::Classic(filters) => {
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
            match_resolved_path(ctx, path, filters, residual, row, used, &mut sink)?;
            Ok(out)
        }
    }
}
