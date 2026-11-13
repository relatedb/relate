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
//! limit cap (its early exit lives in the walker), a lone trailing `WITH`
//! that would only hand a collector the rows the sink projection already
//! builds, and a PROFILE run (per-clause facts are attributed to clauses
//! run one after the other).

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashSet;

use relate_core::Result;
use relate_ir::{Clause, Expr, MatchMode, PathPattern};

use crate::ctx::ExecCtx;
use crate::overlay::{ExecGraph, RetainedTotal};
use crate::row::Row;

use super::aggregate::Aggregation;
use super::match_clause::{charge_finished_container, run_match_operator_ladder, LadderRun};
use super::match_folds::fold_would_take;
use super::pattern_policies::{MatchFold, RowFold};
use super::project::{
    maps_each_row, open_match_fold, project_rows, streams_into_projection, ProjectSpec,
    ProjectionClass, RowSource,
};
use super::pushdown::{live_from, match_limit_cap, trim_rows};
use super::scope::carry_scope_bindings;
use super::sink::{sinkable_projection, SinkProjection};
use super::RowFeed;

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
        spec: ProjectSpec<'a>,
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
    /// What the barrier after the run still names; `None` keeps every
    /// binding — decided once at open: a sink projection's rows (their
    /// columns are the output) and a `WITH`'s (its projection already
    /// dropped the rest) are never trimmed.
    live: Option<HashSet<String>>,
    /// Whether the rows are bindings or the sink projection's columns.
    source: RowSource,
    rows: RefCell<Vec<Row>>,
    retained: RefCell<RetainedTotal>,
}

impl RowFold for Collector {
    fn offer(&self, graph: &ExecGraph<'_>, mut row: Row) -> Result<()> {
        if let Some(live) = &self.live {
            trim_rows(std::slice::from_mut(&mut row), live);
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
}

/// A run of streamed clauses after a `MATCH`: the push consumer of the
/// head clause's walk.
pub(crate) struct MatchChain<'a> {
    head: Stage<'a>,
    /// How many clauses after the head the run covers.
    stages: usize,
}

/// What the run leaves for the clause after it.
enum ChainOutput<'a> {
    /// The last stage streamed into the aggregation of the barrier after
    /// the run; no rows are held.
    Folded(Box<Aggregation<'a>>),
    /// The last stage's retained output.
    Rows { rows: Vec<Row>, source: RowSource },
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
        Clause::With {
            distinct,
            group_by,
            items,
            order,
            skip,
            limit,
            where_clause,
            ..
        } => {
            let spec = ProjectSpec {
                items,
                distinct: *distinct,
                group_by: group_by.as_deref(),
                order,
                skip,
                limit,
                where_clause: where_clause.as_ref(),
            };
            Ok(maps_each_row(&spec.fields()).then_some(Transform::With { spec, scope }))
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
/// follow it, ending at the first barrier. `None` when no clause after the
/// head streams. The caller has already decided the head streams to no
/// other consumer (no cap or fold) and is neither `OPTIONAL` nor a
/// `SEARCH`. `class_slot` is the part's one aggregation classification slot; it is
/// claimed only once every other check has passed.
pub(crate) fn open_match_chain<'a>(
    ctx: ExecCtx<'a>,
    clauses: &'a [Clause],
    class_slot: &'a OnceCell<ProjectionClass<'a>>,
    scope: Option<&'a Row>,
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
                live: None,
                source: RowSource::Bindings,
                rows: RefCell::new(Vec::new()),
                retained: RefCell::new(graph.retain_bytes(0)?),
            };
            // A last `MATCH` owns the barrier's sink projection, else
            // its bindings are trimmed to what the barrier names.
            if let Some(Transform::Match { projection, .. }) = transforms.last_mut() {
                *projection = after.and_then(sinkable_projection);
                if projection.is_some() {
                    collector.source = RowSource::SinkProjected;
                } else {
                    collector.live = live_from(&clauses[1 + stages..]);
                }
            }
            Next::Rows(collector)
        }
    };
    // Built back to front: each stage owns the stage after it.
    let mut run = transforms.into_iter().rev();
    let last = run.next().expect("a non-empty run");
    let stage = |transform, next| Stage {
        ctx,
        transform,
        buffer: RefCell::new(Vec::new()),
        ran: Cell::new(false),
        next,
    };
    let head = run.fold(stage(last, terminal), |after, transform| {
        stage(transform, Next::Stage(Box::new(after)))
    });
    Ok(Some(MatchChain { head, stages }))
}

impl<'a> Stage<'a> {
    /// Run the transform over the buffered rows and pass what it emits on.
    fn flush(&self, graph: &ExecGraph<'_>) -> Result<()> {
        let mut batch = std::mem::take(&mut *self.buffer.borrow_mut());
        graph.check_cancelled()?;
        self.ran.set(true);
        let rows = match &self.transform {
            Transform::Match {
                patterns,
                where_clause,
                projection,
                live_in,
                tail,
            } => {
                if let Some(live) = live_in {
                    trim_rows(&mut batch, live);
                }
                // A stage hands its walk the stage after it as the fold, so
                // the walk pushes row by row; the last stage's rows come
                // back (a sink projection) for the collector.
                let fold = (!matches!(self.next, Next::Rows(_))).then(|| self.next.consumer());
                let LadderRun { rows, .. } = run_match_operator_ladder(
                    self.ctx,
                    batch,
                    false,
                    MatchMode::DifferentRelationships,
                    patterns,
                    *where_clause,
                    &mut None,
                    projection.as_ref(),
                    tail,
                    fold,
                )?;
                rows
            }
            Transform::With { spec, scope } => {
                let mut rows = project_rows(
                    self.ctx,
                    RowFeed::Held(batch),
                    spec,
                    false,
                    RowSource::Bindings,
                    *scope,
                )?;
                // An importing subquery's scope bindings survive a `WITH`.
                if let Some(scope) = scope {
                    carry_scope_bindings(&mut rows, scope);
                }
                rows
            }
        };
        let consumer = self.next.consumer();
        rows.into_iter()
            .try_for_each(|row| consumer.offer(graph, row))
    }

    /// Run the remaining buffered rows, then close the stages after this
    /// one. A stage that never ran still runs its transform once over no
    /// rows, as the held clause does.
    fn finish(&self, graph: &ExecGraph<'_>) -> Result<()> {
        if !self.buffer.borrow().is_empty() || !self.ran.get() {
            self.flush(graph)?;
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
    /// Run what is still buffered through every stage and hand over what
    /// the last one left.
    fn finish(self, graph: &ExecGraph<'_>) -> Result<ChainOutput<'a>> {
        self.head.finish(graph)?;
        let mut stage = self.head;
        loop {
            match stage.next {
                Next::Stage(next) => stage = *next,
                Next::Fold(fold) => {
                    return Ok(ChainOutput::Folded(Box::new((*fold).into_parts().0)))
                }
                Next::Rows(collector) => {
                    let mut rows = collector.rows.into_inner();
                    Row::share_schemas(&mut rows);
                    return Ok(ChainOutput::Rows {
                        rows,
                        source: collector.source,
                    });
                }
            }
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
pub(crate) enum MatchConsumer<'a> {
    Fold(MatchFold<'a>),
    Chain(MatchChain<'a>),
}

/// What a consumer leaves for the clause after the clauses it covered.
pub(crate) enum Landing<'a> {
    /// The rows went into an aggregation. `offered` counts the rows a
    /// directly fed fold took (the clause's PROFILE row count); a run's
    /// fold does not report it.
    Folded {
        aggregation: Box<Aggregation<'a>>,
        offered: Option<u64>,
        /// The clauses after the head the consumer covered.
        consumed: usize,
    },
    /// The last stage's retained output.
    Rows {
        rows: Vec<Row>,
        source: RowSource,
        consumed: usize,
    },
}

/// The consumer for the uncapped, non-`OPTIONAL` `MATCH` at `clauses[0]`,
/// or `None` when its rows are collected. An aggregating clause right after
/// takes them directly; otherwise the stages after it do, unless
/// `streaming_stages` is off (PROFILE attributes per-clause facts to
/// clauses run one after the other, so it keeps the held path).
pub(crate) fn open_match_consumer<'a>(
    ctx: ExecCtx<'a>,
    clauses: &'a [Clause],
    class_slot: &'a OnceCell<ProjectionClass<'a>>,
    scope: Option<&'a Row>,
    streaming_stages: bool,
) -> Result<Option<MatchConsumer<'a>>> {
    let ExecCtx { graph, params, .. } = ctx;
    if let Some(fold) = open_match_fold(graph, clauses.get(1), class_slot, params, scope)? {
        return Ok(Some(MatchConsumer::Fold(fold)));
    }
    if !streaming_stages {
        return Ok(None);
    }
    Ok(open_match_chain(ctx, clauses, class_slot, scope)?.map(MatchConsumer::Chain))
}

impl<'a> MatchConsumer<'a> {
    /// The push contract the head clause's walk offers its rows to.
    pub(crate) fn as_fold(&self) -> &dyn RowFold {
        match self {
            MatchConsumer::Fold(fold) => fold,
            MatchConsumer::Chain(chain) => chain,
        }
    }

    /// Close the consumer once the head clause has emitted everything.
    pub(crate) fn finish(self, graph: &ExecGraph<'_>) -> Result<Landing<'a>> {
        match self {
            MatchConsumer::Fold(fold) => {
                let (aggregation, offered) = fold.into_parts();
                Ok(Landing::Folded {
                    aggregation: Box::new(aggregation),
                    offered: Some(offered),
                    consumed: 0,
                })
            }
            MatchConsumer::Chain(chain) => {
                let consumed = chain.stages;
                Ok(match chain.finish(graph)? {
                    ChainOutput::Folded(aggregation) => Landing::Folded {
                        aggregation,
                        offered: None,
                        consumed,
                    },
                    ChainOutput::Rows { rows, source } => Landing::Rows {
                        rows,
                        source,
                        consumed,
                    },
                })
            }
        }
    }
}
