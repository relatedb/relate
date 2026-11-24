//! The `MATCH` clause arm of the clause dispatcher: the `SEARCH` path, the
//! folded-`MATCH` fast paths, and the operator ladder with its consumers.
//! `dispatch.rs` owns the loop; this owns what one `MATCH` clause does and
//! hands the loop back one [`MatchArm`] for its loop-carried state.

use std::cell::OnceCell;

use relate_core::Result;
use relate_ir::Clause;

use crate::ctx::ExecCtx;
use crate::metrics::PlanMetricsSite;
use crate::row::Row;

use super::aggregate::Aggregation;
use super::classification::ProjectionClass;
use super::match_chain::{open_match_consumer, Landing, LandingKind, MatchConsumer};
use super::match_clause::{
    charge_finished_container, run_match_operator_ladder, LadderFacts, LadderRun, MatchClauseInputs,
};
use super::match_folds::try_match_folds;
use super::profile_tick::StreamedClause;
use super::project::RowSource;
use super::pushdown::{live_from, match_limit_cap, trim_rows};
use super::search::search_match_clause;
use super::sink::sinkable_projection;
use super::QueryResult;

/// What one `MATCH` clause leaves the dispatch loop: its output rows and the
/// loop-carried facts the clause decided.
pub(super) struct MatchArm<'a> {
    pub(super) rows: Vec<Row>,
    /// The part's result when a fold answered the `MATCH` outright.
    pub(super) last_result: Option<QueryResult>,
    /// Extra clauses past this one the arm consumed (0 for the ordinary
    /// one-clause advance).
    pub(super) skip: usize,
    /// The operator ladder's facts (LABS-1117, LABS-1437); `None` when no
    /// `MATCH` operator ran (a fold answered it).
    pub(super) ladder: Option<LadderFacts>,
    /// The aggregation a streamed `MATCH` fed, waiting for its consumer.
    pub(super) folded: Option<Aggregation<'a>>,
    /// Whether the rows already carry the next projection's columns.
    pub(super) rows_source: RowSource,
    /// The whole fact a streamed consumer took as rows were emitted — the
    /// rows that flowed through and the bytes this clause is accountable
    /// for holding — the fact the dispatcher's tick reads when its own
    /// output rows hold only what the run left behind.
    pub(super) streamed: Option<StreamedClause>,
}

/// Run the `MATCH` at `clauses[i]`. A consumer that can take the rows as
/// they are emitted (the following aggregation, or the run of stages
/// `match_chain.rs`) streams them — PROFILE included: `profile`, the
/// statement's PROFILE site when one is attached, rides the run so every
/// consumed stage records its own facts, and the tick observes the head
/// clause through [`MatchArm::streamed`] instead of the clause holding
/// its output for attribution.
// The two classification slots are distinct cells (a sink must never claim
// the fold's), so the arm takes both beside its clause/scope arguments.
#[allow(clippy::too_many_arguments)]
pub(super) fn exec_match_arm<'a>(
    ctx: ExecCtx<'a>,
    mut rows: Vec<Row>,
    clauses: &'a [Clause],
    i: usize,
    fold_class: &'a OnceCell<ProjectionClass<'a>>,
    sink_class: &'a OnceCell<ProjectionClass<'a>>,
    scope: Option<&'a Row>,
    profile: Option<PlanMetricsSite<'a>>,
) -> Result<MatchArm<'a>> {
    let ExecCtx { graph, params, .. } = ctx;
    let Clause::Match {
        optional,
        match_mode,
        patterns,
        where_clause,
        search,
        using_index: _,
        using_join: _,
    } = &clauses[i]
    else {
        unreachable!("exec_match_arm is dispatched on a MATCH clause")
    };
    let (optional, match_mode) = (*optional, *match_mode);
    let mut out = MatchArm {
        rows: Vec::new(),
        last_result: None,
        skip: 0,
        ladder: None,
        folded: None,
        rows_source: RowSource::Bindings,
        streamed: None,
    };
    if let Some(search) = search {
        // The SEARCH walk takes the same consumer as the ladder's
        // `MATCH`: a following aggregation (or a run of stages) folds
        // each matched row as it is produced, so the clause's whole
        // output is never held (ADR-0555, ADR-0660's `expand`
        // disposition). An `OPTIONAL` SEARCH keeps the held path: its
        // null row is the input row, which the consumer's landing does
        // not carry.
        let consumer = if optional {
            None
        } else {
            open_match_consumer(
                ctx,
                &clauses[i..],
                fold_class,
                sink_class,
                scope,
                profile.map(|site| (site, i)),
            )?
        };
        rows = search_match_clause(
            ctx,
            rows,
            MatchClauseInputs {
                optional,
                match_mode,
                patterns,
                where_clause: where_clause.as_ref(),
            },
            search,
            consumer.as_ref().map(MatchConsumer::as_fold),
        )?;
        // A SEARCH MATCH resolves its candidate window from
        // the vector index and then walks the pattern with the
        // per-row `match_clause` directly (`exec/search.rs`),
        // never the ladder — its rung is the walker (LABS-1117).
        out.ladder = Some(LadderFacts::walker());
        if let Some(consumer) = consumer {
            let (
                observed,
                Landing {
                    offered,
                    consumed,
                    kind,
                },
            ) = consumer.finish(graph)?;
            out.skip = consumed;
            out.streamed = Some(StreamedClause {
                rows: offered,
                downstream: observed.unwrap_or_default(),
            });
            match kind {
                LandingKind::Folded(aggregation) => out.folded = Some(*aggregation),
                LandingKind::Rows {
                    rows: landed,
                    source,
                } => {
                    rows = landed;
                    out.rows_source = source;
                }
            }
        } else {
            if let Some(live) = live_from(&clauses[i + 1..]) {
                trim_rows(graph, &mut rows, &live);
            }
            // The same ADR-0463 (LABS-781) MATCH-container charge
            // as the ladder landing below, through the one bulk
            // spelling — the SEARCH arm's own ownership statement:
            // it reports the walker's rung, but its walker runs
            // are per-candidate, so the clause-level container is
            // only whole — and only chargeable — here, and this
            // arm charges it itself rather than arriving charged.
            charge_finished_container(graph, &rows)?;
        }
    } else {
        let folded = if !optional {
            try_match_folds(graph, &rows, &clauses[i..], params)?
        } else {
            None
        };
        match folded {
            Some(folded) => {
                rows = folded.rows;
                out.last_result = Some(folded.result);
                out.skip = folded.consumed_after;
            }
            None => {
                // The MATCH's own WHERE no longer disarms the
                // plain LIMIT pull-in (LABS-853):
                // `match_limit_cap` states the WHERE policy,
                // and `match_clause` filters in the sink ahead
                // of the count.
                let cap = if optional {
                    None
                } else {
                    match_limit_cap(
                        &clauses[i + 1..],
                        patterns,
                        where_clause.as_ref(),
                        graph,
                        params,
                        rows.len(),
                    )?
                };
                // What takes the rows as they are emitted: the
                // aggregation of a following aggregating clause,
                // or the stages after this MATCH that map rows
                // to rows (ADR-0555, `match_chain.rs`).
                let consumer = if cap.is_none() && !optional {
                    open_match_consumer(
                        ctx,
                        &clauses[i..],
                        fold_class,
                        sink_class,
                        scope,
                        profile.map(|site| (site, i)),
                    )?
                } else {
                    None
                };
                let projection = if consumer.is_none() {
                    clauses
                        .get(i + 1)
                        .and_then(|next| sinkable_projection(graph, next))
                } else {
                    None
                };
                // The operator ladder — batch chain
                // (ADR-0468), contracted kernel (ADR-0473),
                // per-row walker, first taker owns the clause
                // — lives in `match_clause.rs`, one place with
                // one admission gate.
                let LadderRun {
                    rows: ladder_rows,
                    facts,
                } = run_match_operator_ladder(
                    ctx,
                    rows,
                    MatchClauseInputs {
                        optional,
                        match_mode,
                        patterns,
                        where_clause: where_clause.as_ref(),
                    },
                    cap,
                    projection.as_ref(),
                    &clauses[i + 1..],
                    consumer.as_ref().map(MatchConsumer::as_fold),
                )?;
                rows = ladder_rows;
                let streamed = consumer.is_some();
                // The ladder's one returned value; the tick
                // projects both PROFILE facts from it below.
                out.ladder = Some(facts);
                if let Some(consumer) = consumer {
                    // The consumer trimmed its own inputs and
                    // output; its last stage's rows (or the
                    // aggregation it fed) are what is left. The two
                    // facts every landing shares — the clauses it
                    // covered and the rows the head's walk offered —
                    // are set once, here; each arm then does only its
                    // own work.
                    let (
                        observed,
                        Landing {
                            offered,
                            consumed,
                            kind,
                        },
                    ) = consumer.finish(graph)?;
                    out.skip = consumed;
                    out.streamed = Some(StreamedClause {
                        rows: offered,
                        // No chain evidence — a directly fed fold — is the
                        // default delta: nothing to subtract.
                        downstream: observed.unwrap_or_default(),
                    });
                    match kind {
                        LandingKind::Folded(aggregation) => {
                            out.folded = Some(*aggregation);
                        }
                        LandingKind::Rows {
                            rows: landed,
                            source,
                        } => {
                            rows = landed;
                            out.rows_source = source;
                        }
                    }
                } else if projection.is_some() {
                    out.rows_source = RowSource::SinkProjected;
                } else if let Some(live) = live_from(&clauses[i + 1..]) {
                    // A sink projection already dropped the
                    // bindings the next clause does not name.
                    // Trimming here would keep the pre-projection
                    // variables and discard the output columns.
                    trim_rows(graph, &mut rows, &live);
                }
                // ADR-0463 (LABS-781,
                // OPLANE_REQ-00345609): the MATCH output
                // container is a retaining container. The
                // operators that charge at their own
                // emission points (the walker, the frontier
                // row rung) arrive charged; the rest hand
                // this layer a finished container, whose
                // rows charge here — the one ownership
                // predicate decides which, never an
                // exclusion list re-derived from operator
                // identity. The refusal fires at ~the cap
                // with the crossing row's honest figure.
                if !streamed && facts.operator.hands_dispatch_a_finished_container() {
                    charge_finished_container(graph, &rows)?;
                }
            }
        }
    }
    out.rows = rows;
    Ok(out)
}
