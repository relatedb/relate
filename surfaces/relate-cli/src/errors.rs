//! Query-run error reporting (ADR-0194): classification, the JSON error
//! envelope, and the one report path every failing query-run site goes
//! through.
//!
//! One engine [`relate::Error`] maps to one [`Classified`], which fixes
//! the process exit code, the stable `code` and `kind` strings and whether a
//! retry can help. The classifier match has no wildcard: a new
//! engine error variant fails to compile until it is classified. The store
//! open has one class override ([`classify_open`]): a semantic-family
//! rejection there is an open failure (exit 1), not a rejected statement
//! (exit 6).
//!
//! ADR-0502 §2: the classification *facts* are the engine's, single-sourced
//! in `relate::error_info` — the retryable bit below is read from it,
//! never restated per arm. The exit-code class and the `kind` string are
//! this surface's own presentation (the task's consumer-specific kind), so
//! they stay a local derivation.

use std::io::Write;

use relate_formatter::{sanitize_error_text, Mode};

use crate::syntax_position::SyntaxPosition;
use crate::{write_error, write_report};

/// The exit-code classes a query run can produce, a subset of the closed
/// enum shared with `neo4j-cli` (ADR-0194). `Timeout` (ADR-0241) is produced
/// only by [`report_timeout_to`] — it has no engine [`relate::Error`]
/// variant, so [`classify`] never returns it; the reserved codes (3, 4, 8)
/// are documented but never produced here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ErrorClass {
    /// Exit 1: store open/IO, capacity, query execution, internal failures.
    Fatal,
    /// Exit 5: a write-write conflict, or a store locked by another opener.
    Conflict,
    /// Exit 6: the statement itself is rejected (syntax, semantic, type).
    Validation,
    /// Exit 7: a statement exceeded `--timeout` (ADR-0241).
    Timeout,
}

impl ErrorClass {
    fn exit_code(self) -> i32 {
        match self {
            Self::Fatal => 1,
            Self::Conflict => 5,
            Self::Validation => 6,
            Self::Timeout => 7,
        }
    }

    fn code(self) -> &'static str {
        match self {
            Self::Fatal => "fatal_error",
            Self::Conflict => "conflict",
            Self::Validation => "validation_error",
            Self::Timeout => "timeout",
        }
    }
}

/// The envelope's `kind`: the engine-error family, one variant per
/// [`classify`] row. A type rather than a string so the open-site override
/// can name the semantic family as the classification's own result instead
/// of re-listing the engine variants that classify into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ErrorFamily {
    Syntax,
    Semantic,
    Argument,
    Type,
    Execution,
    Conflict,
    Capacity,
    Resource,
    Storage,
    Locked,
    Io,
    Internal,
}

impl ErrorFamily {
    /// The stable `kind` string the envelope carries.
    fn as_str(self) -> &'static str {
        match self {
            Self::Syntax => "syntax",
            Self::Semantic => "semantic",
            Self::Argument => "argument",
            Self::Type => "type",
            Self::Execution => "execution",
            Self::Conflict => "conflict",
            Self::Capacity => "capacity",
            Self::Resource => "resource",
            Self::Storage => "storage",
            Self::Locked => "locked",
            Self::Io => "io",
            Self::Internal => "internal",
        }
    }
}

/// Everything the report path needs to know about one engine error.
struct Classified {
    class: ErrorClass,
    /// The engine error family, as the envelope's `kind`.
    kind: ErrorFamily,
    /// Only a write-write conflict is resolved by a plain retry; a locked
    /// store needs its other opener to finish first.
    retryable: bool,
}

impl Classified {
    /// The classification of a cancelled query (ADR-0241): a cancelled query
    /// is an `Error::Cancelled` in the engine, which [`classify`] files with
    /// the execution failures. Only the timeout-specific class is spelled
    /// here; `kind` and `retryable` are derived from that one
    /// classification, so they cannot drift from it.
    fn timeout() -> Self {
        let Classified {
            class: _,
            kind,
            retryable,
        } = classify(&relate::Error::Cancelled);
        Self {
            class: ErrorClass::Timeout,
            kind,
            retryable,
        }
    }
}

fn classify(e: &relate::Error) -> Classified {
    use relate::{ArithmeticFailure, Error as E};
    use ErrorClass::{Conflict, Fatal, Validation};
    // ADR-0502 §2: the retryable bit is the engine's classification fact
    // (the drivers' transient test — a write-write conflict and a retryable
    // processing cap are the only retryable classes), read from the one
    // source instead of restated per arm below.
    let retryable = relate::error_info(e).retryable;
    // The exit-code class and the `kind` string are the CLI's presentation
    // of the error: they are finer than the engine's classification in both
    // directions (a `Semantic` and an `Execution` rejection share one
    // engine class but exit 6 and 1; a `Capacity` refusal and a
    // non-retryable `ResourceLimit` share one Neo4j code), so they stay a
    // local, exhaustive derivation.
    let (class, kind) = match e {
        // The typed procedure-not-found (ADR-0138) keeps the legacy Syntax
        // class its pinned rendering had.
        E::Syntax(_) | E::ProcedureNotFound { .. } => (Validation, ErrorFamily::Syntax),
        // The typed uniqueness error keeps the legacy Semantic class, as
        // do the typed parameter-missing and unknown-database errors
        // (legacy `Semantic`, ADR-0307). This one family is what the
        // open-site override reads (see `classify_open`).
        E::Semantic(_)
        | E::UniquenessViolation { .. }
        | E::ParameterMissing(_)
        | E::DatabaseNotFound { .. } => (Validation, ErrorFamily::Semantic),
        // ADR-0489: a DDL option/index-config argument rejection is a
        // statement rejection like the other validation classes; the
        // run-time argument rejection carries the same class.
        E::ArgumentError(_) | E::ArgumentValue { .. } => (Validation, ErrorFamily::Argument),
        // The typed argument-type/type-error details classify as type
        // errors; the legacy spellings were `Error::ty("InvalidArgumentType:
        // …")` / `Error::ty("TypeError: …")` (a handful reached the CLI as
        // `Execution` and were Fatal — typing them is the deliberate fix).
        // Division and modulo by zero were `Error::ty` too (ADR-0307).
        E::Type(_)
        | E::InvalidArgumentType(_)
        | E::TypeError(_)
        | E::Arithmetic(ArithmeticFailure::DivisionByZero | ArithmeticFailure::ModuloByZero) => {
            (Validation, ErrorFamily::Type)
        }
        // Rounding-necessary and cancellation were `Execution` (ADR-0307).
        E::Execution(_) | E::Arithmetic(ArithmeticFailure::RoundingNecessary) | E::Cancelled => {
            (Fatal, ErrorFamily::Execution)
        }
        E::Conflict => (Conflict, ErrorFamily::Conflict),
        // The capacity family — the graph cap, the expression cap and the
        // per-record WAL payload cap (ADR-0494) — is the one capacity
        // family: fatal, exit 1, kind `capacity`.
        E::Capacity { .. } => (Fatal, ErrorFamily::Capacity),
        E::ResourceLimit { .. } => (Fatal, ErrorFamily::Resource),
        E::Storage(_) => (Fatal, ErrorFamily::Storage),
        E::Locked(_) => (Conflict, ErrorFamily::Locked),
        E::Io(_) => (Fatal, ErrorFamily::Io),
        E::Internal(_) => (Fatal, ErrorFamily::Internal),
    };
    Classified {
        class,
        kind,
        retryable,
    }
}

/// The open-site classification: a semantic-family rejection from
/// `Database::open` is the store failing to open (exit 1), never a statement
/// being rejected (exit 6). The family is read off [`classify`]'s own result,
/// so every variant it files under the semantic kind — including the typed
/// uniqueness, parameter-missing and unknown-database variants (ADR-0307) —
/// gets the override. Everything else keeps [`classify`]'s class, so a locked
/// store still exits 5.
fn classify_open(e: &relate::Error) -> Classified {
    let mut classified = classify(e);
    if classified.kind == ErrorFamily::Semantic {
        classified.class = ErrorClass::Fatal;
    }
    classified
}

/// Which statement of a multi-statement run is executing (ADR-0199). Built by
/// the batch loop per statement; there is none for a single statement. The
/// fields are private so the invariant the arithmetic below relies on, that
/// `1 <= statement <= statements`, is established once, in [`Position::new`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Position {
    statement: usize,
    statements: usize,
}

impl Position {
    /// Statement number `statement` (1-based) of `statements`.
    pub(crate) fn new(statement: usize, statements: usize) -> Self {
        assert!(
            (1..=statements).contains(&statement),
            "statement {statement} of {statements}"
        );
        Self {
            statement,
            statements,
        }
    }
}

/// The one rendering of a position — `2 of 3` — shared by the ADR-0199
/// progress line and the ADR-0402 zero-effect note, so the two cannot spell
/// the numbering differently.
impl std::fmt::Display for Position {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} of {}", self.statement, self.statements)
    }
}

/// Where a multi-statement run stood when one of its statements failed: its
/// [`Position`] and whether an explicit `.begin` transaction was open, so the
/// statements that completed before it are not durable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Progress {
    pub at: Position,
    pub in_transaction: bool,
}

impl Progress {
    /// Statements that completed before the failed one.
    fn completed(&self) -> usize {
        self.at.statement - 1
    }

    /// Statements that completed and are durable: all of them, unless an
    /// explicit transaction is open (nothing is committed until `.commit`).
    /// The one place that decision is made; the stderr line and the error
    /// object both read it.
    fn committed(&self) -> usize {
        if self.in_transaction {
            0
        } else {
            self.completed()
        }
    }

    /// Statements that never ran.
    fn not_run(&self) -> usize {
        self.at.statements - self.at.statement
    }

    /// The error-object fields for this failure (ADR-0199): the same facts as
    /// the stderr line, as data.
    fn error_fields(&self) -> [(String, serde_json::Value); 3] {
        [
            ("statement".into(), self.at.statement.into()),
            ("statements".into(), self.at.statements.into()),
            ("committed".into(), self.committed().into()),
        ]
    }

    /// The stderr line: `statement 2 of 3 failed; 1 completed and committed;
    /// 1 not run`.
    pub(crate) fn line(&self) -> String {
        let completed = self.completed();
        let done = if self.in_transaction {
            format!("{completed} completed, not committed (open transaction)")
        } else {
            format!("{} completed and committed", self.committed())
        };
        format!(
            "statement {} failed; {done}; {} not run",
            self.at,
            self.not_run()
        )
    }
}

/// The one envelope-shape builder (ADR-0194): the five base fields plus the
/// multi-statement progress fields (ADR-0199). Every failure envelope —
/// classified engine errors and the ADR-0241 timeout alike — is assembled
/// here from one [`Classified`], so a new envelope field cannot drift
/// between report paths and the (code, exit_code) pair cannot disagree.
fn error_map(
    classified: &Classified,
    message: String,
    progress: Option<Progress>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut error: serde_json::Map<String, serde_json::Value> = [
        ("code".into(), classified.class.code().into()),
        ("exit_code".into(), classified.class.exit_code().into()),
        ("kind".into(), classified.kind.as_str().into()),
        ("message".into(), message.into()),
        ("retryable".into(), classified.retryable.into()),
    ]
    .into_iter()
    .collect();
    if let Some(p) = progress {
        error.extend(p.error_fields());
    }
    error
}

/// The envelope map for an engine error already classified as `classified`:
/// the builder the engine-error report paths go through — [`envelope_map`]
/// on the query-run failure path (`classify`) and [`report_open_error`] on
/// the store-open path (`classify_open`) — adding the ADR-0290 syntax
/// position on top of [`error_map`]. The timeout path has no engine error
/// to take a position from and builds its map from [`error_map`] directly.
/// `message` is the same sanitized text the stderr line carries, so a
/// crafted value cannot inject a control sequence into either stream.
fn envelope_map_for(
    classified: &Classified,
    e: &relate::Error,
    progress: Option<Progress>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut error = error_map(classified, sanitize_error_text(&e.to_string()), progress);
    add_syntax_position(&mut error, e);
    error
}

/// The classified error and its envelope map for `e`.
fn envelope_map(
    e: &relate::Error,
    progress: Option<Progress>,
) -> (Classified, serde_json::Map<String, serde_json::Value>) {
    let c = classify(e);
    let error = envelope_map_for(&c, e, progress);
    (c, error)
}

/// The envelope for `e` as one compact JSON line (ADR-0194). Only the unit
/// tests render an envelope without reporting it.
#[cfg(test)]
fn envelope_json(e: &relate::Error, progress: Option<Progress>) -> String {
    let (_, error) = envelope_map(e, progress);
    envelope_line(error)
}

/// The `{"error": ...}` root wrap as one compact JSON line (ADR-0194) — the
/// one builder both the report path and the tests render through.
fn envelope_line(error: serde_json::Map<String, serde_json::Value>) -> String {
    serde_json::json!({ "error": error }).to_string()
}

/// The shared report tail every failure path goes through: the
/// multi-statement progress line on the diagnostic writer when there is one,
/// the `{"error": ...}` envelope line on the output writer in the JSON-family
/// modes, and the class's exit code. The envelope map and the returned code
/// both come from the one `classified`, so the pair cannot disagree. Report
/// paths differ only in how they print the human line and build the map.
fn report_envelope(
    classified: &Classified,
    error: serde_json::Map<String, serde_json::Value>,
    mode: &Mode,
    progress: Option<Progress>,
    out: &mut dyn Write,
    diagnostics: &mut dyn Write,
) -> i32 {
    if let Some(p) = progress {
        write_error(diagnostics, p.line());
    }
    if mode.is_json() {
        write_report(out, envelope_line(error));
    }
    classified.class.exit_code()
}

/// For a syntax error, add where it points (ADR-0290): `column` and `source`
/// when the parser echoed the line, `line` and `column` when it reported only a
/// position. Absent, never null, when the position is unknown.
fn add_syntax_position(error: &mut serde_json::Map<String, serde_json::Value>, e: &relate::Error) {
    match SyntaxPosition::from_error(e) {
        Some(SyntaxPosition::Excerpt { source, column }) => {
            error.insert("column".into(), column.into());
            error.insert("source".into(), source.into());
        }
        Some(SyntaxPosition::Line { line, column }) => {
            error.insert("line".into(), line.into());
            error.insert("column".into(), column.into());
        }
        None => {}
    }
}

/// The human text for `e` on the diagnostic writer: for a syntax error that
/// echoed its source line, the headline, the sanitized source and the caret on
/// separate lines (ADR-0290); otherwise `prefix` and the engine text on one
/// sanitized line, exactly as before.
fn print_human(prefix: &str, e: &relate::Error, diagnostics: &mut dyn Write) {
    let Some(echo) = SyntaxPosition::from_error(e).and_then(|p| p.echo_lines()) else {
        write_error(diagnostics, format_args!("{prefix}{e}"));
        return;
    };
    // The first line is the error's own text up to the echo, so its prefix
    // (`syntax error: `) comes from the error and stays as before. The source
    // and caret were built from sanitized parts, so only they keep a layout.
    let text = e.to_string();
    let (headline, _) = text.split_once('\n').expect("an echoed error spans lines");
    write_error(diagnostics, format_args!("{prefix}{headline}"));
    for line in echo {
        write_report(diagnostics, line);
    }
}

/// The context of one failure beyond the error itself, bundled so the
/// report signature stays fixed as reporting grows (ADR-0199 added the
/// multi-statement progress; ADR-0230 the failed statement's text for a
/// command did-you-mean). `ReportCtx::NONE` — a failure with neither, such
/// as the connect step — is the one empty construction.
#[derive(Debug, Clone)]
pub(crate) struct ReportCtx {
    /// Where a multi-statement run stood; `None` for a single statement.
    pub progress: Option<Progress>,
    /// The rendered ADR-0230 did-you-mean line, when the failed statement's
    /// first word is plausibly a mistyped command. Computed by the caller
    /// (`suggest::command_suggestion_line` decides applicability), so this
    /// module stays free of the command-suggestion feature.
    pub suggestion: Option<String>,
}

impl ReportCtx {
    /// No context: the failure is not a statement's.
    pub(crate) const NONE: Self = Self {
        progress: None,
        suggestion: None,
    };
}

/// Report a failed query-run step and return its process exit code: the
/// human line on stderr (`prefix` then the engine error, unchanged from before
/// the envelope existed), then, in the JSON-family modes (`Mode::is_json`:
/// `json`, `jsonl`, `envelope`), the envelope as one line on stdout. Every
/// failing statement or connect site in a query run calls this, so the steps
/// cannot drift apart; the store-open site calls [`report_open_error`], whose
/// only difference is the class override. A command did-you-mean line
/// (ADR-0230), when the caller rendered one, prints last on stderr.
pub(crate) fn report_query_error(
    prefix: &str,
    e: &relate::Error,
    mode: &Mode,
    ctx: ReportCtx,
) -> i32 {
    report_query_error_to(
        prefix,
        e,
        mode,
        ctx,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )
}

/// [`report_query_error`] with its output and diagnostic writers passed in:
/// the statement run's own path binds the process streams, while a run whose
/// writers are injected (the render-failure test) captures the human line,
/// the progress line and the envelope on the sinks it supplies.
pub(crate) fn report_query_error_to(
    prefix: &str,
    e: &relate::Error,
    mode: &Mode,
    ctx: ReportCtx,
    out: &mut dyn Write,
    diagnostics: &mut dyn Write,
) -> i32 {
    print_human(prefix, e, diagnostics);
    let (c, error) = envelope_map(e, ctx.progress);
    let code = report_envelope(&c, error, mode, ctx.progress, out, diagnostics);
    // ADR-0230: the did-you-mean line, when the caller rendered one, is
    // stderr-only and last — it never enters the envelope.
    if let Some(line) = ctx.suggestion {
        write_report(diagnostics, "");
        write_error(diagnostics, line);
    }
    code
}

/// The store-open failure report and its exit code. A rejected
/// `Database::open` is the open-failure row of the exit table (exit 1), not
/// a statement rejection: a semantic-family open error — an invalid
/// `RELATE_PREVIEW` value or bulk-batch knob — is a configuration
/// rejection that no statement can fix, so it takes the fatal class.
/// `Error::Locked`/`Error::Conflict` keep the conflict class (exit 5)
/// the table assigns to a store another opener holds. This is the open site's
/// one classification, so stderr, the envelope and the exit code cannot
/// disagree.
pub(crate) fn report_open_error(prefix: &str, e: &relate::Error, mode: &Mode) -> i32 {
    let mut diagnostics = std::io::stderr();
    let mut out = std::io::stdout();
    print_human(prefix, e, &mut diagnostics);
    let classified = classify_open(e);
    let error = envelope_map_for(&classified, e, None);
    report_envelope(&classified, error, mode, None, &mut out, &mut diagnostics)
}

/// Report a `--timeout` expiry (ADR-0241, LABS-234) and return its exit
/// code, 7: the human line on the diagnostic writer, the multi-statement
/// progress line when there is one, and — in the JSON-family modes — the same
/// envelope shape as any other failure on the output writer, with `code:
/// "timeout"` and `kind: "execution"` (a cancelled query is an
/// `Error::Cancelled` in the engine, classified with the execution failures).
/// [`report_timeout_to`] is the one implementation; a statement run binds the
/// process streams or the writers its caller supplies.
pub(crate) fn report_timeout_to(
    mode: &Mode,
    limit: std::time::Duration,
    progress: Option<Progress>,
    out: &mut dyn Write,
    diagnostics: &mut dyn Write,
) -> i32 {
    let classified = Classified::timeout();
    let message = format!("query timed out after {limit:?}");
    write_error(diagnostics, format_args!("Error: {message}"));
    let error = error_map(&classified, message, progress);
    report_envelope(&classified, error, mode, progress, out, diagnostics)
}

#[cfg(test)]
mod tests {
    use super::*;
    use relate::Error as E;

    /// The envelope for `e` and `progress`, parsed.
    fn envelope_value(e: &relate::Error, progress: Option<Progress>) -> serde_json::Value {
        serde_json::from_str(&envelope_json(e, progress)).expect("envelope is JSON")
    }

    /// The timeout classification borrows `kind` and `retryable` from
    /// `classify`'s cancellation entry — only the class is timeout-specific —
    /// and the envelope renders those borrowed fields.
    #[test]
    fn timeout_borrows_the_execution_classification() {
        let t = Classified::timeout();
        let e = classify(&E::cancelled());
        assert_eq!((t.kind, t.retryable), (e.kind, e.retryable));
        let x = classify(&E::execution("x"));
        assert_eq!((e.kind, e.retryable), (x.kind, x.retryable));
        assert_eq!((t.class.exit_code(), t.class.code()), (7, "timeout"));
        let error = error_map(&t, "query timed out".into(), None);
        assert_eq!(error["kind"], "execution");
        assert_eq!(error["retryable"], false);
    }

    #[test]
    fn classes_follow_the_adr_table() {
        let cases = [
            (E::syntax("x"), 6, "validation_error", "syntax", false),
            (E::semantic("x"), 6, "validation_error", "semantic", false),
            (
                // ADR-0489: Neo4j's ArgumentError class, a validation
                // rejection like the sibling classes.
                E::argument_error("x"),
                6,
                "validation_error",
                "argument",
                false,
            ),
            (E::ty("x"), 6, "validation_error", "type", false),
            (E::execution("x"), 1, "fatal_error", "execution", false),
            (E::conflict(), 5, "conflict", "conflict", true),
            (E::capacity(2, 1), 1, "fatal_error", "capacity", false),
            (
                // ADR-0494: the per-record WAL payload cap refusal is the
                // capacity family, never the storage family it replaced.
                E::capacity_wal_payload(67_108_865, E::WAL_PAYLOAD_CAP_BYTES),
                1,
                "fatal_error",
                "capacity",
                false,
            ),
            (E::storage("x"), 1, "fatal_error", "storage", false),
            (E::Locked("x".into()), 5, "conflict", "locked", false),
            (E::io("x"), 1, "fatal_error", "io", false),
            (E::internal("x"), 1, "fatal_error", "internal", false),
            // ADR-0307's typed variants keep their legacy classes.
            (
                E::parameter_missing("x"),
                6,
                "validation_error",
                "semantic",
                false,
            ),
            (
                E::database_not_found("x"),
                6,
                "validation_error",
                "semantic",
                false,
            ),
            (
                E::arithmetic(relate::ArithmeticFailure::DivisionByZero),
                6,
                "validation_error",
                "type",
                false,
            ),
            (
                E::arithmetic(relate::ArithmeticFailure::ModuloByZero),
                6,
                "validation_error",
                "type",
                false,
            ),
            (
                E::arithmetic(relate::ArithmeticFailure::RoundingNecessary),
                1,
                "fatal_error",
                "execution",
                false,
            ),
            (E::cancelled(), 1, "fatal_error", "execution", false),
        ];
        for (err, exit, code, kind, retryable) in cases {
            let v = envelope_value(&err, None);
            let e = &v["error"];
            assert_eq!(e["exit_code"], exit, "{err}");
            assert_eq!(e["code"], code, "{err}");
            assert_eq!(e["kind"], kind, "{err}");
            assert_eq!(e["retryable"], retryable, "{err}");
        }
    }

    #[test]
    fn open_site_reclassifies_the_whole_semantic_family_as_fatal_and_keeps_lock_as_conflict() {
        // A semantic-family rejection from `Database::open` is a store-open
        // failure (exit 1), while the same variant during a query run stays a
        // statement rejection (exit 6). The family is `classify`'s semantic
        // kind, so every member gets the override — not just `Semantic`.
        let family = [
            E::semantic("invalid RELATE_PREVIEW value"),
            E::uniqueness_violation("u"),
            E::parameter_missing("x"),
            E::database_not_found("nope"),
        ];
        for err in family {
            let query = classify(&err);
            assert_eq!(query.kind, ErrorFamily::Semantic, "{err}");
            assert_eq!(query.class.exit_code(), 6, "{err}");
            let open = classify_open(&err);
            assert_eq!(
                (open.class.exit_code(), open.class.code()),
                (1, "fatal_error"),
                "{err}"
            );
            assert_eq!(
                (open.kind, open.retryable),
                (ErrorFamily::Semantic, false),
                "{err}"
            );
            for mode in [Mode::Table, Mode::Json] {
                assert_eq!(report_open_error("", &err, &mode), 1, "{err} in {mode:?}");
            }
        }
        let locked = classify_open(&E::Locked("held".into()));
        assert_eq!(locked.class.exit_code(), 5);
    }

    #[test]
    fn progress_counts_what_completed_what_is_durable_and_what_never_ran() {
        let p = |statement, statements, in_transaction| Progress {
            at: Position::new(statement, statements),
            in_transaction,
        };
        // The first statement fails: nothing completed, the rest never ran.
        let first = p(1, 3, false);
        assert_eq!(
            first.line(),
            "statement 1 of 3 failed; 0 completed and committed; 2 not run"
        );
        // The last fails: everything before it completed, nothing is left.
        let last = p(3, 3, false);
        assert_eq!(
            last.line(),
            "statement 3 of 3 failed; 2 completed and committed; 0 not run"
        );
        // In an open transaction nothing is durable, however much completed.
        let open = p(3, 4, true);
        assert_eq!(
            open.line(),
            "statement 3 of 4 failed; 2 completed, not committed (open transaction); 1 not run"
        );
        assert_eq!(
            (open.completed(), open.committed(), open.not_run()),
            (2, 0, 1)
        );
        assert_eq!(p(2, 3, false).committed(), 1);
        // An open transaction is reported as such even when nothing completed
        // yet, so the wording never claims a commit that did not happen.
        assert_eq!(
            p(1, 2, true).line(),
            "statement 1 of 2 failed; 0 completed, not committed (open transaction); 1 not run"
        );
    }

    #[test]
    #[should_panic(expected = "statement 0 of 3")]
    fn a_position_before_the_first_statement_is_rejected() {
        let _ = Position::new(0, 3);
    }

    #[test]
    #[should_panic(expected = "statement 4 of 3")]
    fn a_position_past_the_last_statement_is_rejected() {
        let _ = Position::new(4, 3);
    }

    #[test]
    fn envelope_carries_progress_only_for_a_multi_statement_run() {
        let err = E::syntax("x");
        let plain = envelope_value(&err, None);
        for key in ["statement", "statements", "committed"] {
            assert!(
                plain["error"].get(key).is_none(),
                "{key} on a single statement"
            );
        }
        let with = Progress {
            at: Position::new(2, 3),
            in_transaction: false,
        };
        let v = envelope_value(&err, Some(with));
        assert_eq!(v["error"]["statement"], 2);
        assert_eq!(v["error"]["statements"], 3);
        assert_eq!(v["error"]["committed"], 1);
        // The ADR-0194 fields are untouched.
        assert_eq!(v["error"]["code"], "validation_error");
        assert_eq!(v["error"]["exit_code"], 6);
        let open = Progress {
            in_transaction: true,
            ..with
        };
        let v = envelope_value(&err, Some(open));
        assert_eq!(v["error"]["committed"], 0);
    }

    #[test]
    fn envelope_is_one_sanitized_line() {
        let json = envelope_json(&E::semantic("a\nb\u{1b}[31m"), None);
        assert_eq!(json.lines().count(), 1, "{json}");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["error"]["message"], "semantic error: a b[31m");
    }

    #[test]
    fn report_returns_the_class_exit_code_in_every_mode() {
        for mode in [
            Mode::Table,
            Mode::Csv,
            Mode::Markdown,
            Mode::Cypher,
            Mode::Json,
            Mode::Jsonl,
            Mode::Envelope,
        ] {
            for (err, expected) in [(E::syntax("x"), 6), (E::conflict(), 5), (E::io("x"), 1)] {
                let mut out = Vec::new();
                let mut diagnostics = Vec::new();
                assert_eq!(
                    report_query_error_to(
                        "",
                        &err,
                        &mode,
                        ReportCtx::NONE,
                        &mut out,
                        &mut diagnostics
                    ),
                    expected,
                    "{err} in {mode:?}"
                );
                // Every report went to the writers the caller supplied: the
                // human line on the diagnostic sink, and the envelope there
                // too when the mode is JSON-family.
                assert!(
                    !diagnostics.is_empty(),
                    "{err} in {mode:?}: the human line is reported"
                );
                assert_eq!(
                    !out.is_empty(),
                    mode.is_json(),
                    "{err} in {mode:?}: only a JSON-family mode carries the envelope"
                );
            }
        }
    }
}
