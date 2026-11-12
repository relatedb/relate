//! Where a materializing match's rows go (ADR-0555): the one sink both
//! tails push through — the shortest route's bound rows as the search
//! finds them, and the GPM selector's chosen candidates once the walk has
//! enumerated them. Each row is offered the moment it exists: an EXISTS
//! probe answers from it (and halts the search at its first accept), the
//! reduce tail filters, projects and stores it into the caller's vector,
//! charging the MATCH cap at the same `finish_row` seam the riding tail
//! charges at.
//!
//! One sink, not one per caller: the probe-or-collect policy and its halt
//! contract are stated here once, so a routed `LIMIT` and a selector
//! `LIMIT` cannot drift apart in either the count or the stopping rule.

use std::cell::Cell;

use relate_core::Result;

use crate::ctx::ExecCtx;
use crate::exec::pattern_policies::{MatchTail, Matched};
use crate::exec::pushdown::{MatchCap, Residual};
use crate::exec::rider::{PathProbe, Rider};
use crate::row::Row;
use crate::UsedSet;

/// The consumer of a materializing match's rows, with the MATCH-level
/// residual its probe answers through (the one `where_clause` fact the
/// caller threads).
pub(super) struct MatchedSink<'r, 'a> {
    residual: &'r Residual<'r>,
    kind: SinkKind<'r, 'a>,
    /// The cell the consumer raises when it needs no more rows: a probe's
    /// verdict cell, or the collecting arm's cap-full cell. One field, set
    /// by the one rider match `new` already runs — an arm cannot carry a
    /// different cell from the one `halted` reads. The cell itself lives
    /// outside the sink because the search holds a reference to it while
    /// the sink is borrowed mutably.
    halt: &'r Cell<bool>,
}

enum SinkKind<'r, 'a> {
    /// An EXISTS probe: each row is answered through the shared accept
    /// policy (`PathProbe::accept_row`); the first accept sets the probe's
    /// halt cell and the search stops. No row is kept — the verdict is the
    /// cell.
    Probe(&'a PathProbe<'a>),
    /// The reduce tail: the shared fused step (`MatchTail::finish_row`)
    /// with the rider's cap, so a `LIMIT` charges at the same seam the
    /// riding tail charges at — the *pre-projection* row the cap's
    /// contract counts (a `Distinct` cap evaluates its RETURN expressions
    /// on it, which a projected row cannot satisfy). The surviving row is
    /// stored with its carried used-set into the caller's vector.
    Collect {
        tail: MatchTail<'r>,
        out: &'r mut Matched,
        cap: Option<&'r mut MatchCap<'a>>,
    },
    /// An expression consumer's fold (the expression-form shortest path):
    /// each bound row goes straight into the caller's reducer, so memory
    /// follows what the reducer keeps, never the row count. No tail, no
    /// cap: the fold sees every bound row.
    Fold {
        fold: &'r mut dyn FnMut(Row) -> Result<()>,
        /// Rows the fold has accepted (the fold ran to completion on them):
        /// the Fold arm's own state — no other arm touches it, so it lives
        /// here, not on the shared struct.
        folded: usize,
    },
}

impl<'r, 'a> MatchedSink<'r, 'a> {
    /// The one rider→sink decision — a single exhaustive match over the
    /// rider vocabulary, the shape `PathSink::for_rider` gives the
    /// classic walk. A probe answers; a cap or no rider at all collects
    /// through the tail, charging `cap` per row and raising `collect_halt`
    /// once it is full.
    pub(super) fn new(
        rider: Option<&'r mut Rider<'a, '_>>,
        collect_halt: &'r Cell<bool>,
        residual: &'r Residual<'r>,
        tail: MatchTail<'r>,
        out: &'r mut Matched,
    ) -> Self {
        let (kind, halt) = match rider {
            Some(Rider::Probe(probe)) => (SinkKind::Probe(probe), probe.halt),
            // the two collecting arms differ only in whether a rider
            // supplied a cap — the field itself
            Some(Rider::Cap(cap)) => (
                SinkKind::Collect {
                    tail,
                    out,
                    cap: Some(&mut **cap),
                },
                collect_halt,
            ),
            None => (
                SinkKind::Collect {
                    tail,
                    out,
                    cap: None,
                },
                collect_halt,
            ),
        };
        Self {
            residual,
            kind,
            halt,
        }
    }

    /// A sink that folds each row into `fold` as the search binds it. The
    /// halt cell is never raised by the sink: the fold consumes every row.
    pub(super) fn fold(
        residual: &'r Residual<'r>,
        halt: &'r Cell<bool>,
        fold: &'r mut dyn FnMut(Row) -> Result<()>,
    ) -> Self {
        Self {
            residual,
            kind: SinkKind::Fold { fold, folded: 0 },
            halt,
        }
    }

    /// The MATCH-level residual the sink was built with.
    pub(super) fn residual(&self) -> &'r Residual<'r> {
        self.residual
    }

    /// The cell the consumer raises when it needs no more rows: a probe's
    /// verdict cell, or the collecting arm's cap-full cell. The search
    /// owns reading it (`Route::halted`) — one reader, not a check pasted
    /// at every level.
    pub(super) fn halt_cell(&self) -> &'r Cell<bool> {
        self.halt
    }

    /// Rows the fold has accepted so far (always 0 for a non-fold sink): a
    /// caller comparing two reads learns whether a step produced any.
    pub(super) fn folded(&self) -> usize {
        match &self.kind {
            SinkKind::Fold { folded, .. } => *folded,
            _ => 0,
        }
    }

    /// Whether the consumer is answered — the one halt query, over both
    /// arms, for the callers that drive the sink themselves (the selector
    /// fallback's batch loop).
    pub(super) fn halted(&self) -> bool {
        self.halt.get()
    }

    /// Take one selected row with the used-set it carries. The set arrives
    /// as a closure, not a value: a `Finish` tail drops it unread
    /// (`MatchTail::store_row`), and building one per offered row to throw
    /// it away is an allocation per row on every route emission.
    pub(super) fn offer(
        &mut self,
        ctx: ExecCtx<'_>,
        row: Row,
        used: impl FnOnce() -> UsedSet,
    ) -> Result<()> {
        // One cancellation rule for every rider kind and every caller:
        // whether an offered row is interruptible must not depend on which
        // arm happens to consume it.
        ctx.graph.cancel_tick()?;
        let residual = self.residual;
        match &mut self.kind {
            SinkKind::Probe(probe) => {
                probe.accept_row(
                    ctx.graph,
                    row,
                    used(),
                    residual,
                    ctx.params,
                    ctx.cypher_version,
                )?;
            }
            SinkKind::Collect { tail, out, cap } => {
                if let Some(row) = tail.finish_row(ctx.graph, row, cap.as_deref_mut())? {
                    tail.store_row(row, used, out);
                }
                if cap.as_ref().is_some_and(|cap| cap.full()) {
                    self.halt.set(true);
                }
            }
            SinkKind::Fold { fold, folded } => {
                fold(row)?;
                *folded += 1;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::ctx::ExecCtx;
    use crate::exec::pushdown::Residual;
    use crate::graph::Graph;
    use crate::ExecGraph;

    /// The sink's own cancellation rule, on the arm that had none: a collect
    /// sink offered a row on a cancelled statement stops, exactly as the
    /// probe arm does — the tick is hoisted above the rider match, so
    /// interruptibility cannot depend on which rider kind consumes the row.
    ///
    /// Only a direct offer distinguishes this: the GPM walk (`walk.rs`) and
    /// the shortest route (`route.rs`) each tick cancellation before they
    /// reach a sink, so an end-to-end cancelled-match test passes with or
    /// without the hoist.
    #[test]
    fn a_cancelled_collect_offer_stops() {
        let g = Graph::default();
        let token = relate_core::CancelToken::new();
        token.cancel();
        let graph = ExecGraph::read(&g).with_cancel(Some(&token));
        let params = BTreeMap::new();
        let ctx = ExecCtx::new(&graph, &params, None);
        let residual = Residual::empty();
        let halt = Cell::new(false);
        let mut out: Matched = Vec::new();
        let mut sink = MatchedSink::new(
            None,
            &halt,
            &residual,
            MatchTail::Finish { reduce: None },
            &mut out,
        );
        // Past the sampling window: `cancel_tick` consults the token once
        // per `CANCEL_SAMPLE_MASK + 1` ticks (ADR-0111).
        let mut observed = None;
        for _ in 0..2_048 {
            if let Err(err) = sink.offer(ctx, Row::new(), UsedSet::default) {
                observed = Some(err);
                break;
            }
        }
        let err = observed.expect("the collect arm must observe the cancelled statement");
        assert!(
            err.to_string().contains("query cancelled"),
            "expected the cancellation error, got {err}"
        );
    }
}
