//! The GPM mounting of the rider a GPM walk can carry instead of
//! materializing every complete state — the rider vocabulary itself
//! (`Rider`, `PathProbe`) lives at its engine-neutral home,
//! `exec/rider.rs`, since the classic walk sinks the same vocabulary
//! (`PathSink`). A MATCH cap (the LIMIT pull-in — the walk stops the
//! moment the cap is full) or an EXISTS probe (the first accepted row
//! answers it — the remaining frontier is never evaluated). The rider
//! runs each complete state through the per-candidate pipeline the
//! materializing path runs after the walk (path mode, path variable,
//! path predicate spelled here; the reduce checks and projection via the
//! shared `reduce_row` — the materializing path's own tail), so the rows
//! it emits are the rows the materialized walk would emit, in the same
//! order — only sooner, which is what lets the walk halt.

use std::cell::Cell;

use relate_core::Result;
use relate_ir::PathPattern;

use super::super::pattern_policies::{MatchTail, Matched};
use super::super::pushdown::{MatchCap, Residual};
use super::super::rider::{PathProbe, Rider};
use super::{complete_state, GpmCtx};

/// The mounted rider — one enum, one arm per rider kind, each arm owning
/// every fact it consumes. A MATCH cap carries the walked pattern; an
/// EXISTS probe answers through its own pattern (fixed at its
/// construction) and the MATCH-level residual `mount_rider` threads.
/// The walk offers each complete state to it and consults
/// [`GpmCtx::halted`] to stop enumerating — the one reader spelling of
/// the halt cell (`mount_rider` hands the walk the same cell the rider
/// sets: the probe's own verdict cell, or the local one a full cap
/// sets).
pub(super) enum GpmEmit<'a, 'data> {
    /// A MATCH cap on the clause's last pattern: the shared `reduce_row`
    /// tail (the reduce checks and the projection, the same spelling
    /// the collecting sink runs), the cap's count on the pre-projection row
    /// the tail accepted, and a halt once the cap is full —
    /// `CollectSink::offer`'s pipeline for the GPM walk.
    Cap {
        path: &'a PathPattern,
        cap: &'a mut MatchCap<'data>,
        /// The reduce tail as one object (`MatchTail`, pattern_policies —
        /// the reduce bundle plus the used-rel carry decision). Its own
        /// lifetime is separate from the cap's: the cap rides through a
        /// mutable reference (invariant, it pins `'data`), while the tail
        /// holds shared references whose lifetimes covariantly shorten to
        /// the rider's.
        tail: MatchTail<'a>,
        out: &'a mut Matched,
    },
    /// An EXISTS probe: the residual (the body's WHERE, evaluated on the
    /// complete row — the walk applied its ready conjuncts at the binding
    /// points, this pass covers the rest) and then the caller's
    /// predicate; the first accepted row halts the walk. The residual
    /// rides the mounted arm, not the probe: it is `match_gpm_path`'s
    /// one `where_clause` fact, threaded through `mount_rider`.
    Probe {
        probe: &'a PathProbe<'a>,
        residual: &'a Residual<'a>,
    },
}

/// The complete-state delivery policy — the one spelling every per-accept
/// site reads (both `gpm_rel_dfs` arms and the quantified expansion): a
/// mounted rider receives the state (its offer runs the completion
/// pipeline, the reduce tail, the count or the probe-answer policy) and
/// a halt it raised ends the frame; without a rider the state
/// materializes into `out`. Returns whether the walk halted.
pub(super) fn deliver(
    gctx: &GpmCtx<'_>,
    emit: &mut Option<GpmEmit<'_, '_>>,
    state: super::GpmState,
    out: &mut Vec<super::GpmState>,
) -> Result<bool> {
    if let Some(emit) = emit.as_mut() {
        emit.offer(gctx, state)?;
        Ok(gctx.halted())
    } else {
        out.push(state);
        Ok(false)
    }
}

/// The terminal drain — the delivery policy's bulk sibling, composed
/// from the one per-state spelling: the walk's deepest point receives
/// the pattern's complete bindings all at once. A mounted rider consumes
/// each through its offer (a halt stops the drain, nothing continues
/// up); without a rider the states collect into the returned vec for the
/// post-walk candidate loop.
pub(super) fn deliver_all(
    gctx: &GpmCtx<'_>,
    emit: &mut Option<GpmEmit<'_, '_>>,
    states: Vec<super::GpmState>,
) -> Result<Vec<super::GpmState>> {
    let mut out = Vec::new();
    for state in states {
        if gctx.halted() {
            break;
        }
        if deliver(gctx, emit, state, &mut out)? {
            break;
        }
    }
    Ok(out)
}

impl<'a, 'data> GpmEmit<'a, 'data> {
    /// One complete state through the per-candidate pipeline and the
    /// rider — the same completion pipeline, in the same order, the
    /// materializing path's candidate loop applies (`complete_state`, the
    /// one shared spelling, over the arm's own pattern; the reduce tail
    /// through the shared `reduce_row`), so an emitted row is exactly the
    /// row the materialized walk would have emitted.
    pub(super) fn offer(&mut self, gctx: &GpmCtx<'_>, mut state: super::GpmState) -> Result<()> {
        let GpmCtx {
            graph,
            params,
            cypher_version,
            ..
        } = *gctx;
        // the arm's own pattern — the completion pipeline runs once,
        // before the arm match
        let path = match self {
            GpmEmit::Cap { path, .. } => *path,
            GpmEmit::Probe { probe, .. } => probe.pattern,
        };
        if !complete_state(gctx, path, &mut state)? {
            return Ok(());
        }
        match self {
            GpmEmit::Cap { cap, tail, out, .. } => {
                // The materializing path's own fused tail step, shared:
                // `MatchTail::finish_row` (pattern_policies — the one
                // spelling `CollectSink::offer` reads too) runs the
                // reduce's checks and projection and the cap's count on
                // the pre-projection row the checks accepted, so the row
                // pushed here is the row the materialized walk would push
                // by construction.
                let Some(row) = tail.finish_row(graph, state.row, Some(&mut **cap))? else {
                    return Ok(());
                };
                // the shared back half of both cap arms (the row stored
                // with its carried used-set); this arm's own half is the
                // halt consult below
                tail.store_row(row, || state.used, out);
                if cap.full() {
                    gctx.halt.set(true);
                }
            }
            GpmEmit::Probe { probe, residual } => {
                // The one probe-answer policy (PathProbe::accept_row, exec/rider): the
                // deferred element predicates (the row-completion sweep),
                // the residual pass the walk's own application leaves (the
                // start selection and the early conjuncts already decided
                // the ready ones; this covers the rest — later-binding
                // names, rebinding group names), then the predicate, the
                // halt cell on the first accept — the same verdict the
                // materialized rows' answer reads.
                probe.accept_row(
                    graph,
                    state.row,
                    state.used,
                    residual,
                    params,
                    cypher_version,
                )?;
            }
        }
        Ok(())
    }
}

/// The one mount spelling for the walk's rider decision: no rider, or a
/// selector choosing across candidates (a rider's choice is cross-row),
/// keeps the materializing walk and the local halt cell; a mounted rider
/// returns its emit pipeline and halt cell together — the one exhaustive
/// match over the Rider vocabulary.
pub(super) fn mount_rider<'a, 'g, 'cap>(
    rider: Option<&'a mut Rider<'g, 'cap>>,
    path: &'a PathPattern,
    where_clause: &'a Residual<'a>,
    cap_halt: &'a Cell<bool>,
    tail: MatchTail<'a>,
    emitted: &'a mut Matched,
) -> (Option<GpmEmit<'a, 'g>>, &'a Cell<bool>)
where
    'cap: 'a,
    'g: 'a,
{
    let Some(rider) = rider else {
        return (None, cap_halt);
    };
    if path.selector.is_some() {
        return (None, cap_halt);
    }
    match rider {
        Rider::Cap(cap) => (
            Some(GpmEmit::Cap {
                path,
                cap,
                tail,
                out: emitted,
            }),
            cap_halt,
        ),
        Rider::Probe(probe) => (
            Some(GpmEmit::Probe {
                probe,
                residual: where_clause,
            }),
            probe.halt,
        ),
    }
}
