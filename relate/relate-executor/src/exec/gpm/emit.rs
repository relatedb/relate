//! The GPM mounting of the rider a GPM walk can carry instead of
//! materializing every complete state — the rider vocabulary itself
//! (`Rider`, `PathProbe`) lives at its engine-neutral home,
//! `exec/rider.rs`, since the classic walk sinks the same vocabulary
//! (`PathSink`). The mount IS the sink ([`MatchedSink`], gpm/sink.rs),
//! built through the one rider→sink decision, so a MATCH cap charges at
//! the same `finish_row` seam the materializing tail's sink charges at
//! and halts on the same kind of cell, and an EXISTS probe answers
//! through the same accept policy — the rows the walk emits are the rows
//! the materialized walk would emit, in the same order, only sooner,
//! which is what lets the walk halt.

use std::cell::Cell;

use relate_core::Result;
use relate_ir::PathPattern;

use crate::ctx::ExecCtx;

use super::super::pattern_policies::{MatchTail, Matched};
use super::super::pushdown::Residual;
use super::super::rider::Rider;
use super::sink::MatchedSink;
use super::{complete_state, GpmCtx};

/// The mounted rider: the pattern its completion pipeline runs on (the
/// clause's for a cap; a probe's own, fixed at its construction) and the
/// one sink, which owns the per-candidate pipeline's tail — the reduce
/// checks, the projection, the cap's count — and the halt contract.
pub(super) struct GpmEmit<'a, 'data> {
    /// The caller's own context, threaded at mount time: the sink is
    /// offered it unchanged, so a mounted rider sees exactly the context
    /// the selector and route tails hand the same sink — including the
    /// statement's authenticated principal, which a context re-minted
    /// from `GpmCtx`'s three fields would silently drop.
    ctx: ExecCtx<'a>,
    path: &'a PathPattern,
    sink: MatchedSink<'a, 'data>,
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

impl GpmEmit<'_, '_> {
    /// One complete state through the completion pipeline and the sink —
    /// the same pipeline, in the same order, the materializing path's
    /// candidate loop applies (`complete_state`, the one shared
    /// spelling, over the arm's own pattern), so an emitted row is
    /// exactly the row the materialized walk would have emitted.
    pub(super) fn offer(&mut self, gctx: &GpmCtx<'_>, mut state: super::GpmState) -> Result<()> {
        if !complete_state(gctx, self.path, &mut state)? {
            return Ok(());
        }
        self.sink.offer(self.ctx, state.row, || state.used)
    }
}

/// The one mount spelling for the walk's rider decision: no rider, or a
/// selector choosing across candidates (a rider's choice is cross-row),
/// keeps the materializing walk and the local halt cell; a mounted rider
/// becomes the one sink — the same construction the materializing tails
/// push through — and the walk consults the cell that sink raises (the
/// probe's own verdict cell, or the local one a full cap sets).
pub(super) fn mount_rider<'a, 'g, 'cap>(
    ctx: ExecCtx<'a>,
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
    // The pattern the completion pipeline runs on: a probe's own (fixed
    // at its construction — the walk walked the clause's pattern, the
    // probe's answer pipeline completes against its own), the clause's
    // for a cap.
    let path = match &*rider {
        Rider::Probe(probe) => probe.pattern,
        Rider::Cap(_) => path,
    };
    let sink = MatchedSink::new(Some(rider), cap_halt, where_clause, tail, emitted);
    let halt = sink.halt_cell();
    (Some(GpmEmit { ctx, path, sink }), halt)
}
