//! The run-and-render seam of one statement, split out of `session` to keep
//! that module on the session's own concerns: execute through the open
//! connection, render the result through the session's formatter, and report
//! everything the run itself has to say — the row-cap warning, a timeout, a
//! query error, a render error and the `.timer` line — to the writers the
//! caller supplies.
//!
//! The writer seam is a testability boundary, not a policy: production binds
//! `exec_relate_query` to stdout and stderr, while the render-failure
//! test supplies a closed writer (the only way the formatter's `Err` arm is
//! reached after a statement executed) and captures every report instead of
//! leaking it to the process streams. What surrounds a run — the `Exec`
//! bundle, the dot-command dispatch, the ADR-0164 exit policy and the
//! ADR-0402 ledger the session wires — stays in `session`.

use std::io;

use indexmap::IndexMap;
use relate::StatementEffect;
use relate_formatter::{Formatter, PlanPresentation, StatementMeta};
use relate_model::CypherValue;

use crate::{errors, row_cap, write_error, write_report};

/// The per-statement render/execution controls of [`exec_relate_query`]
/// (keeps it under the clippy argument limit). Every field defaults to the
/// unrestricted policy, so a `.info` catalog readout is `RenderOpts::default()`.
#[derive(Default)]
pub(crate) struct RenderOpts {
    /// The row cap (ADR-0196); `None` is unlimited.
    pub(crate) max_rows: Option<usize>,
    /// The per-statement timeout (ADR-0241); `None` is no bound.
    pub(crate) timeout: Option<std::time::Duration>,
    /// Batch position for the error envelope (ADR-0199).
    pub(crate) progress: Option<errors::Progress>,
    /// The session's `.timer` flag.
    pub(crate) timer: bool,
}

/// One executed statement's exit code plus the two facts the ADR-0402 note
/// policy reads: the engine effect the run carries, and whether the result
/// carries a plan artifact. The two are separate because a plan-producing run
/// keeps its effect — `PROFILE` executes, so its write joins the transaction
/// ledger — while only the plan artifact takes the run out of the
/// per-statement note (Decision 2). A failed statement carries the read
/// effect and no plan bit, so the policy never mistakes "no execution" for
/// "changed nothing" — with one exception it must not lose: a statement that
/// executed and then failed to render keeps its real effect, because the
/// change is already applied (or staged) even though the exit code is 1.
///
/// All three fields are crate-visible: the session's note policy destructures
/// the bundle in one place, and `.info` — the one other caller — reads only
/// `code`.
pub(crate) struct StatementRun {
    pub(crate) code: i32,
    pub(crate) effect: StatementEffect,
    pub(crate) plan_produced: bool,
}

impl StatementRun {
    /// A run that executed nothing: the failure already reported, so there
    /// are no effect facts to carry.
    fn failed(code: i32) -> Self {
        Self {
            code,
            effect: StatementEffect::None,
            plan_produced: false,
        }
    }
}

/// The `.timer` line's rendering: the engine-reported wall-clock total
/// (ADR-0221) beside its PARSE / PLAN / EXECUTE partition (ADR-0491 §2),
/// every number one-decimal milliseconds — the same unit the total has
/// always reported, now with the split that answers "where did the time
/// go". One renderer, so the format cannot drift between the emission and
/// the tests that pin it.
fn timer_line(elapsed_ms: f64, parse_ms: f64, plan_ms: f64, execute_ms: f64) -> String {
    format!(
        "Time: {elapsed_ms:.1}ms (parse {parse_ms:.1} / plan {plan_ms:.1} / execute {execute_ms:.1})"
    )
}

/// Run one statement and render it on the process streams.
pub(crate) fn exec_relate_query(
    conn: &mut relate::Connection,
    query: &str,
    params: &std::collections::BTreeMap<String, relate::Value>,
    formatter: &Formatter,
    opts: RenderOpts,
) -> StatementRun {
    let run = exec_relate_query_to(
        conn,
        query,
        params,
        formatter,
        opts,
        &mut io::stdout(),
        &mut io::stderr(),
    );
    // LABS-1736: the statement path's process-stderr binding carries the
    // agent feedback pointer — a failed statement is the diagnostic the
    // pointer exists for. The injected-writer form stays hint-free, so a
    // test's captured sinks are clean.
    if run.code != 0 {
        crate::agent_feedback_cmd::write_hint(&mut io::stderr());
    }
    run
}

/// [`exec_relate_query`] with its output and diagnostic writers passed
/// in: production binds the process streams, and the render-failure test
/// supplies a closed one — the only way the formatter's `Err` arm is reached,
/// and so the only way to prove that a failure there keeps the execution's
/// effect. Both writers receive every report the run makes: the cap warning
/// and every error on `diagnostics`, the rendered result, the error envelope
/// and the `.timer` line on `out`.
fn exec_relate_query_to(
    conn: &mut relate::Connection,
    query: &str,
    params: &std::collections::BTreeMap<String, relate::Value>,
    formatter: &Formatter,
    opts: RenderOpts,
    out: &mut dyn io::Write,
    diagnostics: &mut dyn io::Write,
) -> StatementRun {
    let RenderOpts {
        max_rows,
        timeout,
        progress,
        timer,
    } = opts;
    // ADR-0241, ADR-0692: the statement deadline is the engine's — the
    // connection carries the budget and answers `Error::TimedOut`, the
    // structural classification (no token-state guessing here).
    conn.set_statement_timeout(timeout.filter(|limit| !limit.is_zero()));
    let result = conn.query_with_params(query, params.clone());
    if let (Err(relate::Error::TimedOut { .. }), Some(limit)) = (&result, timeout) {
        return StatementRun::failed(errors::report_timeout_to(
            &formatter.mode,
            limit,
            progress,
            out,
            diagnostics,
        ));
    }
    match result {
        Ok(r) => {
            // The cap is decided from the row count, before any row is
            // converted, so it bounds the conversion work too (ADR-0195).
            // The cap decision, one derivation (ADR-0196/0403): the rows to
            // render and the cut's record.
            let (kept, cut) = row_cap::truncation(r.rows.len(), max_rows);
            if let Some(cut) = &cut {
                write_error(diagnostics, row_cap::truncation_warning(*cut, kept));
            }
            let rows: Vec<IndexMap<String, CypherValue>> = r
                .rows
                .iter()
                .take(kept)
                .map(|vals| {
                    // The engine returns rectangular rows — one value per
                    // column, in column order — so a short row is a contract
                    // break, not a column to skip. Zip pairs them, and the
                    // assert keeps the contract loud.
                    assert!(
                        vals.len() == r.header.columns.len(),
                        "engine rows are rectangular: one value per column"
                    );
                    r.header
                        .columns
                        .iter()
                        .zip(vals.iter())
                        .map(|(col, v)| (col.clone(), relate::value_to_cypher(v)))
                        .collect()
                })
                .collect();
            // The plan presentation (ADR-0345, ADR-0357): built once as one
            // owned bundle. On the HUMAN table mode the plan goes to the
            // diagnostic writer (LABS-373, Oskar's suggestion): the output
            // stream stays the result — the data table the mode promises,
            // pipeable to `jq` or a file — while the plan table is a
            // diagnostic the terminal shows. The machine modes carry the plan
            // inside their own payload (json's `{"data":…,"plan":…}`), where
            // an agent reads it structurally. The view owns the
            // engine-derived notification list too (ADR-0363): the engine
            // derives it with the tree, so a plain run's absent plan is also
            // the absent list. The formatter is the one table-plan routing
            // owner: `format_with` sends the table mode's plan table (and the
            // notification block that follows it) to the diagnostic writer,
            // while every other mode ignores that writer and keeps its
            // payload on the output stream.
            let mut plan_refused = false;
            let presentation = match r
                .plan_root()
                .map(|tree| PlanPresentation::of(tree, r.notifications()))
            {
                Some(Ok(presentation)) => Some(presentation),
                // ADR-0628's plan-wire budget: the statement executed; only
                // its plan document is refused — the typed error goes to the
                // diagnostic writer and the data rows still render, the
                // effect stays, and the run exits 1: for an EXPLAIN the plan
                // IS the payload, so a script reading the exit status can
                // tell a refused plan from a rendered one (the
                // render-failure arm's contract below).
                Some(Err(e)) => {
                    write_error(diagnostics, e);
                    plan_refused = true;
                    None
                }
                // A plain run carries no plan (and no presentation).
                None => None,
            };
            // One read of the plan fact for both `StatementRun` arms below.
            let plan_produced = presentation.is_some();
            let plan = presentation
                .as_ref()
                .map(|presentation| presentation.as_view());
            if let Err(e) = formatter.format_with(
                out,
                diagnostics,
                &r.header.columns,
                &rows,
                StatementMeta {
                    column_types: &r.header.column_types,
                    row_cap: cut,
                    plan,
                    // ADR-0581: the stamp always rides — the jsonl
                    // terminal line's opt-in is the formatter's own
                    // run-level `render_timings` setting (set once per
                    // run from the flag), never a per-statement fact;
                    // every other mode's rider and the always-on
                    // measurement (ADR-0540 decision 2) are unchanged.
                    timings: Some(relate::timing_stamp_ms(&r)),
                },
            ) {
                write_error(diagnostics, e);
                // The statement executed and its change is already in the
                // transaction (or the autocommit store): the effect is the
                // execution's fact, not the render's. Dropping it here would
                // let a later `.commit` report "changed nothing" over a write
                // it persists (ADR-0402 Decision 4).
                return StatementRun {
                    code: 1,
                    effect: r.effect,
                    plan_produced,
                };
            }

            if timer {
                write_report(
                    out,
                    timer_line(r.elapsed_ms(), r.parse_ms(), r.plan_ms(), r.execute_ms()),
                );
            }
            // The engine's ADR-0402 write effect — a mutation operator
            // reached execution, a change was applied — carried on every
            // successful run, plus whether the result carries a plan
            // artifact. The plan fact suppresses only this statement's note
            // (Decision 2): `EXPLAIN` never executed, and `PROFILE` executed
            // and keeps its effect for the transaction ledger while its plan
            // table is the run's own report.
            StatementRun {
                // Exit 1 when the render failed OR the plan was refused
                // (ADR-0628: for an EXPLAIN the plan IS the payload) — the
                // effect is the execution's fact either way and still
                // rides below.
                code: if plan_refused { 1 } else { 0 },
                effect: r.effect,
                plan_produced,
            }
        }
        // ADR-0230: strictly a fallback after the failed parse — the
        // suggestion line renders only for a syntax error whose first word
        // is a near-miss of a command; the envelope is unchanged.
        Err(e) => StatementRun::failed(errors::report_query_error_to(
            "",
            &e,
            &formatter.mode,
            errors::ReportCtx {
                progress,
                suggestion: crate::suggest::command_suggestion_line(&e, query),
            },
            out,
            diagnostics,
        )),
    }
}

#[cfg(test)]
mod render_failure_tests {
    use super::*;
    use relate_formatter::{Mode, RowCap};
    use std::io::Write;

    /// A writer whose every write fails: a closed stdout, the one way the
    /// formatter's `Err` arm is reached after a statement executed.
    struct ClosedStdout;

    impl Write for ClosedStdout {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_render_failure_after_execution_keeps_the_applied_effect() {
        let db = relate::Database::open(":memory:").unwrap();
        let mut conn = db.connect_typed(relate::Namespace::Neo4j).unwrap();
        let formatter = Formatter::new(Mode::Jsonl);
        let mut out = ClosedStdout;
        let mut diagnostics = Vec::new();
        let run = exec_relate_query_to(
            &mut conn,
            "CREATE (n:Kept) RETURN n",
            &std::collections::BTreeMap::new(),
            &formatter,
            RenderOpts::default(),
            &mut out,
            &mut diagnostics,
        );

        assert_eq!(run.code, 1, "the render failure is still reported as one");
        assert_eq!(
            run.effect,
            StatementEffect::Applied,
            "the executed write's effect survives the render failure"
        );
        assert!(!run.plan_produced, "a plain run carries no plan artifact");

        // The render error itself went to the supplied diagnostic writer, not
        // to the process stream.
        let diagnostics = String::from_utf8(diagnostics).expect("diagnostics are text");
        assert!(
            diagnostics.contains("broken pipe"),
            "the render error is captured by the caller's sink: {diagnostics:?}"
        );

        // The write really persisted: a read sees it in the same process.
        let after = conn.query("MATCH (n:Kept) RETURN count(n)").unwrap();
        assert_eq!(after.rows[0][0], relate::Value::Int(1));

        // The session's note policy folds the failed run in without a note:
        // the run reports its failure, and the applied effect it kept
        // silences a later `.commit` summary (ADR-0402 Decision 4).
        let mut ledger = StatementEffect::None;
        assert_eq!(
            crate::zero_effect::note_run(
                &mut ledger,
                crate::telemetry::QuerySource::Oneshot,
                &run,
                None,
            ),
            None,
            "a failed run reports its failure, not a zero-effect note"
        );
        assert_eq!(
            ledger,
            StatementEffect::Applied,
            "the effect the render never showed still reaches the ledger"
        );
        assert!(
            !crate::zero_effect::reports_zero_effect_write(
                crate::telemetry::QuerySource::Oneshot,
                ledger
            ),
            "a commit over the persisted write never says it changed nothing"
        );
    }

    /// ADR-0628's plan-wire budget: a plan past it is refused loudly — the
    /// typed error to the diagnostic sink, the data rows still rendered,
    /// the effect kept — and the run exits 1: for an EXPLAIN the plan IS
    /// the payload, so a script reading the exit status can tell a refused
    /// plan from a rendered one (this module's render-failure contract).
    #[test]
    fn a_refused_plan_document_exits_one_with_the_effect_kept() {
        let db = relate::Database::open(":memory:").unwrap();
        let mut conn = db.connect_typed(relate::Namespace::Neo4j).unwrap();
        let formatter = Formatter::new(Mode::Jsonl);
        let mut out = Vec::new();
        let mut diagnostics = Vec::new();
        // A chain past the plan-wire budget (~4 container levels per chain
        // node — the shared deep-plan fixture's builder).
        let nodes = relate::MAX_PLAN_WIRE_DEPTH as usize / 4 + 50;
        let statement = format!(
            "EXPLAIN {}",
            relate_test_support::deep_plan::create_chain(nodes)
        );
        let run = exec_relate_query_to(
            &mut conn,
            &statement,
            &std::collections::BTreeMap::new(),
            &formatter,
            RenderOpts::default(),
            &mut out,
            &mut diagnostics,
        );

        assert_eq!(
            run.code, 1,
            "the refused plan is a failed run for a script reading the exit status"
        );
        assert!(!run.plan_produced, "no plan artifact rode the run");
        assert_eq!(
            run.effect,
            StatementEffect::None,
            "an EXPLAIN never executed: its effect is None either way"
        );
        let diagnostics = String::from_utf8(diagnostics).expect("diagnostics are text");
        assert!(
            diagnostics.contains("plan-wire budget")
                && diagnostics.contains("only its plan document is refused"),
            "the typed refusal's scope text reaches the caller's sink: {diagnostics:?}"
        );
    }

    /// One run against the supplied writers: the statement's result, what it
    /// wrote to `out`, and what it wrote to the diagnostic writer.
    fn run_with_out(
        conn: &mut relate::Connection,
        statement: &str,
        opts: RenderOpts,
        out: &mut dyn Write,
    ) -> (StatementRun, String) {
        let formatter = Formatter::new(Mode::Jsonl);
        let mut diagnostics = Vec::new();
        let run = exec_relate_query_to(
            conn,
            statement,
            &std::collections::BTreeMap::new(),
            &formatter,
            opts,
            out,
            &mut diagnostics,
        );
        let diagnostics = String::from_utf8(diagnostics).expect("diagnostics are text");
        (run, diagnostics)
    }

    /// [`run_with_out`] over a captured output writer: `(run, out, diagnostics)`.
    fn run_captured(
        conn: &mut relate::Connection,
        statement: &str,
        opts: RenderOpts,
    ) -> (StatementRun, String, String) {
        let mut out = Vec::new();
        let (run, diagnostics) = run_with_out(conn, statement, opts, &mut out);
        let out = String::from_utf8(out).expect("output is text");
        (run, out, diagnostics)
    }

    /// The ADR-0194 envelope a run wrote to its output writer.
    fn envelope_of(out: &str) -> serde_json::Value {
        serde_json::from_str(out.trim()).expect("the envelope is JSON")
    }

    /// Every report on the run and render path goes to the writer the caller
    /// supplies, so the test captures them all instead of leaking them to the
    /// process streams: the cap warning and the render error, the timeout line
    /// with its envelope, a query error's human line with its envelope, and
    /// the `.timer` line.
    #[test]
    fn every_run_report_reaches_the_supplied_writers() {
        let db = relate::Database::open(":memory:").unwrap();
        let mut conn = db.connect_typed(relate::Namespace::Neo4j).unwrap();

        // A capped statement whose output writer is closed: the cap warning
        // and the render error both land on the diagnostic writer, and the
        // applied effect survives the failure.
        let (run, diagnostics) = run_with_out(
            &mut conn,
            "UNWIND [1, 2] AS x CREATE (:Capped) RETURN x",
            RenderOpts {
                max_rows: Some(1),
                ..RenderOpts::default()
            },
            &mut ClosedStdout,
        );
        assert_eq!(run.code, 1, "the render failure is the run's code");
        assert_eq!(run.effect, StatementEffect::Applied);
        let lines: Vec<&str> = diagnostics.lines().collect();
        assert_eq!(
            lines.first().copied(),
            Some(row_cap::truncation_warning(RowCap { total: 2 }, 1).as_str()),
            "the cap warning goes to the diagnostic writer: {diagnostics:?}"
        );
        assert_eq!(
            lines.len(),
            2,
            "the render error follows the warning on the same writer: {diagnostics:?}"
        );

        // A query error: the human line on the diagnostic writer, the ADR-0194
        // envelope on the output writer.
        let (run, out, diagnostics) = run_captured(&mut conn, "RETRN 1", RenderOpts::default());
        assert_eq!(run.code, 6, "a rejected statement keeps its exit class");
        assert!(
            diagnostics.contains("syntax error"),
            "the human line is captured: {diagnostics:?}"
        );
        assert_eq!(
            envelope_of(&out)["error"]["code"],
            "validation_error",
            "the envelope is captured on the output writer: {out:?}"
        );

        // A timeout: the ADR-0241 line on the diagnostic writer and its
        // envelope on the output writer. The cartesian product hits a
        // cooperative cancel point constantly, so a 5ms bound is decisive.
        let (run, out, diagnostics) = run_captured(
            &mut conn,
            "UNWIND range(1, 400000) AS a UNWIND range(1, 400000) AS b RETURN count(*)",
            RenderOpts {
                timeout: Some(std::time::Duration::from_millis(5)),
                ..RenderOpts::default()
            },
        );
        assert_eq!(run.code, 7, "a timed-out statement exits 7");
        assert!(
            diagnostics.contains("query timed out after"),
            "the timeout line is captured: {diagnostics:?}"
        );
        assert_eq!(envelope_of(&out)["error"]["code"], "timeout", "{out:?}");

        // The timer line: a successful run with `.timer` on writes it to the
        // output writer, and the silent diagnostic writer stays empty.
        let (run, out, diagnostics) = run_captured(
            &mut conn,
            "RETURN 1 AS n",
            RenderOpts {
                timer: true,
                ..RenderOpts::default()
            },
        );
        assert_eq!(run.code, 0, "the timed run succeeded");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines.first().copied(),
            Some("{\"n\":1}"),
            "the result row goes to the output writer: {out:?}"
        );
        // LABS-1120: the machine stream closes with the timing terminal
        // line — the formatter's library-default `timings` (ADR-0581) —
        // before the human `.timer` line.
        assert!(
            lines
                .get(1)
                .is_some_and(|line| line.contains("\"timings\"") && line.contains("\"planCache\"")),
            "the timing block closes the machine stream on the same writer: {out:?}"
        );
        assert!(
            lines
                .get(2)
                .is_some_and(|line| line.starts_with("Time: ") && line.ends_with(")")),
            "the timer line follows it on the same writer: {out:?}"
        );
        assert_eq!(lines.len(), 3, "the single statement printed three lines");
        assert!(
            diagnostics.is_empty(),
            "a clean run reports nothing on the diagnostic writer"
        );
    }

    /// The `.timer` line's format (ADR-0491 §2), pinned deterministically:
    /// the ADR-0221 total beside its PARSE / PLAN / EXECUTE split, every
    /// number one-decimal milliseconds. The wall-clock values are
    /// unpinnable — the rendering is the contract, so it is pinned on the
    /// one renderer with the ADR's own example numbers.
    #[test]
    fn the_timer_line_renders_the_adr_0491_phase_split() {
        assert_eq!(
            timer_line(12.34, 0.412, 1.14, 10.789),
            "Time: 12.3ms (parse 0.4 / plan 1.1 / execute 10.8)"
        );
    }
}
