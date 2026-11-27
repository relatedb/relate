//! The result record model (ADR-0144): one statement's differential outcome
//! ([`Outcome`]), its record ([`QueryResult`]), and the run aggregate
//! ([`Summary`]) — the pure data model every renderer (the run report, the
//! timing views, the apoc capture) consumes. Rendering lives one layer up:
//! [`crate::report`] renders outcomes, [`crate::timings`] renders their
//! engine timings.
//!
//! Per ADR-0176 the nightly is report-only: only [`Outcome::SetupFailed`]
//! fails the run; diffs, order-only notes, skips and inconclusive reference
//! timeboxes are reported, never gated on.

/// Why a statement does not run differentially — the payload of
/// [`Outcome::Skipped`], classified by [`crate::adoc`] (dialect/policy
/// skips) and produced by [`crate::suite`]'s run-time dialect skips.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// `gds.*` — the graph data science sandbox queries; different
    /// product, excluded per ADR-0144.
    Gds,
    /// `apoc.*`/apocf — deferred behind the on-demand apoc flag until the
    /// apoc PR merges.
    Apoc,
    /// Dialect the engine does not carry (PROFILE/EXPLAIN output queries,
    /// multi-database `USE`, administration commands, …).
    Dialect(&'static str),
}

/// The human-facing rendering of a skip reason — the one definition both
/// renderers (the report line's SKIP entry, the apoc capture's outcome
/// line) consume, so a skip never leaks a derived `Debug` spelling into an
/// uploaded artifact (F-1-12/F-1-13): the variant name for `Gds`/`Apoc`,
/// the reason text for `Dialect`.
impl std::fmt::Display for SkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.text())
    }
}

impl SkipReason {
    /// The reason as its one owned text ([`SkipReason::Display`] and
    /// [`Outcome::payload`] both consume it, so the two cannot drift).
    fn text(&self) -> &'static str {
        match self {
            SkipReason::Gds => "Gds",
            SkipReason::Apoc => "Apoc",
            SkipReason::Dialect(reason) => reason,
        }
    }
}

/// One statement's differential outcome.
#[derive(Debug, Clone)]
pub enum Outcome {
    Pass,
    /// Rows match exactly, or match once list order inside cells is ignored
    /// (`collect()` order is unspecified by Cypher) — or, for a LIMIT/SKIP
    /// without ORDER BY, every returned row exists in the reference's full
    /// result set. The note records which weaker relation held.
    PassWithOrderNote(String),
    /// A differential diff (row/content mismatch or an engine-side error).
    /// Reported and tracked outside the suite; never fails the run
    /// (ADR-0176).
    Fail(String),
    Skipped(SkipReason),
    /// An infrastructure failure (ADR-0176 kinds 1, 3, 4): the dataset's
    /// setup failed — artifact fetch, database registration, pull, opening
    /// the pulled copy, a pull-verification count mismatch, or an adoc
    /// with zero runnable statements — so there is nothing to
    /// differentially test. The only outcome that fails the run.
    SetupFailed(String),
    /// The reference run exceeded its timebox: with no trustworthy
    /// expected result there is nothing to diff, so the statement is
    /// reported as inconclusive, never failed (ADR-0176).
    InconclusiveTimebox(String),
}

/// One statement's full result record.
#[derive(Debug, Clone)]
pub struct QueryResult {
    pub dataset: String,
    /// Adoc block line (1-based). `None` for a record with no adoc source
    /// — a dataset-level setup failure or a pull-verification query
    /// (F-1-4): a fabricated 0 would contradict the 1-based invariant.
    pub line: Option<usize>,
    /// Full statement text — the stable id hashes this, so two statements
    /// sharing one adoc block (and one first line) never share an id.
    /// `None` for a dataset-level setup failure, which has no statement:
    /// its error text lives in the outcome, not in a fabricated statement
    /// field (F-1-4).
    pub statement: Option<String>,
    pub outcome: Outcome,
    /// Engine-side wall time of the query in milliseconds, measured in
    /// [`crate::suite::run_engine`] — the one place a query is timed. `None`
    /// exactly when the engine never ran the statement (a skip, a reference
    /// error, a dataset-level setup failure); every run — rows returned,
    /// diff, engine error, timebox — carries its measured time, so the
    /// slowest statements surface even when they failed.
    pub engine_ms: Option<f64>,
    /// The statement referenced `apoc.*`/`apocf` and ran under the on-demand
    /// apoc flag (ADR-0144): its outcome belongs to the apoc capture, the
    /// local record the apoc branch consumes.
    pub apoc: bool,
}

impl QueryResult {
    /// The dataset-level record (a setup failure, F-1-4/F-2-2): no adoc
    /// line, no statement, no engine run — the error text lives in the
    /// outcome, never in fabricated fields. The one constructor the
    /// run-fold sites call (the integration test's `fold_outcomes`), so a
    /// statement-less record exists in exactly this shape.
    pub fn dataset_level(dataset: &str, outcome: Outcome) -> Self {
        QueryResult {
            dataset: dataset.to_string(),
            line: None,
            statement: None,
            outcome,
            engine_ms: None,
            apoc: false,
        }
    }

    /// Stable per-statement id (`dataset:adoc-line:sha256-prefix`): nightly
    /// reports stay diffable across runs, so a new or resolved diff is
    /// visible by comparing report artifacts (ADR-0176 removed the
    /// committed baseline; the id stays). A record with no adoc line or no
    /// statement (a dataset-level setup failure, a pull verification)
    /// renders the missing part as `-` — via [`Self::line_text`] and
    /// [`Self::statement_id_text`], the one definitions of that rule.
    pub fn id(&self) -> String {
        format!(
            "{}:{}:{}",
            self.dataset,
            self.line_text(),
            self.statement_id_text()
        )
    }

    /// The record's adoc line for rendering: the 1-based line number, or
    /// `-` for a record with no adoc source (a dataset-level setup failure
    /// or a pull-verification query). The one definition of the
    /// `None`-renders-as-`-` rule for the line field (F-3-5), next to
    /// [`Self::statement_id_text`], its sibling for the statement field.
    pub(crate) fn line_text(&self) -> String {
        self.line.map_or_else(|| "-".to_string(), |l| l.to_string())
    }

    /// The record's statement id part for rendering: the statement's
    /// [`stmt_hash`], or `-` for a record with no statement (a
    /// dataset-level setup failure, a pull verification). The one
    /// definition of the `None`-renders-as-`-` rule for the statement
    /// field, next to [`Self::line_text`] (F-1-16).
    pub(crate) fn statement_id_text(&self) -> String {
        self.statement
            .as_deref()
            .map_or_else(|| "-".to_string(), stmt_hash)
    }

    /// The record's statement text. Every caller renders a
    /// statement-bearing record (a dataset-level setup failure never
    /// reaches a statement renderer), so a record without a statement
    /// here is an invariant violation, not an empty string.
    pub(crate) fn statement_text(&self) -> &str {
        self.statement
            .as_deref()
            .expect("a statement-bearing record carries its statement")
    }
}

/// The first four bytes of the statement's SHA-256 as eight hex digits: a
/// specified, stable hash — `DefaultHasher`'s algorithm is unspecified and
/// may change between Rust releases, which would silently change every
/// statement id.
pub(crate) fn stmt_hash(statement: &str) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(statement.as_bytes());
    format!(
        "{:08x}",
        u32::from_be_bytes(
            digest[..4]
                .try_into()
                .expect("a SHA-256 digest is 32 bytes, so [..4] is always 4 bytes"),
        )
    )
}

/// Aggregate of one suite run.
#[derive(Debug, Default)]
pub struct Summary {
    pub pass: usize,
    /// Passes whose only difference was unspecified ordering (list order or
    /// LIMIT-without-ORDER-BY selection) — reported, never fatal.
    pub pass_order_note: usize,
    pub fail: usize,
    pub skipped_gds: usize,
    pub skipped_apoc: usize,
    pub skipped_dialect: usize,
    /// Infrastructure failures (ADR-0176) — the only count that fails the
    /// run.
    pub setup_failed: usize,
    /// Statements whose reference run exceeded its timebox — reported as
    /// inconclusive, never fatal.
    pub inconclusive_timebox: usize,
}

impl Outcome {
    /// The outcome's one-word kind — the discriminator a stable run-to-run
    /// diff line prints (the sandbox A/B driver's `OUT` lines), the
    /// rendering counterpart of [`Summary::record`]'s count arm beside it.
    pub fn kind(&self) -> &'static str {
        match self {
            Outcome::Pass => "Pass",
            Outcome::PassWithOrderNote(_) => "OrderNote",
            Outcome::Fail(_) => "Fail",
            Outcome::Skipped(_) => "Skip",
            Outcome::SetupFailed(_) => "SetupFailed",
            Outcome::InconclusiveTimebox(_) => "Inconclusive",
        }
    }

    /// The outcome's payload: the "why" of a non-pass, `None` for a plain
    /// pass. A skip renders its reason through [`SkipReason`]'s one text
    /// (the same text [`std::fmt::Display`] writes).
    pub fn payload(&self) -> Option<&str> {
        match self {
            Outcome::Pass => None,
            Outcome::PassWithOrderNote(why)
            | Outcome::Fail(why)
            | Outcome::SetupFailed(why)
            | Outcome::InconclusiveTimebox(why) => Some(why),
            Outcome::Skipped(reason) => Some(reason.text()),
        }
    }
}

impl Summary {
    /// Aggregate a run's records in one pass — the one aggregation fold
    /// every per-dataset view consumes, over any borrowing of the records
    /// (a slice, an iterator of `&QueryResult`).
    pub fn of<'a>(results: impl IntoIterator<Item = &'a QueryResult>) -> Self {
        let mut summary = Self::default();
        for r in results {
            summary.record(&r.outcome);
        }
        summary
    }
    pub(crate) fn record(&mut self, outcome: &Outcome) {
        match outcome {
            Outcome::Pass => self.pass += 1,
            Outcome::PassWithOrderNote(_) => self.pass_order_note += 1,
            Outcome::Fail(_) => self.fail += 1,
            Outcome::Skipped(SkipReason::Gds) => self.skipped_gds += 1,
            Outcome::Skipped(SkipReason::Apoc) => self.skipped_apoc += 1,
            Outcome::Skipped(SkipReason::Dialect(_)) => self.skipped_dialect += 1,
            Outcome::SetupFailed(_) => self.setup_failed += 1,
            Outcome::InconclusiveTimebox(_) => self.inconclusive_timebox += 1,
        }
    }

    /// One human-readable line.
    pub fn line(&self) -> String {
        format!(
            "{} passed ({} with an order-only note), {} failed; skipped: {} gds, {} apoc, \
             {} dialect; {} inconclusive (reference timebox); {} setup failures",
            self.pass,
            self.pass_order_note,
            self.fail,
            self.skipped_gds,
            self.skipped_apoc,
            self.skipped_dialect,
            self.inconclusive_timebox,
            self.setup_failed,
        )
    }
}

/// Group `(key, value)` pairs by key, in first-seen order — the one
/// grouping derivation (F-2-3): every per-dataset view (the report's
/// rollup via [`group_by_dataset`], the timing tables) consumes it, so the
/// two never disagree on grouping or order policy.
/// Group a run's records by dataset, in first-seen (run) order — the one
/// derivation every per-dataset record view (the report's rollup) consumes,
/// over [`crate::group_first_seen`].
pub(crate) fn group_by_dataset(results: &[QueryResult]) -> Vec<(&str, Vec<&QueryResult>)> {
    crate::group_first_seen(results.iter().map(|r| (r.dataset.as_str(), r)))
}

/// The shared record fixtures (F-1-8/F-1-9/F-1-3): a named test-support
/// module sibling test modules (report.rs's, timings.rs's) import, instead
/// of piercing `record::tests`.
#[cfg(test)]
pub(crate) mod fixtures {
    use super::{Outcome, QueryResult};

    /// The shared record fixture: a non-apoc record with no engine timing,
    /// the base shape every test module layers its own variant on
    /// (report.rs's apoc record, timings.rs's timed record).
    pub(crate) fn record(outcome: Outcome) -> QueryResult {
        QueryResult {
            dataset: "ds".to_string(),
            line: Some(7),
            statement: Some("MATCH (n) RETURN n".to_string()),
            outcome,
            engine_ms: None,
            apoc: false,
        }
    }

    /// The shared statement-less fixture (F-1-4/F-1-15): a dataset-level
    /// setup failure's shape — no adoc line, no statement, the error text
    /// lives in the outcome.
    pub(crate) fn statement_less_record(outcome: Outcome) -> QueryResult {
        QueryResult {
            line: None,
            statement: None,
            ..record(outcome)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{record, statement_less_record};
    use super::*;

    /// F-1-8 / ADR-0176: the statement id hash is pinned to SHA-256 (first
    /// four bytes, big-endian, hex). Expected value cross-checked with
    /// `echo -n "MATCH (n) RETURN n" | shasum -a 256 | cut -c1-8`.
    #[test]
    fn stmt_hash_is_sha256_derived() {
        assert_eq!(stmt_hash("MATCH (n) RETURN n"), "a6f6d0e8");
    }

    /// The stable id is `dataset:adoc-line:stmt-hash` — two statements
    /// sharing one adoc block (and one first line) never share an id.
    #[test]
    fn record_id_combines_dataset_line_and_statement_hash() {
        assert_eq!(record(Outcome::Pass).id(), "ds:7:a6f6d0e8");
    }

    /// F-1-4: a statement-less record (a dataset-level setup failure)
    /// renders the missing id parts as `-` instead of hashing fabricated
    /// prose.
    #[test]
    fn statement_less_record_id_carries_no_fake_fields() {
        assert_eq!(
            statement_less_record(Outcome::SetupFailed("pull failed".into())).id(),
            "ds:-:-"
        );
    }

    /// F-1-6: `statement_text` is total over the statement-bearing records
    /// its renderers receive — a record without a statement violates the
    /// invariant and panics rather than silently rendering "".
    #[test]
    #[should_panic(expected = "a statement-bearing record carries its statement")]
    fn statement_text_panics_on_a_statement_less_record() {
        let mut r = record(Outcome::Pass);
        r.statement = None;
        let _ = r.statement_text();
    }

    /// The run-to-run diff rendering: every variant's one-word kind and
    /// its payload (the why of a non-pass, `None` for a plain pass). A
    /// skip's payload is its reason's own text, never a `Debug` spelling.
    #[test]
    fn outcome_kind_and_payload_cover_every_variant() {
        assert_eq!(Outcome::Pass.kind(), "Pass");
        assert_eq!(Outcome::Pass.payload(), None);
        let cases = [
            (
                Outcome::PassWithOrderNote("list order differs".into()),
                "OrderNote",
                "list order differs",
            ),
            (
                Outcome::Fail("row 2 differs".into()),
                "Fail",
                "row 2 differs",
            ),
            (
                Outcome::SetupFailed("pull failed".into()),
                "SetupFailed",
                "pull failed",
            ),
            (
                Outcome::InconclusiveTimebox("reference timeout".into()),
                "Inconclusive",
                "reference timeout",
            ),
            (
                Outcome::Skipped(SkipReason::Dialect("no USE in CE")),
                "Skip",
                "no USE in CE",
            ),
            (Outcome::Skipped(SkipReason::Gds), "Skip", "Gds"),
        ];
        for (outcome, kind, payload) in cases {
            assert_eq!(outcome.kind(), kind);
            assert_eq!(outcome.payload(), Some(payload));
        }
    }

    /// The one grouping derivation: first-seen dataset order, every record
    /// under its dataset.
    #[test]
    fn group_by_dataset_keeps_first_seen_order() {
        let mut other = record(Outcome::Pass);
        other.dataset = "other".to_string();
        let results = vec![
            record(Outcome::Pass),
            other,
            record(Outcome::Fail("row 2 differs".into())),
        ];
        let groups = group_by_dataset(&results);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].0, "ds");
        assert_eq!(groups[0].1.len(), 2);
        assert_eq!(groups[1].0, "other");
        assert_eq!(groups[1].1.len(), 1);
    }
}
