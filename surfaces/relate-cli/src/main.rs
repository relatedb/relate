//! Relate CLI entry point.

use std::io::{self, IsTerminal};

use flags::LifecycleCommand;
use relate_formatter::{sanitize_error_text, Mode};

#[cfg(feature = "agent-context")]
mod agent_context_cmd;
mod agent_mode;
mod agent_vars;
// The shared atomic-write + staging-name kit: the telemetry/upgrade writes,
// and the remote `--load` source's staged file (LABS-872). The `staging`
// marker feature is what those three list, so the kit's gate is one word
// instead of a three-way disjunction (the `serve`/`surface` precedent).
#[cfg(feature = "staging")]
mod atomic_file;
mod clone_cmd;
// The config directory (ADR-0079 §3's resolution): shared by telemetry's
// state walk and the extension registry's discovery (ADR-0429 §6) —
// the eggs' first-run banner marker (ADR-0543) — compiled exactly when
// one of them is.
#[cfg(any(feature = "telemetry", feature = "extensions", feature = "eggs"))]
mod config_dir;
mod dot_info;
mod eggs;
mod errors;
mod examples;
mod exec_query;
// The usage(2)/runtime(1) exit split, stated once for every typed error
// carrier (round 4, F-2-12): `--load`'s errors and the extension install
// flow's classify through the same class, each adding only its own
// message prefix.
mod exit_class;
// The `extension` lifecycle verbs (ADR-0431): `extension init|install|list|
// remove`. `init`/`list`/`remove` are dependency-free; `install` (and
// `list --available`) ride the `extension-install` feature inside the
// extensions tree.
#[cfg(feature = "extensions")]
mod extension_cmd;
// `relate extension init` (ADR-0431 §3): the language templates the
// scaffold writes — generated MCP clients, no published dependency —
// dependency-free beside the verb.
#[cfg(feature = "extensions")]
mod extension_init;
// The extension system (ADR-0429, ADR-0430): the wire protocol, the
// registry, the provenance marker, and — behind `extension-install` —
// the distribution half (ADR-0431: the signed channel, the unpacker,
// the install flow). `full` + `agent` bundles (ADR-0429 §8).
#[cfg(feature = "extensions")]
mod extensions;
#[cfg(feature = "fetch-params")]
mod fetch_params;
mod flags;
mod help;
#[cfg(feature = "info")]
mod info_cmd;
mod params;
mod parse_error;
// The CLI's one plaintext-transport vocabulary (LABS-872): the
// authority→host extraction and the loopback predicates every `http://` gate
// states its policy in terms of.
mod plaintext;
#[cfg(feature = "pull")]
mod pull_cmd;
#[cfg(feature = "push")]
mod push_cmd;
// `--load SOURCE` (LABS-903, ADR-0500): the two-axis store grammar's source
// half — the memory turn-into and the durable-target load — resolved for
// the query session and `serve` alike (the remote fetch is `remote_db`'s).
mod load;
// The remote `--load` source (LABS-872, ADR-0496): the remote `.rl` bundle —
// parsed and refused in every build, fetched only with the
// `remote-database` feature.
mod remote_db;
mod repl;
mod row_cap;
// The staging-directory convention (the `skill install` staging shape,
// ADR-0261, generalized by ADR-0431 §4 step 4): shared by the skills-dir
// installer and the extension installer's XDG dir, and consulted by the
// extension registry's walk (staged siblings are never packages).
#[cfg(any(feature = "skill", feature = "extensions"))]
mod staging;
mod width;
// The ADR-0402 zero-effect write policy — the effect model, its gate and the
// two byte-pinned notes — which `session` only wires.
mod zero_effect;
// `relate serve` (ADR-0232) compiles with the `serve` feature, which
// every network-surface feature implies; each surface flag inside is
// gated on its own feature.
#[cfg(feature = "serve")]
mod serve_args;
#[cfg(feature = "serve")]
mod serve_cmd;
mod session;
// The one capped HTTP GET every CLI-process fetch rides (`--fetch-params`
// sources and the remote `--database` bundle, LABS-872): compiled when
// either capability is.
#[cfg(any(feature = "fetch-params", feature = "remote-database"))]
mod url_fetch;
// The one `StoreInfo` projection every info surface renders (ADR-0165);
// ungated because `.info` compiles in every build.
mod store_info_render;
mod suggest;
mod syntax_position;
// The first-party release channel's shared machinery (ADR-0431 §1): the
// one HTTP client, the probe-then-fallback download routes, the GitHub
// API asset route, and the one signing root — shared by `upgrade`
// (ADR-0250, ADR-0358) and `extension install` (ADR-0431). The `agent`
// bundle carries the extension channel without the self-upgrade.
#[cfg(any(feature = "upgrade", feature = "extension-install"))]
mod release_channel;
mod timeout;
// The agent catalog is shared: `skill` installs into it and `upgrade`
// detects it to offer a skill refresh.
#[cfg(any(feature = "skill", feature = "upgrade"))]
#[cfg_attr(not(feature = "skill"), allow(dead_code))]
mod skill;
#[cfg(feature = "skill")]
mod skill_cmd;
// Anonymous usage telemetry (ADR-0079), wired into the entry/exit points
// below. Nothing reaches stdout/stderr and all failures are swallowed.
mod telemetry;
#[cfg(feature = "telemetry")]
mod telemetry_cmd;
#[cfg(feature = "upgrade")]
mod upgrade_cmd;

use session::{run_relate, QueryContext, RunOpts, Session};

/// Single-sourced from the crate's `Cargo.toml` (`version.workspace = true`).
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The license of the `relate` binary (ADR-0432): the name only, never
/// the license text. One const, consulted by both `--version` output shapes.
const LICENSE: &str = "Apache 2.0 (binary)";

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Parse once; the access slot's one resolution (ADR-0480) — the
    // memory turn-into's mode implication included (LABS-903, ADR-0500) —
    // is taken before the destructure moves the fields.
    let flags = flags::parse(&args);

    // The product-rename guard (ADR-0517): a still-set CYPHERLITE_* name
    // fails closed before any env-reading surface (telemetry, agent mode,
    // the open path) can resolve it as unset.
    if let Err(e) = relate::reject_cypherlite_env() {
        print_error(format!("Error: {e}"));
        std::process::exit(1);
    }
    let open_mode = flags.open_mode();
    let flags::Flags {
        db_path,
        access: _,
        query,
        explicit_query,
        mode,
        no_header,
        pretty,
        show_version,
        show_help,
        lifecycle,
        assignments,
        snapshot_format,
        markdown_body_properties,
        wal_dir,
        checkpoint_interval_secs,
        durability_mode,
        logical_graph_size_limit,
        expression_memory_limit,
        concurrent_logical_workspace_limit,
        max_traversal_depth,
        safe,
        max_rows,
        agent,
        width,
        timeout,
        load_csv_dir,
        namespace,
        load,
    } = flags;

    // Telemetry handle (ADR-0079): resolves the opt-out layers; when
    // disabled it touches nothing (no state file, no thread, no network).
    // Constructed after argument parsing, so usage-error exits above need
    // no telemetry; every exit path below flushes before exiting.
    let mut telemetry = telemetry::Telemetry::new();

    // Help wins over --version when both are set: `-h` breaks out of the
    // parse loop the moment it is seen, exactly like the old mid-parse exit.
    if show_help {
        exit_help(&mut telemetry, &help::top_level());
    }

    if show_version {
        telemetry.emit(telemetry::Event::Startup {
            kind: telemetry::StartupKind::Version,
        });
        println!("{}", version_output(mode.as_ref()));
        exit_flushed(&mut telemetry, 0);
    }

    // A query or a pre-verb access slot riding a lifecycle command has
    // already exited inside flags::parse — see `conflict_error`'s doc for
    // the ADR-0306 §6 policy.
    if let Some(cmd) = lifecycle {
        run_lifecycle_command(cmd, &mut telemetry);
    }

    // Stdin terminal-ness is read once per run so the classification below
    // decides on one fact, read at one moment.
    let stdin_is_tty = is_terminal();

    // The invocation is classified exactly once, here (ADR-0231): a bare
    // form on piped or redirected stdin prints help and exits, so a
    // scripted or harness-driven bare invocation never blocks; every other
    // shape is the `Session` `run_relate` dispatches on. The telemetry
    // `QuerySource` label is derived from that same `Session`, so the
    // dispatch and the label can never disagree.
    let session = match classify_session(query.as_deref(), explicit_query, stdin_is_tty) {
        Some(session) => session,
        None => exit_help(&mut telemetry, &help::top_level()),
    };

    // stdout's terminal-ness is read once per run, before the
    // resolutions that need it (the agent-mode ladder's gate, the output
    // mode, the pretty default) — one fact, one moment (the stdin
    // classification above reads its own once, for the same reason).
    let stdout_is_tty = io::stdout().is_terminal();

    // The env var is the default for `--logical-graph-size-limit`, resolved
    // only once help, version, and lifecycle commands have exited. A bad
    // value takes the same exit-2 path as a bad flag — the one resolve-or-exit
    // scaffold every post-parse resolution rides (ADR-0407's round:
    // one convention, no per-site exiting wrappers).
    let logical_graph_size_limit = resolve_or_exit(
        flags::logical_graph_size_limit_from_env_checked(logical_graph_size_limit),
        &mut telemetry,
        2,
    );
    // ADR-0463: the per-allocation expression cap. Unset keeps the engine
    // default; an explicit none/0 disables it. A bad value exits 2 before
    // the store opens, like the other memory knobs.
    let expression_memory_limit = resolve_or_exit(
        flags::expression_memory_limit_from_env_checked(expression_memory_limit),
        &mut telemetry,
        2,
    );
    // ADR-0452: the workspace pool is the same kind of open knob. A positive
    // value turns admission on; unset leaves it off. A bad value exits 2
    // before the store opens, the same path as the live-graph cap.
    let concurrent_logical_workspace_limit = resolve_or_exit(
        flags::concurrent_logical_workspace_limit_from_env_checked(
            concurrent_logical_workspace_limit,
        ),
        &mut telemetry,
        2,
    );
    // LABS-610: the traversal-depth admission limit is the same kind of open
    // knob. Unset is the engine default (500); a bad flag or env value exits
    // 2 before the store opens.
    let max_traversal_depth = resolve_or_exit(
        flags::max_traversal_depth_from_env_checked(max_traversal_depth),
        &mut telemetry,
        2,
    );
    // LABS-680 (ADR-0477): safe mode is the same kind of open knob —
    // `--safe`/`--no-safe`, then `RELATE_SAFE_MODE=1/0`, then off. A bad
    // environment value exits 2 before the store opens.
    let safe_mode = resolve_or_exit(flags::safe_mode_from_env_checked(safe), &mut telemetry, 2);
    // ADR-0470: the commit durability policy is the same kind of open knob.
    // Unset is the engine default (full); a bad flag or env value exits 2
    // before the store opens.
    let durability_mode = resolve_or_exit(
        flags::durability_mode_from_env_checked(durability_mode),
        &mut telemetry,
        2,
    );
    // The agent mode (ADR-0407) resolves here for the same reason: a bad
    // RELATE_AGENT_MODE exits 2 only on a run that executes statements.
    // One resolution feeds its three consumers — the announcement, the row
    // cap's default, and the piped output-mode default (ADR-0426) — so they
    // can never disagree.
    let agent_mode = resolve_or_exit(
        agent_mode::effective_agent_mode_from_env(agent, stdout_is_tty),
        &mut telemetry,
        2,
    );
    // ADR-0407: the one-line announcement — the deliberate acts confirm
    // themselves; detection stays silent (a per-command CLI never repeats
    // a banner). The mode owns the line whole; `main` only prints it.
    if let Some(line) = agent_mode.announcement() {
        print_error(line);
    }
    // The row cap (ADR-0196) resolves from the decided mode: a bad
    // RELATE_MAX_ROWS exits 2 only on a run that executes statements.
    let max_rows = resolve_or_exit(
        row_cap::effective_max_rows_from_env(max_rows, agent_mode.is_on()),
        &mut telemetry,
        2,
    );

    // Resolve parameter assignments now that every no-side-effect exit
    // (--help, --version, lifecycle) is done. See params::resolve for the
    // last-assignment-wins and error contracts. Fetch/parse failures are
    // runtime errors (exit 1).
    let params = resolve_or_exit(params::resolve(assignments), &mut telemetry, 1);

    // LABS-903 (ADR-0500): the two-axis store grammar resolves into the
    // store to open — `--load`'s source against `--database`'s target —
    // by its own module, so `main` stays dispatch-only. The fetch is a
    // runtime error (exit 1); a refused value shape, safe mode, or an
    // existing durable target is a usage error (exit 2). A durable-target
    // load creates the store with the run's codec knobs (R-M2), so the
    // created store and the open below agree.
    // The flag's one open projection (ADR-0534): the codec it requests
    // and the wide-column opt-in, for the load-create path below and the
    // open after it alike.
    let (snapshot_codec, wide_columns) = match snapshot_format {
        Some(format) => {
            let (codec, wide) = format.open_knobs();
            (Some(codec), wide)
        }
        None => (None, false),
    };
    let db_path = resolve_store(
        db_path,
        load.as_deref(),
        safe_mode,
        load::CreateCodecs {
            // The durable-target create path names a codec; the
            // wide-column opt-in is the open's, applied below.
            to_snapshot_format: snapshot_codec,
            markdown_body_properties: markdown_body_properties.clone(),
        },
        &mut telemetry,
    );

    telemetry.emit(telemetry::Event::Startup {
        kind: telemetry::StartupKind::from(session.source()),
    });
    // First-run notice (ADR-0079 §6): emitted on the first run where
    // telemetry is enabled, regardless of whether stdin is a terminal.
    // `claim_first_run_notice` returns false (and prints nothing) when
    // telemetry is opted out or the notice was already claimed.
    if telemetry.claim_first_run_notice() {
        eprintln!("{FIRST_RUN_NOTICE}");
    }

    // Resolve the output mode exactly once so telemetry and the formatter can
    // never disagree (ADR-0107). An explicit flag always wins; otherwise the
    // default depends on stdout's terminal-ness — read once, above, with
    // the resolutions that needed it first.
    let output_mode_choice =
        mode.unwrap_or_else(|| default_mode(stdout_is_tty, agent_mode.is_on()));
    run_relate(RunOpts {
        db_path: &db_path,
        session,
        params: &params,
        mode: output_mode_choice,
        no_header,
        // ADR-0240: indented JSON is a terminal courtesy; piped or
        // redirected stdout gets one compact line unless --pretty asks.
        pretty: pretty || stdout_is_tty,
        max_rows,
        // LABS-399: an explicit --width wins; otherwise the grid wraps to
        // the terminal width (COLUMNS) on a TTY — a piped run keeps the
        // natural width so its output stays stable.
        width: width::effective_width(width),
        timeout,
        open: relate::OpenOptions {
            // ADR-0534: the opt-in half of the flag's projection; the
            // created-by-load target takes the codec half above.
            wide_columns,
            wal_dir,
            checkpoint_interval: checkpoint_interval_secs.map(std::time::Duration::from_secs),
            durability_mode,
            logical_graph_size_limit,
            expression_memory_limit,
            snapshot_codec,
            markdown_body_properties,
            load_csv_dir,
            load_csv_max_bytes: None,
            mode: open_mode,
            concurrent_logical_workspace_limit,
            max_traversal_depth,
            safe_mode,
            follow_gap: Default::default(),
        },
        namespace,
        telemetry: &mut telemetry,
        query_ctx: QueryContext {
            source: session.source(),
            // The memory-vs-persistent classification happens exactly once,
            // here; `exit_closed` consults `query_ctx.storage` instead of
            // re-deriving it from the path. LABS-903: `for_store` owns the
            // mode-aware mapping — a `--load` memory turn-into IS the
            // in-memory store after the load, whatever path or URL it came
            // from; a durable-target load is the target's persistent store.
            storage: telemetry::StorageKind::for_store(&db_path, open_mode),
            agent_mode: agent_mode.is_on(),
        },
    });
}

/// The `--version` output (ADR-0193, ADR-0432): one compact JSON object with
/// the binary's `license` member in JSON mode; the `Relate v<version>`
/// line plus the `License: <name>` line in every other mode.
fn version_output(mode: Option<&Mode>) -> String {
    if mode == Some(&Mode::Json) {
        serde_json::json!({
            "name": "relate",
            "version": VERSION,
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "license": LICENSE,
        })
        .to_string()
    } else {
        format!("Relate v{VERSION}\nLicense: {LICENSE}")
    }
}

fn is_terminal() -> bool {
    io::stdin().is_terminal()
}

/// The invocation classifier (ADR-0200, ADR-0231): which session the run
/// is, or `None` when the bare form on piped or redirected stdin prints
/// the top-level help instead of running anything. With no statement and
/// no `query` verb, a terminal stdin opens the shell — exactly the session
/// `query` with no STATEMENT runs; a statement (positional or `-c`) is a
/// oneshot either way. Pure so the decision table is unit-testable without
/// a PTY.
fn classify_session(
    query: Option<&str>,
    explicit_query: bool,
    stdin_is_tty: bool,
) -> Option<Session<'_>> {
    match (query, explicit_query, stdin_is_tty) {
        (Some(q), _, _) => Some(Session::Oneshot(q)),
        (None, _, true) => Some(Session::Repl),
        (None, true, false) => Some(Session::Stdin),
        (None, false, false) => None,
    }
}

/// Default output mode when no output flag is given. A terminal is a
/// human's (ADR-0196): the table, whatever the agent mode. A pipe or
/// redirect is a machine's: JSONL for human pipelines (ADR-0107), and —
/// when agent ergonomics are in effect (ADR-0407's ladder: an explicit
/// `--agent-mode`/`RELATE_AGENT_MODE=1` or a detected harness) — the
/// compact tabular `toon` (ADR-0239, flipped per ADR-0426's measured
/// tool-output-bytes win with correctness held). An explicit `--mode`
/// always wins over every default.
fn default_mode(stdout_is_tty: bool, agent_on: bool) -> Mode {
    match (stdout_is_tty, agent_on) {
        (true, _) => Mode::Table,
        (false, true) => Mode::Toon,
        (false, false) => Mode::Jsonl,
    }
}

/// One-time first-run notice (ADR-0079 §6), printed to stderr on the first
/// run where telemetry is enabled (interactive or not).
const FIRST_RUN_NOTICE: &str =
    "relate collects anonymous usage metrics; disable anytime: relate telemetry off";

/// One post-parse resolution's error-exit scaffold: the Err arm prints
/// the shared diagnostic and exits `code` through the flushing exit —
/// spelled once for every resolve-or-exit site (the agent mode, the row
/// cap, the params) instead of a match per resolution.
fn resolve_or_exit<T>(
    result: Result<T, String>,
    telemetry: &mut telemetry::Telemetry,
    code: i32,
) -> T {
    resolve_or_exit_coded(result, telemetry, |_| code)
}

/// The resolve-or-exit scaffold's code-carrying form — the one body the
/// fixed-code form above delegates to (round 4, F-3-1): the error carries
/// its own exit code (and renders itself), so a resolution whose error
/// classes split across codes — [`load::resolve`]'s usage-2 vs runtime-1 —
/// reports through the same one scaffold instead of a hand-rolled exiting
/// match beside it.
fn resolve_or_exit_coded<T, E: std::fmt::Display>(
    result: Result<T, E>,
    telemetry: &mut telemetry::Telemetry,
    code: impl FnOnce(&E) -> i32,
) -> T {
    match result {
        Ok(value) => value,
        Err(e) => {
            print_error(format!("Error: {e}"));
            exit_flushed(telemetry, code(&e));
        }
    }
}

/// The store to open (LABS-903, ADR-0500): `--load`'s source resolved
/// against `--database`'s target by the one shared resolution
/// ([`load::resolve`] — the memory turn-into, the durable-target load, or
/// the plain target passthrough with its remote-URL refusal), reported
/// through the one resolve-or-exit scaffold's code-carrying form: a usage
/// error (exit 2) for a refused value shape, safe mode, or an existing
/// durable target, and a runtime error (exit 1) for the fetch or the
/// clone, like a `--fetch-params` source. The resolved path travels to
/// the open and the storage classification, so neither re-derives it.
/// `create` is the codec knobs a durable-target load creates the store
/// with — the run's `--snapshot-format` and
/// `--markdown-body-properties`, so the created store and the open agree.
fn resolve_store(
    db_path: String,
    load: Option<&str>,
    safe_mode: bool,
    create: load::CreateCodecs,
    telemetry: &mut telemetry::Telemetry,
) -> String {
    resolve_or_exit_coded(
        load::resolve(&db_path, load, safe_mode, create),
        telemetry,
        |e| e.code(),
    )
}

/// Flush pending telemetry events, then exit with `code`. Every exit path
/// after the shared handle is constructed goes through this helper. The
/// flush is one local spool append — never a network wait (ADR-0079 §5,
/// ADR-0545): pending events are delivered by the next enabled run.
fn exit_flushed(telemetry: &mut telemetry::Telemetry, code: i32) -> ! {
    // A remote `--load` source's staged bundle is this process's temp file
    // (LABS-872); every post-fetch exit — success, a failed open, a failed
    // statement — passes through here, and `process::exit` would skip any
    // Drop-based cleanup. Every staged copy is scratch (the turn-into
    // releases it after the load, the durable-target load after the
    // clone), so the exit path removes whatever the load left registered.
    remote_db::cleanup_staged();
    telemetry.flush();
    std::process::exit(code);
}

/// The one help-exit sequence, shared by the top-level `--help` path and a
/// per-command `--help`: `Startup{kind: Help}`, the `RELATE-CLI_HELP`
/// event (ADR-0079 §4), the page, then exit 0 through the shared flush path.
fn exit_help(telemetry: &mut telemetry::Telemetry, page: &str) -> ! {
    telemetry.emit(telemetry::Event::Startup {
        kind: telemetry::StartupKind::Help,
    });
    telemetry.emit(telemetry::Event::Help);
    print!("{page}");
    exit_flushed(telemetry, 0)
}

/// Print a human-facing error or warning to stderr, sanitized by the shared
/// formatter: whitespace control characters (LF, CR, tab) become a single
/// space so multi-line error texts render legibly on one line, and every
/// other unsafe output character (Cc controls + Cf format chars) is
/// stripped.
///
/// Structural rule: EVERY interpolated stderr diagnostic in this binary
/// routes through this function (or its writer form [`write_error`], which a
/// run with a caller-supplied sink uses) — flag-parse errors, usage errors,
/// command errors, and warnings alike. Error text can embed argv,
/// engine-generated, or REPL/stdin-derived strings (a query error may quote
/// part of the statement; a `.foo` token is echoed verbatim), so a crafted
/// value must never reach the terminal raw where it could inject a control
/// sequence or reorder text. The caller supplies any `Error: ...` prefix and
/// it survives untouched. The only raw `eprintln!` calls left in this binary
/// print purely static text, or a message whose interpolated values were
/// already sanitized (e.g. the multi-line skill error — see its call site).
/// New code: if the message interpolates anything, call `print_error`, not
/// `eprintln!`.
pub(crate) fn print_error(msg: impl std::fmt::Display) {
    write_error(&mut io::stderr(), msg);
}

/// [`print_error`]'s writer form: the one sanitized rendering of a
/// human-facing diagnostic, with the sink supplied by the caller. A statement
/// run whose writers are injected (the render-failure test) reports through
/// it instead of the process stderr, and both paths share the sanitizer.
///
/// A failed write is dropped: the sink is the caller's own report channel, so
/// there is no second stream to escalate to, and the run's exit code is the
/// outcome of record.
pub(crate) fn write_error(sink: &mut dyn std::io::Write, msg: impl std::fmt::Display) {
    let _ = writeln!(sink, "{}", sanitize_error_text(&msg.to_string()));
}

/// [`write_error`]'s plain form for a line the CLI generated itself — a
/// timing line or an error envelope: no sanitization (the bytes are the CLI's
/// own), one newline-terminated line to the caller's sink, and the same
/// dropped-on-failure rule.
pub(crate) fn write_report(sink: &mut dyn std::io::Write, line: impl std::fmt::Display) {
    let _ = writeln!(sink, "{line}");
}

/// Run a parsed lifecycle command and exit with its status code: 0 on
/// success, 2 for usage errors (unknown agent) and commands not included in
/// this build, 1 for runtime errors. Emits `RELATE-CLI_COMMAND`
/// (ADR-0079 §4) and flushes before exiting.
fn run_lifecycle_command(cmd: LifecycleCommand, telemetry: &mut telemetry::Telemetry) -> ! {
    // The command word for the RELATE-CLI_COMMAND event, captured before
    // the match consumes cmd.
    let name = cmd.name();
    match cmd {
        // A per-command `--help` (after a compiled-in command word) prints
        // the command's help page and exits 0 through the shared flush path:
        // the same two events as the top-level help, no COMMAND event, and
        // nothing after the word parsed or executed.
        LifecycleCommand::Help { topic } => exit_help(telemetry, &help::command_help(topic)),
        // The resolved action's pre-scanned page (`run <name> --help`):
        // the same help-exit sequence as every command's page — the two
        // help events, no COMMAND event, nothing runs (LABS-552).
        #[cfg(feature = "extensions")]
        LifecycleCommand::RunHelp { page } => exit_help(telemetry, &page),
        LifecycleCommand::NotIncluded { command } => {
            print_error(format!(
                "Error: the '{command}' command is not included in this build of relate"
            ));
            exit_flushed(telemetry, 2);
        }
        #[cfg(feature = "skill")]
        LifecycleCommand::Skill { args } => {
            let action = args.action.as_str();
            let exit_code = skill_cmd::run(args);
            finish_command(telemetry, name, Some(action), exit_code);
        }
        #[cfg(feature = "upgrade")]
        LifecycleCommand::Upgrade { opts } => {
            let action = if opts.check { Some("check") } else { None };
            let exit_code = upgrade_cmd::run(opts);
            finish_command(telemetry, name, action, exit_code);
        }
        #[cfg(feature = "telemetry")]
        LifecycleCommand::Telemetry { action } => {
            // The command owns the handle: `off` disarms it (one ordered
            // transition, ADR-0545 §5) and `status` reports the state
            // this same handle resolved, so env layers override the
            // config file exactly as in Telemetry::new.
            let exit_code = telemetry_cmd::run(action, telemetry);
            finish_command(telemetry, name, Some(action.as_str()), exit_code);
        }
        #[cfg(feature = "pull")]
        LifecycleCommand::Pull { cmd } => {
            let exit_code = pull_cmd::run(cmd);
            finish_command(telemetry, name, None, exit_code);
        }
        #[cfg(feature = "push")]
        LifecycleCommand::Push { cmd } => {
            let exit_code = push_cmd::run(cmd);
            finish_command(telemetry, name, None, exit_code);
        }
        LifecycleCommand::Clone { cmd } => {
            let exit_code = clone_cmd::run(cmd);
            finish_command(telemetry, name, None, exit_code);
        }
        #[cfg(feature = "extensions")]
        LifecycleCommand::Run { cmd } => {
            // ADR-0429 §9: the report says command `run`, action
            // `extension` — never a user-chosen name (ADR-0079
            // anonymity), which is also what keeps the event's
            // `&'static str` fields honest: the installed name is
            // data, not a static.
            let exit_code = crate::extensions::run::run(cmd, telemetry);
            finish_command(telemetry, name, Some("extension"), exit_code);
        }
        #[cfg(feature = "info")]
        LifecycleCommand::Info { cmd } => {
            let exit_code = info_cmd::run(cmd);
            finish_command(telemetry, name, None, exit_code);
        }
        #[cfg(feature = "serve")]
        LifecycleCommand::Serve { cmd } => {
            // `serve` runs until SIGINT/SIGTERM (ADR-0232), then returns the
            // shutdown code; the COMMAND event and exit happen here.
            let exit_code = serve_cmd::run(cmd);
            finish_command(telemetry, name, None, exit_code);
        }
        #[cfg(feature = "agent-context")]
        LifecycleCommand::AgentContext { command } => {
            let exit_code = agent_context_cmd::run(command);
            finish_command(telemetry, name, command.map(|spec| spec.word), exit_code);
        }
        #[cfg(feature = "extensions")]
        LifecycleCommand::Extension { action } => {
            // ADR-0431 §3: the action word is the event's action — an
            // `&'static str`, never a package name (ADR-0079 anonymity),
            // the `skill` command's pattern.
            let action_word = action.as_str();
            let exit_code = extension_cmd::run(action);
            finish_command(telemetry, name, Some(action_word), exit_code);
        }
    }
}

/// The shared tail of every [`run_lifecycle_command`] arm that ran a
/// command: emit `RELATE-CLI_COMMAND` (ADR-0079 §4) and exit through
/// [`exit_flushed`]. `name` is the command word as telemetry reports it
/// (`update` reports "upgrade"); `action` is the sub-action, if any.
fn finish_command(
    telemetry: &mut telemetry::Telemetry,
    name: &'static str,
    action: Option<&'static str>,
    exit_code: i32,
) -> ! {
    telemetry.emit(telemetry::Event::Command {
        command: name,
        action,
        success: exit_code == 0,
    });
    exit_flushed(telemetry, exit_code);
}

/// The one argv builder for the command-parser unit tests (the `*_cmd`
/// modules): `args(&["./db", "--json"])` instead of a verbatim copy per
/// module.
#[cfg(test)]
pub(crate) fn args(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|s| s.to_string()).collect()
}

// Unit tests for the entry point's pure helpers live in `tests.rs` so
// this file stays under the repo's 1k-line budget.
#[cfg(test)]
mod tests;
