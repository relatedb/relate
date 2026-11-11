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
pub(crate) struct MatchedSink<'r, 'a> {
    residual: &'r Residual<'r>,
    kind: SinkKind<'r, 'a>,
    /// Rows a fold sink has accepted (the fold ran to completion on them).
    folded: usize,
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
        /// The collecting arm's halt, raised once the cap is full: the
        /// consumer needs no more rows, so the search feeding this sink
        /// stops exactly as a probe's accept stops it. The cell lives
        /// outside the sink because the search holds a reference to it
        /// while the sink is borrowed mutably.
        halt: &'r Cell<bool>,
    },
    /// An expression consumer's fold (the expression-form shortest path):
    /// each bound row goes straight into the caller's reducer, so memory
    /// follows what the reducer keeps, never the row count. No tail, no
    /// cap: the fold sees every bound row.
    Fold {
        fold: &'r mut dyn FnMut(Row) -> Result<()>,
        halt: &'r Cell<bool>,
    },
}

impl<'r, 'a> MatchedSink<'r, 'a> {
    /// The one rider→sink decision — a single exhaustive match over the
    /// rider vocabulary, the shape `PathSink::for_rider` gives the
    /// classic walk. A probe answers; a cap or no rider at all collects
    /// through the tail, charging `cap` per row and raising `collect_halt`
    /// once it is full.
    pub(crate) fn new(
        rider: Option<&'r mut Rider<'a, '_>>,
        collect_halt: &'r Cell<bool>,
        residual: &'r Residual<'r>,
        tail: MatchTail<'r>,
        out: &'r mut Matched,
    ) -> Self {
        let kind = match rider {
            Some(Rider::Probe(probe)) => SinkKind::Probe(probe),
            Some(Rider::Cap(cap)) => SinkKind::Collect {
                tail,
                out,
                cap: Some(&mut **cap),
                halt: collect_halt,
            },
            None => SinkKind::Collect {
                tail,
                out,
                cap: None,
                halt: collect_halt,
            },
        };
        Self {
            residual,
            kind,
            folded: 0,
        }
    }

    /// A sink that folds each row into `fold` as the search binds it. The
    /// halt cell is never raised by the sink: the fold consumes every row.
    pub(crate) fn fold(
        residual: &'r Residual<'r>,
        halt: &'r Cell<bool>,
        fold: &'r mut dyn FnMut(Row) -> Result<()>,
    ) -> Self {
        Self {
            residual,
            kind: SinkKind::Fold { fold, halt },
            folded: 0,
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
        match &self.kind {
            SinkKind::Probe(probe) => probe.halt,
            SinkKind::Collect { halt, .. } | SinkKind::Fold { halt, .. } => halt,
        }
    }

    /// Rows the fold has accepted so far (always 0 for a non-fold sink): a
    /// caller comparing two reads learns whether a step produced any.
    pub(super) fn folded(&self) -> usize {
        self.folded
    }

    /// Whether the consumer is answered — the one halt query, over both
    /// arms, for the callers that drive the sink themselves (the selector
    /// fallback's batch loop).
    pub(super) fn halted(&self) -> bool {
        self.halt_cell().get()
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
        let residual = self.residual;
        match &mut self.kind {
            SinkKind::Probe(probe) => {
                ctx.graph.cancel_tick()?;
                probe.accept_row(
                    ctx.graph,
                    row,
                    used(),
                    residual,
                    ctx.params,
                    ctx.cypher_version,
                )?;
            }
            SinkKind::Collect {
                tail,
                out,
                cap,
                halt,
            } => {
                if let Some(row) = tail.finish_row(ctx.graph, row, cap.as_deref_mut())? {
                    tail.store_row(row, used, out);
                }
                if cap.as_ref().is_some_and(|cap| cap.full()) {
                    halt.set(true);
                }
            }
            SinkKind::Fold { fold, .. } => {
                ctx.graph.cancel_tick()?;
                fold(row)?;
                self.folded += 1;
            }
        }
        Ok(())
    }
}
