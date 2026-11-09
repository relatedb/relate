//! Where a routed shortest path's rows go (ADR-0555): each bound row is
//! pushed into the consumer the moment it is selected — an EXISTS probe
//! that answers from it (and halts the search at the first accept), or the
//! reduce tail that filters, projects and stores it into the caller's
//! vector — instead of the route building a vector of every row for the
//! caller to walk a second time.

use std::cell::Cell;

use relate_core::{RelId, Result};

use crate::ctx::ExecCtx;
use crate::exec::pattern_policies::{MatchTail, Matched};
use crate::exec::pushdown::Residual;
use crate::exec::rider::{PathProbe, Rider};
use crate::row::Row;
use crate::UsedSet;

/// The consumer of one route run's rows, with the MATCH-level residual
/// the route chooses its path against (the one `where_clause` fact the
/// caller threads — a probe answers through it too).
pub(crate) struct RouteSink<'a> {
    residual: &'a Residual<'a>,
    kind: SinkKind<'a>,
}

enum SinkKind<'a> {
    /// An EXISTS probe: each row is answered through the shared accept
    /// policy (`PathProbe::accept_row`); the first accept sets the probe's
    /// halt cell and the route stops. No row is kept — the verdict is the
    /// cell.
    Probe(&'a PathProbe<'a>),
    /// The reduce tail: the shared fused step (`MatchTail::finish_row`,
    /// cap `None` — a cap does not ride the route), the surviving row
    /// stored with its carried used-set into the caller's vector.
    Collect {
        tail: MatchTail<'a>,
        out: &'a mut Matched,
    },
}

impl<'a> RouteSink<'a> {
    /// A probe rider answers; anything else (a cap, which never rides the
    /// route, or no rider) reduces through the tail into `out`.
    pub(crate) fn new(
        rider: Option<&'a Rider<'_, '_>>,
        residual: &'a Residual<'a>,
        tail: MatchTail<'a>,
        out: &'a mut Matched,
    ) -> Self {
        let kind = match rider.and_then(Rider::probe) {
            Some(probe) => SinkKind::Probe(probe),
            None => SinkKind::Collect { tail, out },
        };
        Self { residual, kind }
    }

    /// The MATCH-level residual the sink was built with.
    pub(super) fn residual(&self) -> &'a Residual<'a> {
        self.residual
    }

    /// The cell the consumer raises when it needs no more rows: a probe's
    /// verdict cell. A collecting sink never halts, so it has none. The
    /// route owns reading it (`Route::halted`) — one reader, not a check
    /// pasted at every level.
    pub(super) fn halt_cell(&self) -> Option<&'a Cell<bool>> {
        match &self.kind {
            SinkKind::Probe(probe) => Some(probe.halt),
            SinkKind::Collect { .. } => None,
        }
    }

    /// Take one selected row with the trail it was bound from.
    pub(super) fn offer(&mut self, ctx: ExecCtx<'_>, row: Row, trail: &[RelId]) -> Result<()> {
        let used = || trail.iter().copied().collect::<UsedSet>();
        match &mut self.kind {
            SinkKind::Probe(probe) => {
                ctx.graph.cancel_tick()?;
                probe.accept_row(
                    ctx.graph,
                    row,
                    used(),
                    self.residual,
                    ctx.params,
                    ctx.cypher_version,
                )?;
            }
            SinkKind::Collect { tail, out } => {
                if let Some(row) = tail.finish_row(ctx.graph, row, None)? {
                    tail.store_row(row, used, out);
                }
            }
        }
        Ok(())
    }
}
