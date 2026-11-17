//! One dataset's differential run (ADR-0144): load the dump into the
//! reference, pull the whole database into a fresh `relate::Database`
//! through the real CLI, then run every plain adoc statement on both
//! engines and compare.

use std::path::Path;

use relate::value_to_cypher;

use crate::adoc::{
    classify_block, cypher_blocks, is_ordered, strip_limit_skip, Classified, ClassifiedKind,
};
use crate::diff::{compare_rows, rows_subset, Relation, StatementOrder};
use crate::docker::DatasetTest;
use crate::manifest::Dataset;
use crate::record::{Outcome, QueryResult};
use crate::reference;
use crate::setup;

/// The reference's rows for one query: Query API v2 JSON values.
type ReferenceRows = Vec<serde_json::Value>;
/// The engine's rows for one query: one flattened list value per row.
type EngineRows = Vec<relate_model::CypherValue>;

/// Options for one dataset run (F-1-13): named fields, so the two flags
/// can never be swapped silently at a call site the way adjacent bare
/// bools can.
#[derive(Debug, Clone, Copy)]
pub struct RunOptions {
    /// Allow network downloads (dumps, adocs, docker image); the caller
    /// decides cold-cache policy.
    pub fetch: bool,
    /// The on-demand apoc flag (ADR-0144): with it, apoc statements also
    /// run; without it they are skipped with the reason.
    pub apoc_enabled: bool,
}

/// Run one dataset end-to-end. Requires the reference booted with this
/// dataset's dump already loaded (see [`crate::docker::start_with_dumps`]).
///
/// The body is the phase list; each phase is a named helper: phases 1, 3
/// and 4 (artifacts, CLI pull into a fresh directory, opening the pulled
/// copy) live in [`crate::setup`], phase 2 (database registration) is the
/// one `ensure_database` call below, and phases 5–6 (pull verification,
/// the classify/dispatch loop) below.
pub fn run_dataset(
    reference: &DatasetTest,
    dataset: &Dataset,
    workdir: &Path,
    options: RunOptions,
) -> Result<Vec<QueryResult>, String> {
    let adoc = setup::fetch_artifacts(dataset, options.fetch)?;
    // Phase 2 — register the pre-loaded store as a database (the reference
    // volume was loaded before the server booted, so there is no quarantine).
    reference.ensure_database(dataset.name)?;
    let destination = setup::pull_fresh_copy(reference, dataset, workdir)?;
    let mut conn = setup::open_pulled_copy(&destination)?;
    let mut results = verify_pull(reference, dataset, &mut conn);
    results.extend(run_statements(
        reference,
        dataset,
        &mut conn,
        &adoc,
        options.apoc_enabled,
    )?);
    // Extras run after every adoc statement: the writes among them (the
    // `apoc.periodic.iterate` rewrite) must not shift any adoc comparison.
    results.extend(
        crate::extras::for_dataset(dataset)
            .into_iter()
            .map(|extra| {
                run_both(reference, dataset, &mut conn, extra.statement).into_record(
                    dataset.name,
                    Some(extra.line),
                    Some(extra.statement.to_string()),
                    false,
                )
            }),
    );
    Ok(results)
}

/// Phase 5 — verify the pull copied the whole database: node and
/// relationship counts must match the reference (ADR-0144 ingest
/// verification).
fn verify_pull(
    reference: &DatasetTest,
    dataset: &Dataset,
    conn: &mut relate::Connection,
) -> Vec<QueryResult> {
    [
        "MATCH (n) RETURN count(n) AS c",
        "MATCH ()-[r]->() RETURN count(r) AS c",
    ]
    .into_iter()
    .map(|count_query| {
        verify_count(reference, dataset, conn, count_query).into_record(
            dataset.name,
            // No adoc line: pull verification runs no adoc statement (F-1-4).
            None,
            Some(format!("pull verification: {count_query}")),
            false,
        )
    })
    .collect()
}

/// One pull-verification count query. A count mismatch is ADR-0176
/// infrastructure failure kind 3: the pull did not copy the database, so
/// every downstream diff for the dataset would be a comparison against a
/// different graph — uninterpretable. The outcome is therefore
/// [`Outcome::SetupFailed`], the run-failing variant, never a plain diff.
fn verify_count(
    reference: &DatasetTest,
    dataset: &Dataset,
    conn: &mut relate::Connection,
    count_query: &str,
) -> TimedOutcome {
    let (reference_rows, engine_rows, engine_ms) =
        match query_both_engines(reference, dataset, conn, count_query) {
            Ok(rows) => rows,
            Err(timed) => return timed,
        };
    // One row, one integer, no ordering question: anything but Exact is a
    // count mismatch.
    let outcome = match compare_rows(&reference_rows, &engine_rows, StatementOrder::Ordered) {
        Relation::Exact => Outcome::Pass,
        Relation::UpToListOrder(why) | Relation::Different(why) => Outcome::SetupFailed(format!(
            "pull-verification count mismatch (ADR-0176 kind 3): {why}"
        )),
    };
    TimedOutcome::timed(outcome, engine_ms)
}

/// Phase 6 — run every classified statement on both engines. A dataset
/// whose adoc yields no runnable statements is a failure, not a vacuous
/// pass: a truncated or upstream-changed adoc would otherwise drop the
/// whole dataset's coverage silently. The `Err` is ADR-0176 infrastructure
/// failure kind 4; `fold_outcomes` in the suite test turns it into the
/// run-failing setup-failed outcome.
fn run_statements(
    reference: &DatasetTest,
    dataset: &Dataset,
    conn: &mut relate::Connection,
    adoc: &str,
    apoc_enabled: bool,
) -> Result<Vec<QueryResult>, String> {
    let blocks = cypher_blocks(adoc);
    let classified: Vec<Classified> = blocks
        .iter()
        .flat_map(|b| classify_block(b, apoc_enabled))
        .collect();
    let runnable = classified.iter().filter(|c| c.is_runnable()).count();
    if runnable == 0 {
        return Err(format!(
            "{}: adoc produced no runnable statements ({} blocks) — vacuous pass refused",
            dataset.name,
            blocks.len()
        ));
    }
    Ok(classified
        .into_iter()
        .map(|c| run_classified(reference, dataset, conn, c))
        .collect())
}

/// Run one classified statement and build its result record. apoc-ness was
/// decided at classification time (F-1-10): the `ClassifiedKind::Apoc` kind
/// (or a `ClassifiedKind::Skipped` with `SkipReason::Apoc`) is the marker the
/// apoc capture consumes — every apoc-classified statement is marked, whether
/// it ran under the flag or was skipped.
fn run_classified(
    reference: &DatasetTest,
    dataset: &Dataset,
    conn: &mut relate::Connection,
    classified: Classified,
) -> QueryResult {
    let apoc = classified.is_apoc();
    // `is_runnable` is the single enumeration of the runnable kind set
    // (F-8-5): it guards the vacuous-pass count in run_statements. The
    // dispatch below is one concern — run the runnable kind — and stays
    // a total match, so a new kind fails to compile here instead of
    // reaching a runtime unreachable!. A skipped statement never reaches
    // the engines; its reason is the outcome.
    let timed = match &classified.kind {
        // An apoc statement under the flag runs differentially like a
        // plain query.
        ClassifiedKind::Plain | ClassifiedKind::Apoc => {
            run_both(reference, dataset, conn, &classified.statement)
        }
        // LIMIT/SKIP without ORDER BY: compare against the reference's
        // full result set (subset check) instead of the limited rows.
        ClassifiedKind::LimitedSelection => {
            run_both_limited_selection(reference, dataset, conn, &classified.statement)
        }
        ClassifiedKind::Skipped(reason) => {
            TimedOutcome::never_ran(Outcome::Skipped(reason.clone()))
        }
    };
    // full text: the stable statement id hashes this
    let Classified {
        line, statement, ..
    } = classified;
    timed.into_record(dataset.name, Some(line), Some(statement), apoc)
}

/// The single reference-error policy (F-6-3/F-6-6/F-6-9): the walkthrough
/// was written against an older Cypher, so when the reference itself
/// refuses the statement's syntax (pattern expressions in size(),
/// placeholder text like `WIP`) the statement tests nothing — skip it with
/// the reference's reason. Any other reference error is a failure, with
/// `context` naming the query that failed. The classification reads the
/// typed Neo4j error code, never a formatted-message substring.
fn reference_error_outcome(context: &str, e: &reference::ReferenceError) -> Outcome {
    if e.code() == Some("Neo.ClientError.Statement.SyntaxError") {
        return Outcome::Skipped(crate::record::SkipReason::Dialect(
            "the reference rejects this statement's syntax (adoc predates it)",
        ));
    }
    Outcome::Fail(format!("{context}: {e}"))
}

/// The outcome for a failed reference run (ADR-0176): a timebox is
/// inconclusive — with no trustworthy expected result there is nothing to
/// diff — while an error carrying a Neo4j code goes through
/// [`reference_error_outcome`]. The one mapping every reference-query call
/// site shares (F-1-7): [`query_both_engines`] and
/// [`limited_selection_rows`] route every `reference::query` call through
/// it as `.map_err(...)`, so no hand-rolled reference-error match remains.
fn reference_run_outcome(context: &str, e: &reference::ReferenceError) -> Outcome {
    match e {
        reference::ReferenceError::TimedOut(why) => Outcome::InconclusiveTimebox(why.clone()),
        _ => reference_error_outcome(context, e),
    }
}

/// One differential run's outcome plus the engine-side query time (ms).
/// `engine_ms` is `None` exactly when the engine never ran the statement
/// (a skip, a reference error, a dataset-level setup failure); an engine
/// error or a timebox cancellation still carries its measured time — the
/// slowest statements are exactly the ones worth timing. The named pair
/// replaces the positional `(Outcome, Option<f64>)` tuple the runners
/// used to return (F-2-2's lesson one layer up).
pub struct TimedOutcome {
    outcome: Outcome,
    engine_ms: Option<f64>,
}

impl TimedOutcome {
    /// The differential record for this run: the outcome and its timing,
    /// placed on a statement of `dataset` at `line`. The one constructor
    /// every statement-bearing record-building site in this module routes
    /// through; a dataset-level setup failure goes through
    /// [`QueryResult::dataset_level`] instead (no runner state to convert).
    pub fn into_record(
        self,
        dataset: &str,
        line: Option<usize>,
        statement: Option<String>,
        apoc: bool,
    ) -> QueryResult {
        QueryResult {
            dataset: dataset.to_string(),
            line,
            statement,
            outcome: self.outcome,
            engine_ms: self.engine_ms,
            apoc,
        }
    }

    /// The engine never ran: there is no timing to carry. A dataset-level
    /// setup failure's record is built through [`QueryResult::dataset_level`]
    /// (record.rs) instead — the model owns the statement-less shape — so
    /// this constructor stays private to the runner.
    fn never_ran(outcome: Outcome) -> Self {
        Self {
            outcome,
            engine_ms: None,
        }
    }

    /// The engine ran (rows or error): carry the measured query time.
    fn timed(outcome: Outcome, engine_ms: f64) -> Self {
        Self {
            outcome,
            engine_ms: Some(engine_ms),
        }
    }
}

/// The query phase shared by [`verify_count`] and [`run_both`]: one
/// statement on both engines. The reference error arm is the shared
/// [`reference_run_outcome`] combinator (F-1-7); the engine error arm is
/// the shared `relate error` fail (F-1-10), and it keeps the measured
/// time — an engine error is a run that happened. Both row sets come back
/// together, plus the engine-side query time, or the outcome to report.
fn query_both_engines(
    reference: &DatasetTest,
    dataset: &Dataset,
    conn: &mut relate::Connection,
    statement: &str,
) -> Result<(ReferenceRows, EngineRows, f64), TimedOutcome> {
    let reference_rows = reference::query(&reference.uri(), dataset.name, statement)
        .map_err(|e| TimedOutcome::never_ran(reference_run_outcome("reference error", &e)))?;
    let (engine_ms, engine_result) = run_engine(conn, statement);
    let engine_rows = engine_result
        .map_err(|e| TimedOutcome::timed(Outcome::Fail(format!("relate error: {e}")), engine_ms))?;
    Ok((reference_rows, engine_rows, engine_ms))
}

/// The three row sets [`run_both_limited_selection`] compares (F-2-2:
/// field-named, not positional — the limited and full reference row sets
/// share one type and were swappable as adjacent tuple elements).
struct LimitedSelectionRows {
    /// The reference's rows for the limited statement.
    limited: ReferenceRows,
    /// The reference's rows for the full statement (LIMIT/SKIP stripped).
    full: ReferenceRows,
    /// The engine's rows for the limited statement.
    engine: EngineRows,
    /// The engine-side query time of the limited statement (ms) — the
    /// engine ran whenever these rows exist.
    engine_ms: f64,
}

/// The query phase of [`run_both_limited_selection`] (F-2-1): the limited
/// reference query and the engine run are exactly [`query_both_engines`]'s
/// job, so compose it; only the full-set reference query is local, with
/// its own error context.
fn limited_selection_rows(
    reference: &DatasetTest,
    dataset: &Dataset,
    conn: &mut relate::Connection,
    statement: &str,
    full_statement: &str,
) -> Result<LimitedSelectionRows, TimedOutcome> {
    let (limited, engine, engine_ms) = query_both_engines(reference, dataset, conn, statement)?;
    let full = reference::query(&reference.uri(), dataset.name, full_statement).map_err(|e| {
        TimedOutcome::never_ran(reference_run_outcome(
            &format!("reference full-set query error ({full_statement})"),
            &e,
        ))
    })?;
    Ok(LimitedSelectionRows {
        limited,
        full,
        engine,
        engine_ms,
    })
}

/// Run one statement on both engines and compare. An exact mismatch is
/// retried with list-order-insensitive comparison: `collect()` order is
/// unspecified by Cypher, so a list-order-only difference is a pass with a
/// note, not a diff. A reference timebox is inconclusive (ADR-0176), never
/// a diff; other reference errors follow [`reference_run_outcome`].
fn run_both(
    reference: &DatasetTest,
    dataset: &Dataset,
    conn: &mut relate::Connection,
    statement: &str,
) -> TimedOutcome {
    let (reference_rows, engine_rows, engine_ms) =
        match query_both_engines(reference, dataset, conn, statement) {
            Ok(rows) => rows,
            Err(timed) => return timed,
        };
    let ordered = is_ordered(statement);
    // db.schema.visualization() gets its own comparison (LABS-257): the
    // caller knows the procedure, so the extra normalization (constraint
    // repr fold, entry/cell sorting) applies exactly here and never
    // shape-sniffs data.
    let relation = if crate::adoc::is_schema_visualization(statement) {
        crate::diff::compare_rows_schema_viz(&reference_rows, &engine_rows)
    } else {
        let order = if ordered {
            StatementOrder::Ordered
        } else {
            StatementOrder::Unordered
        };
        compare_rows(&reference_rows, &engine_rows, order)
    };
    let outcome = match relation {
        Relation::Exact => Outcome::Pass,
        // ORDER BY with equal sort keys leaves the row order unspecified
        // (ties), and collect() list order is unspecified anywhere — so a
        // pure reordering passes with a note. Content drift still fails.
        Relation::UpToListOrder(strict_why) => Outcome::PassWithOrderNote(format!(
            "row or list order differs (collect() order and ORDER BY ties are unspecified): \
             {strict_why}"
        )),
        Relation::Different(strict_why) => Outcome::Fail(strict_why),
    };
    TimedOutcome::timed(outcome, engine_ms)
}

/// `LIMIT`/`SKIP` without `ORDER BY`: the engines may pick different rows.
/// Every row relate returns must exist in the reference's *full* result
/// set (same statement with the LIMIT/SKIP clause removed), and the row
/// count must match the reference's limited result — that catches missing or
/// invented data while tolerating the unspecified selection.
fn run_both_limited_selection(
    reference: &DatasetTest,
    dataset: &Dataset,
    conn: &mut relate::Connection,
    statement: &str,
) -> TimedOutcome {
    let full_statement = strip_limit_skip(statement);
    let rows = match limited_selection_rows(reference, dataset, conn, statement, &full_statement) {
        Ok(rows) => rows,
        Err(timed) => return timed,
    };
    if rows.engine.len() != rows.limited.len() {
        return TimedOutcome::timed(
            Outcome::Fail(format!(
                "row count differs: neo4j {} vs relate {} (LIMIT without ORDER BY)",
                rows.limited.len(),
                rows.engine.len()
            )),
            rows.engine_ms,
        );
    }
    let outcome = match rows_subset(&rows.full, &rows.engine) {
        Ok(()) => Outcome::PassWithOrderNote(format!(
            "LIMIT/SKIP without ORDER BY: {} rows, all present in the reference's full result set",
            rows.engine.len()
        )),
        Err(why) => Outcome::Fail(why),
    };
    TimedOutcome::timed(outcome, rows.engine_ms)
}

/// Run one statement on the pulled relate copy, one flattened row value
/// per result row, under the per-query timebox (ADR-0144): a watcher thread
/// cancels the query's [`relate::CancelToken`] past the budget, and a
/// cancellation that the watcher fired is reported as the timebox, not as
/// an engine error. The connection stays usable after a cancellation.
///
/// Returns the engine-side wall time of the query in milliseconds plus the
/// rows — the time travels *outside* the `Result` so a failed run (an
/// engine error, a timebox cancellation) still carries its measurement:
/// the slowest statements are exactly the ones worth timing. This is the
/// one place a query is timed, so every differential record
/// (`QueryResult::engine_ms`) carries the same measurement.
fn run_engine(conn: &mut relate::Connection, statement: &str) -> (f64, Result<EngineRows, String>) {
    let budget = crate::timebox::query_timeout();
    let token = relate::CancelToken::new();
    // Probe clone: after the query returns, only a token the watcher actually
    // cancelled (the query returned the "query cancelled" error) is a timebox —
    // a genuine engine error that happens to land as the budget elapses keeps
    // its real message. The token is caller-driven and the watcher is the sole
    // code holding a clone that can cancel, so `is_cancelled()` alone is the
    // truth (F-1-12).
    let probe = token.clone();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let watcher = {
        let token = token.clone();
        std::thread::spawn(move || {
            if done_rx.recv_timeout(budget).is_err() {
                token.cancel();
            }
        })
    };
    let started = std::time::Instant::now();
    let result = conn.query_with_params_cancellable(statement, Default::default(), token);
    let engine_ms = started.elapsed().as_secs_f64() * 1000.0;
    let _ = done_tx.send(());
    let result = result.map_err(|e| {
        if probe.is_cancelled() {
            format!("relate query exceeded the {}s timebox", budget.as_secs())
        } else {
            e.to_string()
        }
    });
    let _ = watcher.join();
    let rows = match result {
        Ok(result) => result
            .rows
            .iter()
            .map(|cells| {
                relate_model::CypherValue::List(cells.iter().map(value_to_cypher).collect())
            })
            .collect(),
        Err(e) => return (engine_ms, Err(e)),
    };
    (engine_ms, Ok(rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::SkipReason;
    use crate::reference::ReferenceError;

    /// F-6-3/F-6-6/F-6-9: the reference-error policy classifies on the
    /// typed Neo4j error code parsed from the Query API v2 error body —
    /// a syntax error skips, any other code fails.
    #[test]
    fn reference_error_policy_maps_syntax_error_to_skip_and_others_to_fail() {
        // Representative Query API v2 error bodies, as the reference
        // returns them on a non-2xx reply.
        let syntax_body = r#"{"errors":[{"code":"Neo.ClientError.Statement.SyntaxError","message":"Invalid input 'WIP': expected an expression (line 1, column 8 (offset: 7))"}]}"#;
        let not_found_body = r#"{"errors":[{"code":"Neo.ClientError.Database.DatabaseNotFound","message":"Graph not found: movies"}]}"#;
        let syntax = ReferenceError::from(relate_load::LoadError::Http {
            status: 400,
            reply: syntax_body.to_string(),
        });
        match reference_error_outcome("reference error", &syntax) {
            Outcome::Skipped(SkipReason::Dialect(why)) => {
                assert!(why.contains("syntax"), "{why}")
            }
            other => panic!("syntax error must skip: {other:?}"),
        }
        let not_found = ReferenceError::from(relate_load::LoadError::Http {
            status: 404,
            reply: not_found_body.to_string(),
        });
        match reference_error_outcome("reference error", &not_found) {
            Outcome::Fail(why) => {
                assert!(why.contains("reference error"), "{why}");
                assert!(why.contains("DatabaseNotFound"), "{why}");
            }
            other => panic!("non-syntax reference error must fail: {other:?}"),
        }
        // No Neo4j error object at all (transport): also a failure.
        let transport = ReferenceError::Other("reference query failed: connection refused".into());
        assert!(
            matches!(
                reference_error_outcome("reference error", &transport),
                Outcome::Fail(_)
            ),
            "transport error must fail"
        );
    }

    /// ADR-0176: a reference timebox is inconclusive, never a diff. Every
    /// reference-query call site maps through `reference_run_outcome`, so
    /// pinning the mapping with each call site's context pins it for
    /// `run_both` and for both queries of `run_both_limited_selection`.
    #[test]
    fn reference_timebox_maps_to_inconclusive_at_every_call_site() {
        let timed_out =
            || ReferenceError::TimedOut("reference query exceeded the 20s timebox".to_string());
        // run_both's (and run_both_limited_selection's limited query's)
        // context:
        match reference_run_outcome("reference error", &timed_out()) {
            Outcome::InconclusiveTimebox(why) => assert!(why.contains("timebox"), "{why}"),
            other => panic!("a reference timebox must be inconclusive: {other:?}"),
        }
        // run_both_limited_selection's full-set query context:
        match reference_run_outcome(
            "reference full-set query error (MATCH (n) RETURN n)",
            &timed_out(),
        ) {
            Outcome::InconclusiveTimebox(why) => assert!(why.contains("timebox"), "{why}"),
            other => panic!("a full-set reference timebox must be inconclusive: {other:?}"),
        }
        // verify_count's context:
        assert!(
            matches!(
                reference_run_outcome("reference error", &timed_out()),
                Outcome::InconclusiveTimebox(_)
            ),
            "a pull-verification reference timebox must be inconclusive"
        );
    }

    /// Errors carrying a Neo4j code still go through the reference-error
    /// policy — the timebox mapping never swallows a typed code.
    #[test]
    fn coded_reference_errors_bypass_the_timebox_mapping() {
        let syntax_body = r#"{"errors":[{"code":"Neo.ClientError.Statement.SyntaxError","message":"Invalid input 'WIP'"}]}"#;
        let syntax = ReferenceError::from(relate_load::LoadError::Http {
            status: 400,
            reply: syntax_body.to_string(),
        });
        assert!(
            matches!(
                reference_run_outcome("reference error", &syntax),
                Outcome::Skipped(SkipReason::Dialect(_))
            ),
            "a coded syntax error must still skip"
        );
    }
}
