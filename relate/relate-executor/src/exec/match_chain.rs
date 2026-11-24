//! Streaming clause stages (ADR-0555, LABS-1439).
//!
//! The clause driver hands the rows of one clause to the next as a whole
//! vector. For `MATCH (a), (b) … MATCH …` that vector is the cross product
//! of the first clause, retained and charged against the per-value
//! expression cap although the second clause consumes it row by row. The
//! general rule this module states: **a clause that maps each input row to
//! zero or more output rows is a stage, not a barrier**. A run of stages
//! after a `MATCH` is one push pipeline: the head clause's walk offers each
//! finished row to the first stage ([`RowFold`], the contract the
//! aggregation fold already uses), a stage buffers at most [`CHAIN_BATCH`]
//! rows and runs its transform over them, and what the transform emits goes
//! to the next stage the same way — a `MATCH` stage by handing the next
//! stage to its operator ladder as the fold, so its own walk pushes row by
//! row and never builds the batch's output.
//!
//! **Stages** (stream; hold at most a batch per stage):
//! - a plain `MATCH` (default match mode, not `OPTIONAL`, not `SEARCH`);
//! - a projection-only `WITH`: no aggregation, `DISTINCT`, `ORDER BY`,
//!   `SKIP` or `LIMIT` (the `WHERE` filter is per-row and streams).
//!
//! **Barriers** (the pipeline ends before them; the held path takes the
//! rows): the breakers — aggregation, `DISTINCT`, `ORDER BY`, `SKIP`,
//! `LIMIT` — and every clause kind not listed as a stage (`OPTIONAL MATCH`,
//! `SEARCH`, `UNWIND`, `CALL`, subqueries, writes). An aggregating
//! `WITH`/`RETURN` right after the run is the one barrier the pipeline
//! feeds directly: the run's last stage folds into it as rows are emitted
//! (the #110 mechanism), so the breaker holds its own group state and
//! nothing else. Any other barrier receives the last stage's output as a
//! vector; that vector is charged per row into one retaining container
//! (ADR-0463), exactly as the clause's own output vector was, so memory
//! scales with what the last stage emits, never with what an earlier stage
//! produced.
//!
//! Semantics are the held path's. Row order: batches are consecutive
//! slices of the upstream output and every stage emits per input row in
//! input order, so the sequence is unchanged. Relationship uniqueness is
//! per clause (a fresh per-clause used-set either way). The stages read
//! only, so side-effect order is untouched. A deadline or cancellation is
//! observed by the operator ladder's per-row ticks and the per-batch check
//! here. An error surfaces for the same statement but, as ADR-0555 notes,
//! may now surface before an upstream clause has finished.
//!
//! The pipeline stays off a stage the held path serves better or carries
//! behaviour a per-batch run would lose: a `MATCH` an aggregate fold
//! answers in one pass over the whole entering row set, a `MATCH` with a
//! limit cap (its early exit lives in the walker), and a lone trailing
//! `WITH` that would only hand a collector the rows the sink projection
//! already builds. PROFILE runs the pipeline too: a held path under
//! PROFILE would materialize the clause's whole output, changing the
//! memory shape of the very statement PROFILE reports. Each consumed
//! stage records its own facts instead — the sidecar in
//! `profile_chain.rs` ([`StageStats`]).

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashSet;

use relate_core::Result;
use relate_ir::{Clause, Expr, MatchMode, PathPattern};

use crate::ctx::ExecCtx;
use crate::eval::rows_logical_bytes;
use crate::metrics::PlanMetricsSite;
use crate::overlay::{ExecGraph, RetainedTotal};
use crate::row::Row;

use super::aggregate::Aggregation;
use super::classification::ProjectionClass;
use super::feed::RowStream;
use super::match_clause::{
    charge_finished_container, run_match_operator_ladder, LadderRun, MatchClauseInputs,
};
use super::match_folds::fold_would_take;
use super::pattern_policies::{MatchFold, RowFold};
use super::profile_chain::{CountingFold, StageStats};
use super::project::{
    maps_each_row, open_match_fold, project_rows, streams_into_projection, ProjectionInput,
    RowSource,
};
use super::pushdown::{live_from, match_limit_cap, trim_rows};
use super::scope::{carry_scope_bindings, ProjectionFields};
use super::sink::{sinkable_projection, SinkProjection};
use super::{RowFeed, StageDelta};

/// The most rows a stage buffers before it runs its transform over them
/// (ADR-0555: bounded batches, low hundreds by default).
pub(crate) const CHAIN_BATCH: usize = 256;

/// The most stages one run covers. A stage's flush and finish recurse into
/// the next stage, so the run length bounds the native stack depth
/// independently of how long the statement is; clauses past the cap are
/// barriers and take the held path.
const MAX_STAGES: usize = 16;

/// What a stage does to a batch of rows, with everything only that kind of
/// stage reads.
enum Transform<'a> {
    /// A plain `MATCH`.
    Match {
        patterns: &'a [PathPattern],
        where_clause: Option<&'a Expr>,
        /// The sink projection of the clause after the run, set on the
        /// last stage when one is sinkable.
        projection: Option<SinkProjection<'a>>,
        /// What the stage's input rows keep (the suffix's names).
        live_in: Option<HashSet<String>>,
        /// The statement's clauses after this one: the operator ladder
        /// reads the suffix to prove answer shapes, as for the held clause.
        tail: &'a [Clause],
    },
    /// A projection-only `WITH`.
    With {
        spec: ProjectionFields<'a>,
        /// The importing subquery's scope bindings a `WITH` must keep.
        scope: Option<&'a Row>,
    },
}

/// Where a stage's output goes.
enum Next<'a> {
    /// The next stage of the run.
    Stage(Box<Stage<'a>>),
    /// The aggregation of the barrier after the run.
    Fold(Box<MatchFold<'a>>),
    /// The vector the barrier after the run receives.
    Rows(Collector),
}

impl Next<'_> {
    /// The consumer of what a stage emits: every kind of consumer takes
    /// rows through the one [`RowFold`] contract.
    fn consumer(&self) -> &dyn RowFold {
        match self {
            Next::Stage(stage) => stage.as_ref(),
            Next::Fold(fold) => fold.as_ref(),
            Next::Rows(collector) => collector,
        }
    }
}

/// The run's retained output: what the barrier after the run takes as its
/// input rows, charged per row (ADR-0463).
struct Collector {
    shape: Collected,
    rows: RefCell<Vec<Row>>,
    retained: RefCell<RetainedTotal>,
}

/// What a collector's rows are, decided once at open.
enum Collected {
    /// Bindings; the set is what the barrier after the run still names,
    /// `None` keeps every binding. A `WITH`'s rows (its projection already
    /// dropped the rest) are never trimmed.
    Bindings(Option<HashSet<String>>),
    /// The sink projection's columns (their columns are the output),
    /// never trimmed.
    SinkProjected,
}

impl Collected {
    fn source(&self) -> RowSource {
        match self {
            Self::Bindings(_) => RowSource::Bindings,
            Self::SinkProjected => RowSource::SinkProjected,
        }
    }
}

impl RowFold for Collector {
    fn offer(&self, graph: &ExecGraph<'_>, mut row: Row) -> Result<()> {
        if let Collected::Bindings(Some(live)) = &self.shape {
            trim_rows(graph, std::slice::from_mut(&mut row), live);
        }
        self.retained.borrow_mut().add_row(graph, row.values())?;
        self.rows.borrow_mut().push(row);
        Ok(())
    }
}

/// One clause of the run: its transform, its input buffer and its consumer.
struct Stage<'a> {
    ctx: ExecCtx<'a>,
    transform: Transform<'a>,
    buffer: RefCell<Vec<Row>>,
    /// Whether the transform has run at least once.
    ran: Cell<bool>,
    next: Next<'a>,
    /// Rows offered into the stage across its batches — the head stage's
    /// figure is the head clause's offered-row fact (what its walk fed
    /// the run), the count the landing reports. A production fact, not an
    /// instrumentation one: one `Cell` add per batch, the
    /// `MatchFold::offer` and `StreamedStats::note_row` precedent.
    rows_in: Cell<u64>,
    /// The stage's PROFILE facts (`profile_chain.rs`), present only when
    /// the run observes itself, reported once it drains.
    stats: Option<StageStats>,
}

/// A run of streamed clauses after a `MATCH`: the push consumer of the
/// head clause's walk.
pub(crate) struct MatchChain<'a> {
    head: Stage<'a>,
    /// How many clauses after the head the run covers.
    stages: usize,
    /// The PROFILE site with the head clause's index: present when the
    /// statement is PROFILEd — the run then observes itself through the
    /// stage sidecars and reports each consumed stage's own facts when it
    /// drains. Its absence is the untimed form: the stages' windows and
    /// notes run bare.
    record: Option<(PlanMetricsSite<'a>, usize)>,
}

/// The stage `clauses[0]` is, or `None` when it is a barrier.
///
/// A plain `MATCH` is a stage unless the held path serves it better: the
/// fold family taking it with its aggregate, a limit cap stopping its walk,
/// or any shape a per-batch run would not serve identically. A `WITH` is a
/// stage when it only maps rows.
fn stage_transform<'a>(
    ctx: ExecCtx<'a>,
    clauses: &'a [Clause],
    scope: Option<&'a Row>,
) -> Result<Option<Transform<'a>>> {
    match &clauses[0] {
        Clause::Match {
            optional: false,
            match_mode: MatchMode::DifferentRelationships,
            patterns,
            where_clause,
            search: None,
            using_index: _,
            using_join: _,
        } => {
            let ExecCtx { graph, params, .. } = ctx;
            // The widest cap arming (one entering row) is the conservative
            // question.
            if match_limit_cap(
                &clauses[1..],
                patterns,
                where_clause.as_ref(),
                graph,
                params,
                1,
            )?
            .is_some()
            {
                return Ok(None);
            }
            // The fold family answers a `MATCH` plus its aggregate in one
            // pass over the whole entering row set; a rung that would take
            // this clause keeps it out of the streaming stages.
            if fold_would_take(graph, clauses, params)? {
                return Ok(None);
            }
            Ok(Some(Transform::Match {
                patterns,
                where_clause: where_clause.as_ref(),
                projection: None,
                live_in: live_from(clauses),
                tail: &clauses[1..],
            }))
        }
        Clause::With { .. } => {
            let spec = ProjectionFields::of_projection(&clauses[0])?;
            Ok(maps_each_row(&spec).then_some(Transform::With { spec, scope }))
        }
        _ => Ok(None),
    }
}

/// The run of stages starting at `clauses[0]`. A trailing `WITH` whose
/// barrier is not an aggregation is dropped from the run: it would only
/// feed a collector what the preceding `MATCH`'s sink projection already
/// builds.
fn stage_run<'a>(
    ctx: ExecCtx<'a>,
    clauses: &'a [Clause],
    scope: Option<&'a Row>,
) -> Result<Vec<Transform<'a>>> {
    let mut run = Vec::new();
    while run.len() < MAX_STAGES {
        let Some(rest) = clauses.get(run.len()..).filter(|rest| !rest.is_empty()) else {
            break;
        };
        let Some(transform) = stage_transform(ctx, rest, scope)? else {
            break;
        };
        run.push(transform);
    }
    while matches!(run.last(), Some(Transform::With { .. }))
        && !streams_into_projection(clauses.get(run.len()))
    {
        run.pop();
    }
    Ok(run)
}

/// Open the pipeline for the `MATCH` at `clauses[0]`: the stages that
/// follow it, ending at the first barrier. `record` is the statement's
/// PROFILE site paired with the head clause's index (the consumed stages
/// report at `head clause + 1 + position`), `None` for an untimed run.
/// `None` when no clause after the
/// head streams. The caller has already decided the head streams to no
/// other consumer (no cap or fold) and is neither `OPTIONAL` nor a
/// `SEARCH`. `class_slot` is the part's one aggregation classification slot; it is
/// claimed only once every other check has passed.
pub(crate) fn open_match_chain<'a>(
    ctx: ExecCtx<'a>,
    clauses: &'a [Clause],
    class_slot: &'a OnceCell<ProjectionClass<'a>>,
    scope: Option<&'a Row>,
    record: Option<(PlanMetricsSite<'a>, usize)>,
) -> Result<Option<MatchChain<'a>>> {
    let mut transforms = stage_run(ctx, &clauses[1..], scope)?;
    let stages = transforms.len();
    if stages == 0 {
        return Ok(None);
    }
    let ExecCtx { graph, params, .. } = ctx;
    // The run's consumer: the barrier after it. An aggregating clause is
    // fed directly (when its order slot is free); any other receives the
    // last stage's retained output.
    let after = clauses.get(1 + stages);
    let terminal = match open_match_fold(graph, after, class_slot, params, scope)? {
        Some(fold) => Next::Fold(Box::new(fold)),
        None => {
            let mut collector = Collector {
                shape: Collected::Bindings(None),
                rows: RefCell::new(Vec::new()),
                retained: RefCell::new(graph.retain_bytes(0)?),
            };
            // A last `MATCH` owns the barrier's sink projection, else
            // its bindings are trimmed to what the barrier names.
            if let Some(Transform::Match { projection, .. }) = transforms.last_mut() {
                *projection = after.and_then(|next| sinkable_projection(graph, next));
                if projection.is_some() {
                    collector.shape = Collected::SinkProjected;
                } else {
                    collector.shape = Collected::Bindings(live_from(&clauses[1 + stages..]));
                }
            }
            Next::Rows(collector)
        }
    };
    // Built back to front: each stage owns the stage after it.
    let mut run = transforms.into_iter().rev();
    let last = run.next().expect("a non-empty run");
    let observing = record.is_some();
    let stage = |transform, next| Stage {
        ctx,
        transform,
        buffer: RefCell::new(Vec::new()),
        ran: Cell::new(false),
        next,
        rows_in: Cell::new(0),
        stats: observing.then(StageStats::new),
    };
    let head = run.fold(stage(last, terminal), |after, transform| {
        stage(transform, Next::Stage(Box::new(after)))
    });
    Ok(Some(MatchChain {
        head,
        stages,
        record,
    }))
}

impl<'a> Stage<'a> {
    /// Run the transform over the buffered rows and pass what it emits on.
    fn flush(&self, graph: &ExecGraph<'_>) -> Result<()> {
        self.observed(graph, || self.run_batch(graph))
    }

    /// Run `work` inside the stage's PROFILE window when the run observes
    /// itself, bare otherwise — the one place the sidecar's presence
    /// decides, so `flush` and `finish` state only what they wrap.
    fn observed<T>(&self, graph: &ExecGraph<'_>, work: impl FnOnce() -> Result<T>) -> Result<T> {
        match &self.stats {
            Some(stats) => stats.window(graph, work),
            None => work(),
        }
    }

    /// One transform run: the buffered batch through the transform, its
    /// output offered to the consumer. The PROFILE window — the batch
    /// flush, or the drain inside [`Stage::finish`] — is the caller's, so
    /// the stats' windows nest exactly once per run; the walk-start swap
    /// lives in the window (`profile_chain.rs`), and a `MATCH` transform
    /// captures its own first walk start while its ladder holds the slot.
    fn run_batch(&self, graph: &ExecGraph<'_>) -> Result<()> {
        let mut batch = std::mem::take(&mut *self.buffer.borrow_mut());
        graph.check_cancelled()?;
        self.ran.set(true);
        self.rows_in.set(self.rows_in.get() + batch.len() as u64);
        // The stage's consumer, wrapped to count at the emission boundary
        // when the run observes itself: a fold consumer takes rows as they
        // are offered, so the transform's returned vector holds none of
        // them and cannot be the count.
        let next_consumer = self.next.consumer();
        let counting;
        let consumer: &dyn RowFold = if let Some(stats) = &self.stats {
            counting = CountingFold::new(next_consumer, stats);
            &counting
        } else {
            next_consumer
        };
        let rows = match &self.transform {
            Transform::Match {
                patterns,
                where_clause,
                projection,
                live_in,
                tail,
            } => {
                if let Some(live) = live_in {
                    trim_rows(graph, &mut batch, live);
                }
                // A stage hands its walk the stage after it as the fold, so
                // the walk pushes row by row; the last stage's rows come
                // back (a sink projection) for the collector.
                let fold = (!matches!(self.next, Next::Rows(_))).then_some(consumer);
                let LadderRun { rows, facts } = run_match_operator_ladder(
                    self.ctx,
                    batch,
                    MatchClauseInputs {
                        optional: false,
                        match_mode: MatchMode::DifferentRelationships,
                        patterns,
                        where_clause: *where_clause,
                    },
                    None,
                    projection.as_ref(),
                    tail,
                    fold,
                )?;
                if let Some(stats) = &self.stats {
                    stats.note_facts(facts);
                    stats.note_start(super::take_walk_start());
                }
                rows
            }
            Transform::With { spec, scope } => {
                let mut rows = project_rows(
                    self.ctx,
                    ProjectionInput::Feed(RowFeed::Held(RowStream::held(batch))),
                    spec,
                    false,
                    *scope,
                )?;
                // An importing subquery's scope bindings survive a `WITH`.
                if let Some(scope) = scope {
                    carry_scope_bindings(&mut rows, scope);
                }
                rows
            }
        };
        rows.into_iter()
            .try_for_each(|row| consumer.offer(graph, row))
    }

    /// Run the remaining buffered rows, then close the stages after this
    /// one. A stage that never ran still runs its transform once over no
    /// rows, as the held clause does. One PROFILE window covers the whole
    /// drain — this stage's last batch and every downstream stage's — so
    /// the stage's totals keep covering everything downstream did.
    fn finish(&self, graph: &ExecGraph<'_>) -> Result<()> {
        self.observed(graph, || self.drain(graph))
    }

    /// The drain itself: this stage's last batch, then the stages after
    /// it closing in turn.
    fn drain(&self, graph: &ExecGraph<'_>) -> Result<()> {
        if !self.buffer.borrow().is_empty() || !self.ran.get() {
            self.run_batch(graph)?;
        }
        match &self.next {
            Next::Stage(stage) => stage.finish(graph),
            Next::Fold(_) | Next::Rows(_) => Ok(()),
        }
    }
}

impl RowFold for Stage<'_> {
    fn offer(&self, graph: &ExecGraph<'_>, row: Row) -> Result<()> {
        let full = {
            let mut buffer = self.buffer.borrow_mut();
            buffer.push(row);
            buffer.len() >= CHAIN_BATCH
        };
        if full {
            self.flush(graph)?;
        }
        Ok(())
    }

    /// A rung's finished container headed to a stage is retained until it
    /// is offered; the pipeline bounds that figure as the held path did.
    fn offer_container(&self, graph: &ExecGraph<'_>, rows: Vec<Row>) -> Result<()> {
        charge_finished_container(graph, &rows)?;
        rows.into_iter().try_for_each(|row| self.offer(graph, row))
    }
}

impl<'a> MatchChain<'a> {
    /// Run what is still buffered through every stage and hand the run's
    /// [`Landing`] back with its PROFILE evidence beside it: the head
    /// clause's offered-row count (the head stage's input count, complete
    /// only once the drain has flushed its last batch, so it is read here
    /// and never before), the covered-clause count, what the terminal
    /// left, and — only when the run observes itself — the run's window
    /// totals for the arm's delta to subtract. The consumed stages'
    /// PROFILE records are written here too, before the terminal's rows
    /// leave the run — the terminal stage's record carries the bytes its
    /// collector retains.
    fn finish(self, graph: &ExecGraph<'_>) -> Result<(Option<StageDelta>, Landing<'a>)> {
        self.head.finish(graph)?;
        let offered = self.head.rows_in.get();
        // The run's window totals — the head stage's, which contain every
        // stage below it — for the arm's own delta to subtract, so the
        // head clause's record states its own clause's work and never the
        // run's. PROFILE's own evidence, produced only when the run
        // observes itself; the landing stays production-only.
        let observed = self.head.stats.as_ref().map(StageStats::totals);
        // The consumed stages' records are written before the terminal's
        // rows leave the run — the terminal stage's carries the bytes its
        // collector holds. The whole traversal is PROFILE's: an untimed
        // run reports nothing and walks once, straight to the terminal.
        self.report_profiled();
        let mut stage = self.head;
        let kind = loop {
            match stage.next {
                Next::Stage(next) => stage = *next,
                Next::Fold(fold) => break LandingKind::Folded(Box::new((*fold).into_parts().0)),
                Next::Rows(collector) => {
                    let mut rows = collector.rows.into_inner();
                    Row::share_schemas(&mut rows);
                    break LandingKind::Rows {
                        rows,
                        source: collector.shape.source(),
                    };
                }
            }
        };
        Ok((
            observed,
            Landing {
                offered,
                consumed: self.stages,
                kind,
            },
        ))
    }

    /// Report each consumed stage's own PROFILE facts once the run has
    /// drained: one borrowed walk over the stage list, the terminal
    /// stage's retained bytes captured where the walk meets the collector,
    /// then one record per stage, tail-first, with the immediate
    /// downstream stage's totals subtracted out. Nothing at all runs
    /// without a site — an untimed run performs neither the traversal nor
    /// the byte sum.
    fn report_profiled(&self) {
        let Some((site, head_clause)) = self.record.as_ref() else {
            return;
        };
        let mut stages: Vec<&Stage> = Vec::with_capacity(self.stages);
        let mut stage = &self.head;
        let mut retained = 0;
        loop {
            stages.push(stage);
            match &stage.next {
                Next::Stage(next) => stage = next,
                Next::Rows(collector) => {
                    retained = rows_logical_bytes(&collector.rows.borrow());
                    break;
                }
                Next::Fold(_) => break,
            }
        }
        // Tail-first: each stage's own facts are its totals minus the
        // immediate downstream stage's totals — which contain every stage
        // below it, so summing the tail would count each deeper stage
        // twice — and each report hands its totals upstream as exactly
        // that subtrahend. The terminal stage's record carries the
        // retention its collector holds until the barrier consumes it;
        // every other stage's in-flight batch is bounded and reports
        // zero.
        let last = stages.len().saturating_sub(1);
        let mut downstream = StageDelta::default();
        for (idx, stage) in stages.iter().enumerate().rev() {
            let memory_bytes = if idx == last { retained } else { 0 };
            let stats = stage
                .stats
                .as_ref()
                .expect("an observing run carries every stage's sidecar");
            downstream = stats.report(site, head_clause + 1 + idx, downstream, memory_bytes);
        }
    }
}

impl RowFold for MatchChain<'_> {
    fn offer(&self, graph: &ExecGraph<'_>, row: Row) -> Result<()> {
        self.head.offer(graph, row)
    }

    fn offer_container(&self, graph: &ExecGraph<'_>, rows: Vec<Row>) -> Result<()> {
        self.head.offer_container(graph, rows)
    }
}

/// What takes a `MATCH`'s rows as they are emitted instead of the clause
/// collecting them: the aggregation of a following aggregating
/// `WITH`/`RETURN`, or the run of stages after it.
// The chain's PROFILE window totals (one `StageDelta`, now carrying the memo
// counters) put its size past the lint's variant-difference budget; a consumer
// is built once per clause and moves by value, so boxing it buys nothing.
#[allow(clippy::large_enum_variant)]
pub(crate) enum MatchConsumer<'a> {
    Fold(MatchFold<'a>),
    Chain(MatchChain<'a>),
}

/// What a consumer leaves for the clause after the clauses it covered:
/// the two facts every landing shares — the rows the head clause's walk
/// offered the consumer (its PROFILE row count, the same fact for a
/// directly fed fold and for a run of stages) and the clauses the run
/// covered — plus what the terminal left.
pub(crate) struct Landing<'a> {
    pub(crate) offered: u64,
    pub(crate) consumed: usize,
    pub(crate) kind: LandingKind<'a>,
}

/// The terminal's own leaving: the rows went into an aggregation, or the
/// last stage's retained output (the terminal stage's PROFILE record
/// reports the retention its collector held until the barrier consumed
/// it).
pub(crate) enum LandingKind<'a> {
    Folded(Box<Aggregation<'a>>),
    Rows { rows: Vec<Row>, source: RowSource },
}

/// The consumer for the uncapped, non-`OPTIONAL` `MATCH` at `clauses[0]`,
/// or `None` when its rows are collected. An aggregating clause right after
/// takes them directly; otherwise the stages after it do — under PROFILE
/// too. PROFILE observes the streamed run instead of holding the path —
/// through the head clause's offered-row count and each consumed stage's
/// own facts (`StageStats`) — because a held path under PROFILE would
/// materialize the clause's whole output, changing the memory shape of the
/// very statement PROFILE reports.
pub(crate) fn open_match_consumer<'a>(
    ctx: ExecCtx<'a>,
    clauses: &'a [Clause],
    class_slot: &'a OnceCell<ProjectionClass<'a>>,
    scope: Option<&'a Row>,
    record: Option<(PlanMetricsSite<'a>, usize)>,
) -> Result<Option<MatchConsumer<'a>>> {
    let ExecCtx { graph, params, .. } = ctx;
    if let Some(fold) = open_match_fold(graph, clauses.get(1), class_slot, params, scope)? {
        return Ok(Some(MatchConsumer::Fold(fold)));
    }
    Ok(open_match_chain(ctx, clauses, class_slot, scope, record)?.map(MatchConsumer::Chain))
}

impl<'a> MatchConsumer<'a> {
    /// The push contract the head clause's walk offers its rows to.
    pub(crate) fn as_fold(&self) -> &dyn RowFold {
        match self {
            MatchConsumer::Fold(fold) => fold,
            MatchConsumer::Chain(chain) => chain,
        }
    }

    /// Close the consumer once the head clause has emitted everything:
    /// the landing, and — only when a run of stages observed itself — its
    /// window totals (a directly fed fold's offer-time grouping is the
    /// head clause's own work, the established fold semantics, so a fold
    /// lands no evidence).
    pub(crate) fn finish(self, graph: &ExecGraph<'_>) -> Result<(Option<StageDelta>, Landing<'a>)> {
        match self {
            MatchConsumer::Fold(fold) => {
                let (aggregation, offered) = fold.into_parts();
                Ok((
                    None,
                    Landing {
                        offered,
                        consumed: 0,
                        kind: LandingKind::Folded(Box::new(aggregation)),
                    },
                ))
            }
            MatchConsumer::Chain(chain) => chain.finish(graph),
        }
    }
}
