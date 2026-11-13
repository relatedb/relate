//! The `MATCH` clause arm of the clause dispatcher: the `SEARCH` path, the
//! folded-`MATCH` fast paths, and the operator ladder with its consumers.
//! `dispatch.rs` owns the loop; this owns what one `MATCH` clause does and
//! hands the loop back one [`MatchArm`] for its loop-carried state.

use std::cell::OnceCell;

use relate_core::Result;
use relate_ir::Clause;

use crate::ctx::ExecCtx;
use crate::row::Row;

use super::aggregate::Aggregation;
use super::match_chain::{open_match_consumer, Landing, MatchConsumer};
use super::match_clause::{
    charge_finished_container, run_match_operator_ladder, LadderFacts, LadderRun,
};
use super::match_folds::try_match_folds;
use super::project::{ProjectionClass, RowSource};
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
    pub(super) pending_fold: Option<Aggregation<'a>>,
    /// Whether the rows already carry the next projection's columns.
    pub(super) rows_source: RowSource,
    /// Rows a directly fed fold took (the clause's PROFILE row count).
    pub(super) streamed_rows: Option<u64>,
}

/// Run the `MATCH` at `clauses[i]`. `profiled` says a PROFILE sink is
/// attached, which keeps the stage-streaming consumers off (PROFILE
/// attributes facts to clauses run one after the other).
pub(super) fn exec_match_arm<'a>(
    ctx: ExecCtx<'a>,
    mut rows: Vec<Row>,
    clauses: &'a [Clause],
    i: usize,
    fold_class: &'a OnceCell<ProjectionClass<'a>>,
    scope: Option<&'a Row>,
    profiled: bool,
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
        pending_fold: None,
        rows_source: RowSource::Bindings,
        streamed_rows: None,
    };
    if let Some(search) = search {
        rows = search_match_clause(
            ctx,
            rows,
            optional,
            match_mode,
            patterns,
            where_clause.as_ref(),
            search,
        )?;
        // A SEARCH MATCH resolves its candidate window from
        // the vector index and then walks the pattern with the
        // per-row `match_clause` directly (`exec/search.rs`),
        // never the ladder — its rung is the walker (LABS-1117).
        out.ladder = Some(LadderFacts::walker());
        if let Some(live) = live_from(&clauses[i + 1..]) {
            trim_rows(&mut rows, &live);
        }
        // The same ADR-0463 (LABS-781) MATCH-container charge
        // as the ladder landing below, through the one bulk
        // spelling — the SEARCH arm's own ownership statement:
        // it reports the walker's rung, but its walker runs
        // are per-candidate, so the clause-level container is
        // only whole — and only chargeable — here, and this
        // arm charges it itself rather than arriving charged.
        charge_finished_container(graph, &rows)?;
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
                let mut cap = if optional {
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
                    open_match_consumer(ctx, &clauses[i..], fold_class, scope, !profiled)?
                } else {
                    None
                };
                let projection = if consumer.is_none() {
                    clauses.get(i + 1).and_then(sinkable_projection)
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
                    optional,
                    match_mode,
                    patterns,
                    where_clause.as_ref(),
                    &mut cap,
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
                    // aggregation it fed) are what is left.
                    match consumer.finish(graph)? {
                        Landing::Folded {
                            aggregation,
                            offered,
                            consumed,
                        } => {
                            out.pending_fold = Some(*aggregation);
                            out.skip = consumed;
                            out.streamed_rows = offered;
                        }
                        Landing::Rows {
                            rows: landed,
                            source,
                            consumed,
                        } => {
                            rows = landed;
                            out.rows_source = source;
                            out.skip = consumed;
                        }
                    }
                } else if projection.is_some() {
                    out.rows_source = RowSource::SinkProjected;
                } else if let Some(live) = live_from(&clauses[i + 1..]) {
                    // A sink projection already dropped the
                    // bindings the next clause does not name.
                    // Trimming here would keep the pre-projection
                    // variables and discard the output columns.
                    trim_rows(&mut rows, &live);
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
