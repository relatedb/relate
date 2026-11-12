//! The statement-execution cluster (ADR-0104): the `Exec` session bundle,
//! the `run_relate` dispatch, the per-statement exec helpers, and the
//! interactive tails (`run_repl`, `run_import_interactive`) with the ADR-0164
//! open-transaction exit policy and the ADR-0402 zero-effect write notes.
//! `main.rs` stays thin parse-and-dispatch, and the run-and-render seam of one
//! statement — [`crate::exec_query`] — stays out of this module: the session
//! owns what surrounds a run (dot-command dispatch, the ledger, the exit
//! policy), that module owns the run and its reports.
//!
//! The ADR-0402 criterion is the engine's: the executor stamps a
//! [`relate::StatementEffect`] on every executed result (a mutation
//! operator reached execution, a change was applied), and `zero_effect` owns
//! the CLI half — batch-only, a plan artifact suppressing only its own note,
//! a failed run taking no note, and the pinned wording.

use std::collections::HashMap;
use std::io::{self, Read};

use relate::StatementEffect;
use relate_formatter::{Formatter, Mode};
use relate_model::CypherValue;
use relate_splitter::split_statements;

use crate::eggs;
use crate::exec_query::{self, RenderOpts};
use crate::VERSION;
use crate::{errors, exit_flushed, print_error, repl, telemetry, zero_effect};

/// The final-checkpoint policy shared by [`exit_closed`] and the extension
/// persist path: a persistent, writable database checkpoints before exit; a
/// checkpoint failure is reported and forces a non-zero exit code.
pub(crate) fn final_checkpoint(
    db: &relate::Database,
    storage: telemetry::StorageKind,
    code: i32,
) -> i32 {
    if storage.is_persistent() && !db.is_read_only() {
        if let Err(e) = db.checkpoint() {
            print_error(format_args!("Error: final checkpoint failed: {e}"));
            return code.max(1);
        }
    }
    code
}

/// Printed to stderr when the process exits with an open `.begin`
/// transaction (ADR-0164). Byte-for-byte the pre-579592c9 text: wrapper
/// scripts grep stderr for `rolling back`, so the wording is an observable
/// contract, not prose.
const OPEN_TRANSACTION_WARNING: &str = "Warning: exiting with an open transaction; rolling back uncommitted changes. Use .commit before exiting to persist them.";

/// The ADR-0164 open-transaction exit contract: an open `.begin`
/// transaction at exit is discarded (never checkpointed), so warn on stderr
/// before the final checkpoint. Batch paths (one-shot, piped stdin) force
/// the exit code to 1 without overriding a higher code already earned by a
/// failed statement; interactive exit keeps its code — discarding on
/// connection drop is the documented ADR-0028 behavior there, not an error.
/// The `source` is the invocation-mode trichotomy `main` already classified
/// as `telemetry::QuerySource`; the exit policy reads it directly instead of
/// re-probing `is_terminal()` on the exit path.
fn warn_open_transaction(
    conn: &relate::Connection,
    code: i32,
    source: telemetry::QuerySource,
) -> i32 {
    if !conn.in_transaction() {
        return code;
    }
    print_error(OPEN_TRANSACTION_WARNING);
    // No catch-all: the match enumerates the closed QuerySource taxonomy, so
    // a new variant fails to compile until the exit policy classifies it.
    // `is_batch` is itself an exhaustive match over the closed QuerySource
    // taxonomy, so a new variant fails to compile until it is classified.
    if source.is_batch() {
        code.max(1)
    } else {
        code
    }
}

/// Close the database, then exit through [`exit_flushed`]. `process::exit`
/// skips the engine's persist-on-drop checkpoint, so a persistent database
/// checkpoints here; that pins the `--snapshot-format` codec into
/// manifest.json. An open `.begin` transaction is never checkpointed, so its
/// writes are discarded; [`warn_open_transaction`] reports that discard
/// (ADR-0165) before the checkpoint runs.
fn exit_closed(exec: &mut Exec<'_>, code: i32) -> ! {
    let code = warn_open_transaction(&exec.conn, code, exec.query_ctx.source);
    // Memory-vs-persistent was classified once, in `main` (or in the
    // bootstrap), as `query_ctx.storage`; consult it here rather than
    // re-deriving it from the database path.
    let code = final_checkpoint(exec.db, exec.query_ctx.storage, code);
    exit_flushed(exec.telemetry, code)
}

/// The interactive tail shared by the plain shell and the extension
/// (ADR-0279): print the banner, then run the prompt loop. Returns the REPL
/// exit code; the caller owns the final checkpoint and the process exit.
fn run_repl(exec: &mut Exec<'_>) -> i32 {
    let banner = format!("Relate {VERSION}. End statements with ;  (.quit to exit)");
    let human = exec.query_ctx.is_human_repl();
    if !eggs::print_banner(human, &banner) {
        println!("{banner}");
    }
    repl::run_interactive(&eggs::prompt_style(human), |input| {
        exec_relate_input(exec, input);
    })
}

/// The interactive-tail decision every import-run lifecycle shares
/// (ADR-0286): only an omitted store target — the `:memory:` default,
/// whose graph disappears with the process — opens the shell, and only
/// on a real TTY with `repl` compiled in. An explicit `-d`/`--database`
/// (`Some`) names a target the caller keeps, so that invocation prints
/// its summary and exits.
///
/// The extension host's decision (ADR-0430 §7) — one policy, owned here
/// since the compiled-in importers retired with the `lab` surface.
#[cfg(feature = "extensions")]
pub(crate) fn opens_interactive_shell(db: Option<&str>, tty: bool) -> bool {
    db.is_none() && tty && cfg!(feature = "repl")
}

/// Open the interactive shell on an already-populated import database
/// (ADR-0279, ADR-0286): the extension host's interactive handoff — the default in-memory run's graph is queried
/// before the process exits and it disappears. Bootstraps the shared REPL
/// ([`run_repl`]) and returns its exit code instead of exiting, so the
/// command layers own the final checkpoint and the process exit. Only
/// reachable when an importer surface and `repl` are compiled in
/// (ADR-0104).
#[cfg(feature = "extensions")]
pub(crate) fn run_import_interactive(
    db: &relate::Database,
    db_path: &str,
    storage: telemetry::StorageKind,
    telemetry: &mut telemetry::Telemetry,
) -> i32 {
    let conn = match db.connect_typed(relate::Namespace::Neo4j) {
        Ok(c) => c,
        Err(e) => {
            print_error(e);
            return 1;
        }
    };
    let mut exec = Exec::interactive(db, conn, db_path, storage, telemetry);
    let code = run_repl(&mut exec);
    // The importing command layers own their final checkpoint, so warn
    // about an open transaction here, before it runs (ADR-0164).
    // Interactive, so the exit code is unchanged.
    warn_open_transaction(&exec.conn, code, exec.query_ctx.source)
}

/// The invocation-mode trichotomy (ADR-0079 §4), classified exactly once in
/// `main`: a oneshot query argument, piped stdin, or the interactive REPL.
/// `run_relate` dispatches on it, so the oneshot query text travels with
/// its discriminant and the dispatch site needs no re-assertion that the two
/// agree.
#[derive(Clone, Copy)]
pub(crate) enum Session<'a> {
    Oneshot(&'a str),
    Stdin,
    Repl,
}

impl Session<'_> {
    /// The telemetry `RELATE-CLI_QUERY` `source` label for this session
    /// kind; the strings are fixed by ADR-0079 §4.
    pub(crate) fn source(&self) -> telemetry::QuerySource {
        match self {
            Session::Oneshot(_) => telemetry::QuerySource::Oneshot,
            Session::Stdin => telemetry::QuerySource::Stdin,
            Session::Repl => telemetry::QuerySource::Repl,
        }
    }
}

/// Bundled options for [`run_relate`] (avoids `clippy::too_many_arguments`).
pub(crate) struct RunOpts<'a> {
    pub(crate) db_path: &'a str,
    pub(crate) session: Session<'a>,
    pub(crate) params: &'a HashMap<String, CypherValue>,
    pub(crate) mode: Mode,
    pub(crate) no_header: bool,
    pub(crate) pretty: bool,
    /// The `--timings` flag (ADR-0581): the jsonl terminal timing line's
    /// opt-in; off by default, so the jsonl stream is pure rows.
    pub(crate) timings: bool,
    pub(crate) max_rows: Option<usize>,
    /// The total display width the human table wraps to (LABS-399).
    pub(crate) width: Option<usize>,
    /// ADR-0241 per-statement timeout; `None` is no bound.
    pub(crate) timeout: Option<std::time::Duration>,
    /// The engine open configuration, bundled as the type
    /// `Database::open_with` takes rather than carried field-by-field.
    pub(crate) open: relate::OpenOptions,
    pub(crate) namespace: Option<relate::Namespace>,
    pub(crate) telemetry: &'a mut telemetry::Telemetry,
    pub(crate) query_ctx: QueryContext,
}

/// The `RELATE-CLI_QUERY` dimensions that are fixed for a whole CLI run
/// (ADR-0079 §4); only `success` varies per statement.
#[derive(Clone, Copy)]
pub(crate) struct QueryContext {
    pub(crate) source: telemetry::QuerySource,
    pub(crate) storage: telemetry::StorageKind,
    /// The resolved agent mode (ADR-0407), `true` when on.
    pub(crate) agent_mode: bool,
}

impl QueryContext {
    /// The interactive REPL with a human at it: the one fact the hidden
    /// client-side commands gate on (never agent mode).
    fn is_human_repl(&self) -> bool {
        self.source == telemetry::QuerySource::Repl && !self.agent_mode
    }
}

/// Shared context for executing statements: engine handles, formatting, and
/// telemetry (keeps the exec helpers under the clippy argument limit).
struct Exec<'a> {
    db: &'a relate::Database,
    conn: relate::Connection,
    /// The database path as opened, for `.info`'s cold store read (ADR-0165).
    db_path: &'a str,
    params: std::collections::BTreeMap<String, relate::Value>,
    formatter: Formatter,
    /// The row cap for statement output (ADR-0196); `None` is unlimited.
    max_rows: Option<usize>,
    /// The per-statement timeout (ADR-0241); `None` is no bound.
    timeout: Option<std::time::Duration>,
    /// The `.timer` display state; default OFF (SQLite behavior).
    timer: bool,
    telemetry: &'a mut telemetry::Telemetry,
    query_ctx: QueryContext,
    /// The engine effect of every successful statement run since the last
    /// transaction command, folded with [`StatementEffect::merge`], for the
    /// ADR-0402 commit summary: a plan-producing `PROFILE` folds in too (it
    /// executed), so the summary reads the same facts the per-statement notes
    /// did. `.begin` / `.commit` / `.rollback` are its only reset, so an
    /// autocommit statement outside a transaction cannot reach a later
    /// summary.
    txn_ledger: StatementEffect,
}

impl Exec<'_> {
    /// Interactive-shell bootstrap for the import handoffs (ADR-0279,
    /// ADR-0430 §7): no parameters, table output, REPL telemetry source.
    /// The plain shell builds its `Exec` inline instead.
    #[cfg(feature = "extensions")]
    fn interactive<'a>(
        db: &'a relate::Database,
        conn: relate::Connection,
        db_path: &'a str,
        storage: telemetry::StorageKind,
        telemetry: &'a mut telemetry::Telemetry,
    ) -> Exec<'a> {
        Exec {
            db,
            conn,
            db_path,
            params: std::collections::BTreeMap::new(),
            formatter: Formatter::new(Mode::Table),
            max_rows: None,
            timeout: None,
            timer: false,
            telemetry,
            query_ctx: QueryContext {
                source: telemetry::QuerySource::Repl,
                storage,
                // The extension host has no agent-mode flag of its own; the
                // env ladder decides, and an invalid value fails closed
                // (agent mode) rather than guessing a human.
                agent_mode: crate::agent_mode::effective_agent_mode_from_env(None, true)
                    .map_or(true, |m| m.is_on()),
            },
            txn_ledger: StatementEffect::None,
        }
    }

    /// Emit `RELATE-CLI_QUERY` for one executed statement. `&mut self`:
    /// the live handle's `emit` takes the owning thread's `&mut` (its
    /// backlog is plainly owned), and this method's only caller holds
    /// `&mut exec`.
    fn emit_query(&mut self, success: bool) {
        self.telemetry.emit(telemetry::Event::Query {
            success,
            source: self.query_ctx.source,
            storage: self.query_ctx.storage,
            // Derived from the formatter at emit time, so telemetry and
            // the formatter can never disagree.
            output_mode: telemetry::OutputMode::from(&self.formatter.mode),
        });
    }
}

pub(crate) fn run_relate(opts: RunOpts<'_>) -> ! {
    let telemetry = opts.telemetry;
    // LABS-903: the open mode this run opens with — the key the post-open
    // release rule below reads (`OpenOptions` itself moves into the open).
    let open_mode = opts.open.mode;
    let db = match relate::Database::open_with(opts.db_path, opts.open) {
        Ok(d) => d,
        Err(e) => {
            let code = errors::report_open_error("Failed to open Relate: ", &e, &opts.mode);
            exit_flushed(telemetry, code);
        }
    };
    // LABS-903: an ephemeral open — the `--load` memory turn-into —
    // releases the source after loading it, so a remote download's scratch
    // copy is unreferenced the moment the open returns — remove it then
    // instead of at exit. (The engine closed the host handle, so this
    // succeeds on Windows too.) The rule is `load`'s one statement, keyed
    // on the mode this run opened with — the same key `serve`'s post-open
    // site uses.
    crate::load::release_source_after_open(open_mode);
    let namespace = opts.namespace.unwrap_or(relate::Namespace::Neo4j);
    let conn = match db.connect_typed(namespace) {
        Ok(c) => c,
        Err(e) => {
            let code = errors::report_query_error("", &e, &opts.mode, errors::ReportCtx::NONE);
            exit_flushed(telemetry, code);
        }
    };
    let mapped: std::collections::BTreeMap<String, relate::Value> = opts
        .params
        .iter()
        .map(|(k, v)| (k.clone(), relate::cypher_to_value(v)))
        .collect();
    let mut formatter = Formatter::new(opts.mode);
    if opts.no_header {
        formatter.headers = false;
    }
    formatter.pretty = opts.pretty;
    formatter.width = opts.width;
    // ADR-0581: the jsonl terminal timing line's opt-in — one run-level
    // fact on the formatter (the `pretty` shape), off by default, so the
    // piped default's stream is pure rows.
    formatter.render_timings = opts.timings;
    // The ADR-0164 exit policy reads `query_ctx.source` directly: the
    // invocation-mode trichotomy `main` already classified as a
    // `telemetry::QuerySource`.
    let mut exec = Exec {
        db: &db,
        conn,
        db_path: opts.db_path,
        params: mapped,
        formatter,
        max_rows: opts.max_rows,
        timeout: opts.timeout,
        // `.timer` display state; default OFF (SQLite behavior, and the pre-merge
        // shell's default in relate-repl/src/terminal.rs).
        timer: false,
        telemetry,
        query_ctx: opts.query_ctx,
        txn_ledger: StatementEffect::None,
    };

    // The session kind was classified once in `main`; dispatch on it
    // directly — a `Session::Oneshot` carries its query text.
    match opts.session {
        Session::Oneshot(q) => {
            let code = exec_relate_input(&mut exec, q);
            exit_closed(&mut exec, code);
        }
        Session::Stdin => {
            let mut input = String::new();
            // A read error here — including invalid UTF-8 — is fatal:
            // executing a partial or mangled script silently would be worse
            // than failing. Report it and exit non-zero, executing nothing.
            if let Err(e) = io::stdin().read_to_string(&mut input) {
                print_error(format!("Error: could not read query input from stdin: {e}"));
                exit_closed(&mut exec, 1);
            }
            let code = exec_relate_input(&mut exec, &input);
            exit_closed(&mut exec, code);
        }
        Session::Repl => {
            let code = run_repl(&mut exec);
            exit_closed(&mut exec, code);
        }
    }
}

/// Execute one or more statements / slim REPL commands.
/// The batch-vs-interactive bit derives from the session kind `main`
/// classified once (the `QuerySource` the `Exec` bundle already carries),
/// not a hand-threaded bool: batch sessions (one-shot and piped stdin)
/// fail fast — the first failure stops the batch and returns 1 — and only
/// such a run reports its progress (ADR-0199): an interactive line keeps
/// going after a failure, so "N not run" would not be true there.
fn exec_relate_input(exec: &mut Exec<'_>, input: &str) -> i32 {
    // `is_batch` is itself an exhaustive match over the closed QuerySource
    // taxonomy (like warn_open_transaction), so a new variant fails to
    // compile until the batch policy classifies it.
    let fail_fast = exec.query_ctx.source.is_batch();
    let mut code = 0;
    let mut statements = split_statements(input);
    statements.retain(|s| !s.trim().is_empty());
    let total = statements.len();
    for (i, stmt) in statements.iter().enumerate() {
        let at = (fail_fast && total > 1).then(|| errors::Position::new(i + 1, total));
        let stmt_code = exec_relate_stmt(exec, stmt.trim(), at);
        if stmt_code != 0 {
            code = stmt_code;
            if fail_fast {
                return code;
            }
        }
    }
    code
}

fn exec_relate_stmt(exec: &mut Exec<'_>, stmt: &str, at: Option<errors::Position>) -> i32 {
    let cmd = stmt.split_whitespace().next().unwrap_or("");
    match cmd {
        ".quit" | ".exit" | "EXIT" | "exit" => {
            exit_closed(exec, 0);
        }
        ".help" => {
            exec.telemetry.emit(telemetry::Event::Help);
            println!("  Cypher statement ending with ;");
            println!("  .begin / .commit / .rollback / .checkpoint");
            println!("  .info");
            println!("  .timer on|off");
            println!("  .quit");
            0
        }
        ".timer" => {
            match stmt.split_whitespace().nth(1) {
                Some("on") => {
                    exec.timer = true;
                    println!("Timer: on");
                }
                Some("off") => {
                    exec.timer = false;
                    println!("Timer: off");
                }
                _ => println!("Usage: .timer on|off"),
            }
            0
        }
        ".begin" => exec.begin(),
        ".commit" => exec.commit(),
        ".rollback" => exec.rollback(),
        ".checkpoint" => report(exec.db.checkpoint()),
        ".info" => exec.info(),
        other if other.starts_with('.') => {
            if let Some(code) = eggs::try_dispatch(
                stmt,
                exec.db,
                &mut exec.conn,
                exec.query_ctx.is_human_repl(),
            ) {
                return code;
            }
            print_error(format_args!("Error: unknown command {other}"));
            1
        }
        _ => {
            let code = exec.run_query(stmt, at);
            exec.emit_query(code == 0);
            code
        }
    }
}

/// A `.`-command's `Result<()>` to exit-code policy, written once: 0 on
/// success, the error on stderr and 1 on failure.
fn report(result: relate::Result<()>) -> i32 {
    result.map(|_| 0).unwrap_or_else(|e| {
        print_error(e);
        1
    })
}

impl Exec<'_> {
    /// `.begin` (ADR-0028) with the ADR-0402 ledger opened with it: the
    /// commit summary counts only statements this transaction ran. A failed
    /// `begin` leaves the live transaction's ledger alone.
    fn begin(&mut self) -> i32 {
        let code = report(self.conn.begin());
        self.reset_ledger(code);
        code
    }

    /// `.commit` with the ADR-0402 Decision 4 summary: a commit that closed a
    /// transaction which ran a write but applied nothing says so on stderr.
    /// A failed commit keeps its error line and exit code, so no summary.
    fn commit(&mut self) -> i32 {
        // ADR-0502 §7: the engine commit returns the transaction's
        // bookmark; the shell's `.commit` reports success only — no
        // bookmark echo is printed.
        let code = report(self.conn.commit().map(|_| ()));
        if let Some(note) = zero_effect::commit_note(code, self.query_ctx.source, self.txn_ledger) {
            print_error(note);
        }
        self.reset_ledger(code);
        code
    }

    /// `.rollback` — the discard was asked for, so no summary (ADR-0402
    /// Decision 4); only the ledger resets.
    fn rollback(&mut self) -> i32 {
        let code = report(self.conn.rollback());
        self.reset_ledger(code);
        code
    }

    /// The one ledger reset for the three transaction commands: a successful
    /// `.begin`, `.commit` or `.rollback` starts a fresh ADR-0402 ledger, and
    /// a failed one leaves the live transaction's ledger alone. Every
    /// statement folds in as it runs, so this reset is the only place a
    /// statement's effect leaves the ledger — no in-transaction guard
    /// duplicates it.
    fn reset_ledger(&mut self, code: i32) {
        if code == 0 {
            self.txn_ledger = StatementEffect::None;
        }
    }

    /// Run one statement and render it under the session's knobs (row cap,
    /// timeout, `.timer`), forwarding the bundle's fields so the dispatcher
    /// arm does not re-thread them. `at` is the batch statement's position
    /// (ADR-0199), absent for a single statement and in the shell.
    fn run_query(&mut self, query: &str, at: Option<errors::Position>) -> i32 {
        let progress = at.map(|at| errors::Progress {
            at,
            in_transaction: self.conn.in_transaction(),
        });
        let run = exec_query::exec_relate_query(
            &mut self.conn,
            query,
            &self.params,
            &self.formatter,
            RenderOpts {
                max_rows: self.max_rows,
                timeout: self.timeout,
                progress,
                timer: self.timer,
            },
        );
        let code = run.code;
        if let Some(note) =
            zero_effect::note_run(&mut self.txn_ledger, self.query_ctx.source, &run, at)
        {
            print_error(note);
        }
        code
    }

    /// `.info` (ADR-0165 Decision 4), forwarded with the bundle's fields so
    /// the dispatcher arm does not reach into `Exec`'s layout.
    fn info(&mut self) -> i32 {
        crate::dot_info::run_info(
            &mut self.conn,
            self.db_path,
            &self.params,
            &self.formatter,
            self.query_ctx.storage,
        )
    }
}

#[cfg(test)]
mod warn_open_transaction_tests {
    use super::*;
    use telemetry::QuerySource;

    /// The ADR-0164 open-transaction exit decision table. The warning text
    /// itself is pinned byte-for-byte by the e2e tests; here only the exit
    /// code matters (the stderr write is harmless in a unit test).
    #[test]
    fn warn_open_transaction_decision_table() {
        let db = relate::Database::open(":memory:").unwrap();
        let mut conn = db.connect_typed(relate::Namespace::Neo4j).unwrap();

        // No open transaction: the incoming code passes through in every mode.
        assert_eq!(warn_open_transaction(&conn, 0, QuerySource::Oneshot), 0);
        assert_eq!(warn_open_transaction(&conn, 0, QuerySource::Stdin), 0);
        assert_eq!(warn_open_transaction(&conn, 0, QuerySource::Repl), 0);

        conn.begin().unwrap();

        // Open transaction: batch paths force at least 1...
        assert_eq!(warn_open_transaction(&conn, 0, QuerySource::Oneshot), 1);
        assert_eq!(warn_open_transaction(&conn, 0, QuerySource::Stdin), 1);
        // ...interactive keeps the incoming code...
        assert_eq!(warn_open_transaction(&conn, 0, QuerySource::Repl), 0);
        // ...and a higher already-earned code is never lowered.
        assert_eq!(warn_open_transaction(&conn, 2, QuerySource::Oneshot), 2);
        assert_eq!(warn_open_transaction(&conn, 2, QuerySource::Stdin), 2);
        assert_eq!(warn_open_transaction(&conn, 2, QuerySource::Repl), 2);
    }
}

/// The interactive-handoff decision every import-run lifecycle shares
/// (ADR-0286) — moved here with the decision when the extension host
/// became its caller.
#[cfg(all(test, feature = "extensions"))]
mod opens_interactive_shell_tests {
    use super::opens_interactive_shell;

    #[test]
    fn explicit_db_never_opens_the_shell() {
        assert!(!opens_interactive_shell(Some("./graph"), true));
        assert!(!opens_interactive_shell(Some("./graph"), false));
        assert!(!opens_interactive_shell(Some(":memory:"), true));
    }

    #[test]
    fn omitted_db_opens_the_shell_only_on_a_tty_with_repl() {
        assert_eq!(opens_interactive_shell(None, true), cfg!(feature = "repl"));
        assert!(!opens_interactive_shell(None, false));
    }
}
