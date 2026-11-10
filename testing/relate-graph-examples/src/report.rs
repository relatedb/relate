//! All outcome-to-text rendering (ADR-0144): per-record lines, the run-level
//! report document, the apoc capture — over the record model in
//! [`crate::record`] ([`QueryResult`], [`Outcome`], [`Summary`]). The suite
//! test only orchestrates.
//!
//! Per ADR-0176 the nightly is report-only: only [`Outcome::SetupFailed`]
//! fails the run; diffs, order-only notes, skips and inconclusive
//! reference timeboxes are reported, never gated on.
//!
//! The run report is triage-ready: the summary line and a per-dataset
//! rollup lead the document, setup-failed datasets are listed first under
//! a NOT TESTED heading, and every DIFF entry carries its stable statement
//! id and the full statement text.

use crate::record::{group_by_dataset, Outcome, QueryResult, Summary};

impl QueryResult {
    /// Run-report rendering of one record: `None` for a plain pass, else the
    /// record's ORDER-ONLY / DIFF / SKIP / SETUP-FAILED / INCONCLUSIVE block
    /// (trailing newline included). The one place the run report's
    /// per-variant shapes live. Statement-bearing entries print the stable
    /// statement id and the full statement, every line indented.
    pub(crate) fn report_line(&self) -> Option<String> {
        match &self.outcome {
            Outcome::Pass => None,
            Outcome::PassWithOrderNote(note) => Some(entry("ORDER-ONLY", self, note)),
            Outcome::Fail(why) => Some(entry("DIFF", self, why)),
            Outcome::Skipped(reason) => Some(entry("SKIP", self, reason)),
            // A setup failure is dataset-level (no line, no statement): the
            // dataset name and the error text are the whole entry.
            Outcome::SetupFailed(why) => Some(format!("SETUP-FAILED {} — {why}\n", self.dataset)),
            Outcome::InconclusiveTimebox(why) => Some(entry("INCONCLUSIVE", self, why)),
        }
    }

    /// Compact one-line outcome rendering for artifacts (the apoc capture)
    /// — beside [`Self::report_line`], the outcome-to-text renderers both
    /// artifacts consume (F-1-2). A skip renders through `SkipReason`'s
    /// `Display`, the one skip-reason text (F-1-13).
    pub(crate) fn outcome_line(&self) -> String {
        match &self.outcome {
            Outcome::Pass => "pass".to_string(),
            Outcome::PassWithOrderNote(note) => format!("pass (order-only: {note})"),
            Outcome::Fail(why) => format!("fail: {why}"),
            Outcome::Skipped(reason) => format!("skipped: {reason}"),
            Outcome::SetupFailed(why) => format!("setup failed: {why}"),
            Outcome::InconclusiveTimebox(why) => format!("inconclusive: {why}"),
        }
    }
}

/// The entry skeleton the four statement-bearing report arms share (F-1-6):
/// `{tag} {id} — {payload}\n` then the full statement, every line indented
/// (trailing newline included). Each arm passes only its tag and payload;
/// the rendered bytes stay identical per arm.
fn entry(tag: &str, result: &QueryResult, payload: impl std::fmt::Display) -> String {
    format!(
        "{tag} {} — {payload}\n{}",
        result.id(),
        indent_statement(result.statement_text())
    )
}

/// Indent every statement line two spaces (trailing newline included), so
/// a multi-line statement reads as one block under its entry header.
fn indent_statement(statement: &str) -> String {
    let mut out = String::new();
    for line in statement.lines() {
        out.push_str("  ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// One row of the per-dataset rollup: the dataset's aggregate counts.
struct DatasetRow {
    name: String,
    summary: Summary,
}

impl DatasetRow {
    /// All skip reasons folded into one column.
    fn skipped(&self) -> usize {
        self.summary.skipped_gds + self.summary.skipped_apoc + self.summary.skipped_dialect
    }

    /// `FAILED` when the dataset's setup failed (it went untested), else
    /// `ok`.
    fn setup_status(&self) -> &'static str {
        if self.summary.setup_failed > 0 {
            "FAILED"
        } else {
            "ok"
        }
    }
}

/// Group a run's records by dataset into rollup rows, in first-seen (run)
/// order — one derivation ([`group_by_dataset`]), one aggregate.
fn dataset_rows(results: &[QueryResult]) -> Vec<DatasetRow> {
    group_by_dataset(results)
        .into_iter()
        .map(|(name, rows)| DatasetRow {
            name: name.to_string(),
            summary: Summary::of(rows),
        })
        .collect()
}

/// The per-dataset rollup table: one row per dataset run, in the order the
/// datasets first appear in the record list.
fn render_dataset_table(results: &[QueryResult]) -> String {
    let rows = dataset_rows(results);
    let width = rows
        .iter()
        .map(|r| r.name.len())
        .max()
        .unwrap_or(0)
        .max("dataset".len());
    let mut text = format!(
        "{:width$}  passed  order-only  failed  inconclusive  skipped  setup\n",
        "dataset",
        width = width
    );
    for row in &rows {
        text.push_str(&format!(
            "{:width$}  {:<6}  {:<10}  {:<6}  {:<12}  {:<7}  {}\n",
            row.name,
            row.summary.pass,
            row.summary.pass_order_note,
            row.summary.fail,
            row.summary.inconclusive_timebox,
            row.skipped(),
            row.setup_status(),
            width = width,
        ));
    }
    text
}

/// Render the run-level report document, triage-first: the summary line,
/// the per-dataset rollup, the NOT TESTED list (setup-failed datasets,
/// each with its error text), then one block per remaining non-pass
/// record in record order.
pub fn render_run(results: &[QueryResult], summary: &Summary) -> String {
    let mut text = format!("{}\n\n", summary.line());
    text.push_str(&render_dataset_table(results));
    let (not_tested, rest): (Vec<&QueryResult>, Vec<&QueryResult>) = results
        .iter()
        .partition(|r| matches!(r.outcome, Outcome::SetupFailed(_)));
    if !not_tested.is_empty() {
        text.push_str("\nNOT TESTED (setup failed)\n");
        for r in &not_tested {
            if let Some(line) = r.report_line() {
                text.push_str(&line);
            }
        }
    }
    for r in rest {
        if let Some(line) = r.report_line() {
            text.push_str(&line);
        }
    }
    text.push_str(&crate::timings::render_timings(results));
    text
}

/// Markdown rendering of the run summary and the per-dataset rollup,
/// appended to `$GITHUB_STEP_SUMMARY` by the suite test when GitHub
/// Actions sets it (a no-op locally, where the var is unset). The engine
/// timing section renders through [`crate::timings`], the module that owns
/// the timing statistics.
pub fn render_summary_markdown(results: &[QueryResult], summary: &Summary) -> String {
    let mut md = format!("## Graph-examples differential\n\n{}\n\n", summary.line());
    md.push_str("| dataset | passed | order-only | failed | inconclusive | skipped | setup |\n");
    md.push_str("| --- | ---: | ---: | ---: | ---: | ---: | --- |\n");
    for row in &dataset_rows(results) {
        md.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} |\n",
            row.name,
            row.summary.pass,
            row.summary.pass_order_note,
            row.summary.fail,
            row.summary.inconclusive_timebox,
            row.skipped(),
            row.setup_status(),
        ));
    }
    md.push_str(&crate::timings::summary_markdown_section(results));
    md
}

/// Render the apoc capture (ADR-0144): every apoc-classified statement and
/// its outcome. With the flag off this lists the pending statements; with
/// it on it records exactly how the engine diverges from the reference on
/// apoc — the local record the apoc branch consumes (distilled into
/// apoc-findings.md in the crate root). Returns `None` when no record is
/// apoc-classified (F-1-11): the emptiness decision lives here, the one
/// definition of "apoc record", so the caller writes apoc-report.txt
/// exactly when this returns `Some`.
pub fn render_apoc_capture(results: &[QueryResult], apoc_enabled: bool) -> Option<String> {
    let apoc_results: Vec<&QueryResult> = results.iter().filter(|r| r.apoc).collect();
    if apoc_results.is_empty() {
        return None;
    }
    let mut text = format!(
        "# apoc capture ({} statements, flag {})\n",
        apoc_results.len(),
        if apoc_enabled { "on" } else { "off" }
    );
    for r in &apoc_results {
        text.push_str(&format!(
            "{} {}:{} — {}\n",
            r.outcome_line(),
            r.dataset,
            r.line_text(),
            r.statement_text()
        ));
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::fixtures::{record, statement_less_record};
    use crate::record::SkipReason;

    /// An apoc-marked record — the explicit shape, so every apoc call
    /// site says so instead of a positional `true`.
    fn record_apoc(outcome: Outcome) -> QueryResult {
        QueryResult {
            apoc: true,
            ..record(outcome)
        }
    }

    #[test]
    fn summary_counts_by_skip_reason() {
        let mut s = Summary::default();
        s.record(&Outcome::Pass);
        s.record(&Outcome::Fail("x".into()));
        s.record(&Outcome::Skipped(SkipReason::Gds));
        s.record(&Outcome::Skipped(SkipReason::Apoc));
        s.record(&Outcome::Skipped(SkipReason::Dialect("why")));
        s.record(&Outcome::PassWithOrderNote("list order".into()));
        s.record(&Outcome::SetupFailed("pull failed".into()));
        s.record(&Outcome::InconclusiveTimebox("20s".into()));
        assert_eq!(
            s.line(),
            "1 passed (1 with an order-only note), 1 failed; skipped: 1 gds, 1 apoc, 1 dialect; \
             1 inconclusive (reference timebox); 1 setup failures"
        );
    }

    #[test]
    fn summary_of_aggregates_in_one_pass() {
        let results = vec![
            record(Outcome::Pass),
            record(Outcome::PassWithOrderNote("list order".into())),
            record(Outcome::Fail("x".into())),
            record_apoc(Outcome::Skipped(SkipReason::Apoc)),
            record(Outcome::InconclusiveTimebox("20s".into())),
        ];
        let s = Summary::of(&results);
        assert_eq!(
            s.line(),
            "1 passed (1 with an order-only note), 1 failed; skipped: 0 gds, 1 apoc, 0 dialect; \
             1 inconclusive (reference timebox); 0 setup failures"
        );
    }

    /// Every variant's report rendering: `Pass` renders nothing, the
    /// statement-bearing variants print the stable id and the full
    /// statement, and a setup failure renders as the dataset name plus its
    /// error text (the NOT TESTED entry).
    #[test]
    fn report_line_covers_each_variant() {
        assert_eq!(record(Outcome::Pass).report_line(), None);
        assert_eq!(
            record(Outcome::PassWithOrderNote("list order".into())).report_line(),
            Some("ORDER-ONLY ds:7:a6f6d0e8 — list order\n  MATCH (n) RETURN n\n".to_string())
        );
        assert_eq!(
            record(Outcome::Fail("row 2 differs".into())).report_line(),
            Some("DIFF ds:7:a6f6d0e8 — row 2 differs\n  MATCH (n) RETURN n\n".to_string())
        );
        assert_eq!(
            record(Outcome::Skipped(SkipReason::Gds)).report_line(),
            Some("SKIP ds:7:a6f6d0e8 — Gds\n  MATCH (n) RETURN n\n".to_string())
        );
        // The Dialect arm renders through SkipReason's Display (the reason
        // text, never a derived Debug spelling) — pinned separately because
        // Gds/Apoc's Debug and Display spellings coincide.
        assert_eq!(
            record(Outcome::Skipped(SkipReason::Dialect("browser command"))).report_line(),
            Some("SKIP ds:7:a6f6d0e8 — browser command\n  MATCH (n) RETURN n\n".to_string())
        );
        assert_eq!(
            record(Outcome::SetupFailed("pull failed".into())).report_line(),
            Some("SETUP-FAILED ds — pull failed\n".to_string())
        );
        assert_eq!(
            record(Outcome::InconclusiveTimebox("20s".into())).report_line(),
            Some("INCONCLUSIVE ds:7:a6f6d0e8 — 20s\n  MATCH (n) RETURN n\n".to_string())
        );
    }

    /// A DIFF entry prints the stable statement id and the full statement
    /// — every line, indented — not only the first line (which is often a
    /// `// comment`).
    #[test]
    fn diff_entry_prints_the_id_and_the_full_statement() {
        let mut r = record(Outcome::Fail("row 0 differs".into()));
        r.statement = Some("// find the cast\nMATCH (p:Person)\nRETURN p.name".to_string());
        assert_eq!(
            r.report_line(),
            Some(format!(
                "DIFF {} — row 0 differs\n  // find the cast\n  MATCH (p:Person)\n  \
                 RETURN p.name\n",
                r.id()
            ))
        );
    }

    /// F-1-4: a statement-less record (a dataset-level setup failure)
    /// renders its report entry as the dataset name plus the error — no
    /// fabricated line or statement.
    #[test]
    fn statement_less_record_report_line_carries_no_fake_fields() {
        assert_eq!(
            statement_less_record(Outcome::SetupFailed("pull failed".into())).report_line(),
            Some("SETUP-FAILED ds — pull failed\n".to_string())
        );
    }

    /// The report leads with the summary line and the per-dataset rollup,
    /// lists setup-failed datasets first under NOT TESTED, then the
    /// remaining non-pass records. The setup-failed record carries only
    /// real data (F-1-4): no adoc line, no statement — the error text is
    /// the outcome's.
    #[test]
    fn render_run_starts_with_summary_rollup_then_not_tested() {
        let mut untested = statement_less_record(Outcome::SetupFailed("pull failed".into()));
        untested.dataset = "pole".to_string();
        let results = vec![
            record(Outcome::Pass),
            record(Outcome::Fail("row 2 differs".into())),
            record_apoc(Outcome::Skipped(SkipReason::Apoc)),
            untested,
        ];
        let summary = Summary::of(&results);
        assert_eq!(
            render_run(&results, &summary),
            "1 passed (0 with an order-only note), 1 failed; skipped: 0 gds, 1 apoc, 0 dialect; \
             0 inconclusive (reference timebox); 1 setup failures\n\
             \n\
             dataset  passed  order-only  failed  inconclusive  skipped  setup\n\
             ds       1       0           1       0             1        ok\n\
             pole     0       0           0       0             0        FAILED\n\
             \n\
             NOT TESTED (setup failed)\n\
             SETUP-FAILED pole — pull failed\n\
             DIFF ds:7:a6f6d0e8 — row 2 differs\n  MATCH (n) RETURN n\n\
             SKIP ds:7:a6f6d0e8 — Apoc\n  MATCH (n) RETURN n\n"
        );
    }

    /// No setup failures: no NOT TESTED section, and the summary still
    /// leads.
    #[test]
    fn render_run_omits_not_tested_when_every_dataset_ran() {
        let results = vec![
            record(Outcome::Pass),
            record(Outcome::Fail("row 2 differs".into())),
        ];
        let summary = Summary::of(&results);
        let text = render_run(&results, &summary);
        assert!(!text.contains("NOT TESTED"), "{text}");
        assert!(text.starts_with(&summary.line()), "{text}");
    }

    #[test]
    fn render_summary_markdown_renders_summary_and_rollup_table() {
        let mut untested = statement_less_record(Outcome::SetupFailed("pull failed".into()));
        untested.dataset = "pole".to_string();
        let results = vec![
            record(Outcome::Pass),
            record(Outcome::Fail("row 2 differs".into())),
            untested,
        ];
        let summary = Summary::of(&results);
        assert_eq!(
            render_summary_markdown(&results, &summary),
            "## Graph-examples differential\n\
             \n\
             1 passed (0 with an order-only note), 1 failed; skipped: 0 gds, 0 apoc, 0 dialect; \
             0 inconclusive (reference timebox); 1 setup failures\n\
             \n\
             | dataset | passed | order-only | failed | inconclusive | skipped | setup |\n\
             | --- | ---: | ---: | ---: | ---: | ---: | --- |\n\
             | ds | 1 | 0 | 1 | 0 | 0 | ok |\n\
             | pole | 0 | 0 | 0 | 0 | 0 | FAILED |\n"
        );
    }

    #[test]
    fn render_apoc_capture_lists_apoc_records_only() {
        let results = vec![
            record(Outcome::Pass),
            record_apoc(Outcome::Fail("row 2 differs".into())),
            record_apoc(Outcome::Skipped(SkipReason::Apoc)),
        ];
        assert_eq!(
            render_apoc_capture(&results, false),
            Some(
                "# apoc capture (2 statements, flag off)\n\
                 fail: row 2 differs ds:7 — MATCH (n) RETURN n\n\
                 skipped: Apoc ds:7 — MATCH (n) RETURN n\n"
                    .to_string()
            )
        );
        assert!(render_apoc_capture(&results, true)
            .expect("apoc-classified records exist")
            .starts_with("# apoc capture (2 statements, flag on)\n"));
    }

    /// F-1-11: with no apoc-classified record the renderer returns `None`
    /// and `write_reports` writes no apoc-report.txt — the emptiness
    /// decision is the renderer's, not a second `r.apoc` filter.
    #[test]
    fn render_apoc_capture_is_none_without_apoc_records() {
        let results = vec![
            record(Outcome::Pass),
            record(Outcome::Fail("row 2 differs".into())),
        ];
        assert_eq!(render_apoc_capture(&results, false), None);
        assert_eq!(render_apoc_capture(&results, true), None);
        assert_eq!(render_apoc_capture(&[], false), None);
    }
}
