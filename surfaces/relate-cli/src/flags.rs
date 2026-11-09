//! Hand-rolled CLI flag parser (ADR-0069, ADR-0104): the flag-state struct
//! and the argv parse loop (the help text itself lives in
//! [`crate::help`]; lifecycle verbs live in [`lifecycle`]; the usage-error
//! convention and its suggestion slot live in [`usage`]). Extracted from
//! `main.rs` to keep the entry point under the repo's 1k-line file budget.

use indexmap::IndexMap;

use relate_formatter::Mode;

use crate::help::{HelpScope, HelpTopic};
use crate::params;
use crate::parse_error::{is_access_sighting, parse_access_sighting, set_access_once};
use crate::print_error;

mod lifecycle;
pub(crate) use lifecycle::LifecycleCommand;

// The --snapshot-format value: the grammar, the one `parquet-wide`
// spelling, and the accepted-value list (ADR-0163, ADR-0534).
mod snapshot_format;
pub(crate) use snapshot_format::{codec_names, values as snapshot_format_values, SnapshotFormat};
// The serve help row's opt-in clause — serve-gated like its only
// consumer, so the always-compiled parser module carries no dead code in
// the other bundles.
#[cfg(feature = "serve")]
pub(crate) use snapshot_format::opt_in_note;

// The usage-error presentation (renderer, suggestion slot, shared
// assemblies). The private `use` serves this module's parse loop (and
// the lifecycle grammars via `super::`); the `pub(crate)` re-exports are
// exactly the items the extensions-gated `run` mount consumes — the
// render body (its manifest-scope entry) and the suggestion type it
// takes — plus the shared command-scope assembly, so the ungated lanes
// never see an unused re-export.
mod usage;
pub(crate) use usage::command_usage_error;
use usage::usage_error;
#[cfg(feature = "extensions")]
pub(crate) use usage::{render_usage_error, UsageSuggestion};
// The reserved-word lookup (ADR-0069, ADR-0429 §6), for the extension
// registry's collision refusal and the verbs that pre-flight it — the
// lifecycle module itself stays private to this parser; the re-export rides
// the consumers that read it from outside.
#[cfg(feature = "extensions")]
pub(crate) use lifecycle::reserved_word;
// The `-h`/`--help` pre-scan (the help-wins rule every compiled-in
// command's tail gets), for the `run` mount's dynamic help decision
// (LABS-552) — one policy fn, re-exported the way `reserved_word` is.
#[cfg(feature = "extensions")]
pub(crate) use lifecycle::wants_help;

/// Parsed command-line state: every knob the flag loop can set, with the
/// same defaults `main` used before the extraction.
pub(crate) struct Flags {
    pub db_path: String,
    pub query: Option<String>,
    /// The `query` verb was the first positional. With no statement, that
    /// verb is the session (stdin script or the shell). Without it, no
    /// statement prints help.
    pub explicit_query: bool,
    pub mode: Option<Mode>,
    pub no_header: bool,
    /// `--pretty`: force indented JSON even when stdout is not a terminal
    /// (ADR-0240).
    pub pretty: bool,
    pub show_version: bool,
    pub show_help: bool,
    pub lifecycle: Option<LifecycleCommand>,
    pub assignments: IndexMap<String, params::Assignment>,
    /// `--snapshot-format` (ADR-0163, ADR-0534): a codec name, or
    /// `parquet-wide` for the Parquet codec's wide-column part encoding.
    /// `None` defers to `RELATE_SNAPSHOT_FORMAT`, then the build default.
    pub snapshot_format: Option<SnapshotFormat>,
    /// `--markdown-body-properties` (ADR-0460): the markdown vault's
    /// body-property configuration — the ordered default list plus
    /// `Label=…` per-label overrides. `None` defers to
    /// `RELATE_MARKDOWN_BODY_PROPERTIES` or the sidecar manifest's
    /// recorded configuration.
    pub markdown_body_properties: Option<relate::MarkdownBodyProperties>,
    pub wal_dir: Option<std::path::PathBuf>,
    pub checkpoint_interval_secs: Option<u64>,
    /// `--durability-mode`. `None` defers to `RELATE_DURABILITY_MODE`,
    /// then the engine default (`full`) — the shared resolution (ADR-0470).
    pub durability_mode: Option<relate::DurabilityMode>,
    pub logical_graph_size_limit: Option<u64>,
    /// `--expression-memory-limit`. `None` defers to
    /// `RELATE_EXPRESSION_MEMORY_LIMIT`; unset keeps the engine default;
    /// `Some(0)` (the flag spelled `none` or `0`) disables the cap (ADR-0463).
    pub expression_memory_limit: Option<u64>,
    /// `--concurrent-logical-workspace-limit`. `None` defers to
    /// `RELATE_CONCURRENT_LOGICAL_WORKSPACE_LIMIT`; unset leaves
    /// admission off (ADR-0452).
    pub concurrent_logical_workspace_limit: Option<u64>,
    /// `--max-traversal-depth`. `None` defers to
    /// `RELATE_MAX_TRAVERSAL_DEPTH`, then the engine default (LABS-610).
    pub max_traversal_depth: Option<u32>,
    /// The safe-mode override (LABS-680, ADR-0477): `Some(true)` for
    /// `--safe`, `Some(false)` for `--no-safe`, `None` when neither was
    /// given — the ladder (flag, then `RELATE_SAFE_MODE`, then off)
    /// resolves after the no-side-effect exits.
    pub safe: Option<bool>,
    /// `--max-rows`; `None` when absent (the default policy is resolved in
    /// `row_cap`, after help/version/lifecycle exits).
    pub max_rows: Option<usize>,
    /// The agent-mode override (ADR-0407): `Some(true)` for
    /// `--agent-mode`, `Some(false)` for `--no-agent-mode`, `None` when
    /// neither was given — the
    /// ladder (flag, then `RELATE_AGENT_MODE`, then detection) resolves
    /// in `agent_vars` after the no-side-effect exits.
    pub agent: Option<bool>,
    /// `--width` — the total display width the human table wraps to
    /// (LABS-399); `None` when absent (the terminal width is auto-detected
    /// in `main`, and a piped run keeps the natural width).
    pub width: Option<usize>,
    /// `--timeout` per-statement wall-clock bound (ADR-0241); `None` when
    /// absent or `0` (no bound).
    pub timeout: Option<std::time::Duration>,
    pub load_csv_dir: Option<std::path::PathBuf>,
    /// The `--read-only` sighting's open mode (ADR-0480): `None` is the
    /// no-flag `OpenMode::Writable` default, `Some(mode)` the witnessed
    /// freshness's mode — one fact, parked once-only through
    /// `set_access_once`. Resolve through [`Flags::open_mode`], never
    /// re-derive the default.
    pub access: Option<relate::OpenMode>,
    /// `None` when `--namespace` was not given; the supplied-ness of the
    /// flag is exactly the `Option` (default `neo4j` is applied by the
    /// caller).
    pub namespace: Option<relate::Namespace>,
    /// `--load SOURCE` (LABS-903, ADR-0500): the store-content source — a
    /// `.rl` path, a directory, or an `https` URL. With the `:memory:`
    /// target (the `--database` default) it is the memory turn-into; with
    /// a path target it creates the store there. `None` when not given.
    /// Parses in every build (the remote fetch fails closed without the
    /// `remote-database` feature).
    pub load: Option<String>,
}

impl Default for Flags {
    fn default() -> Self {
        Self {
            db_path: ":memory:".to_string(),
            query: None,
            explicit_query: false,
            mode: None,
            no_header: false,
            pretty: false,
            show_version: false,
            show_help: false,
            lifecycle: None,
            assignments: IndexMap::new(),
            snapshot_format: None,
            markdown_body_properties: None,
            wal_dir: None,
            checkpoint_interval_secs: None,
            durability_mode: None,
            logical_graph_size_limit: None,
            expression_memory_limit: None,
            concurrent_logical_workspace_limit: None,
            max_traversal_depth: None,
            safe: None,
            max_rows: None,
            agent: None,
            width: None,
            timeout: None,
            load_csv_dir: None,
            access: None,
            namespace: None,
            load: None,
        }
    }
}

impl Flags {
    /// The access slot's resolution — [`parse_error::resolve_access`]'s
    /// one statement of the no-flag default (ADR-0480) — with the one
    /// implication layered on it (LABS-903, ADR-0500): a `--load` whose
    /// target is a memory URI opens as [`OpenMode::Ephemeral`] — the
    /// memory turn-into — whatever the access slot said. The resolution
    /// itself is [`crate::load::access_mode`]'s, the one statement both
    /// surfaces delegate to (serve's access build is its sibling caller),
    /// so the implication cannot drift between them; the refused
    /// combinations (`--load` with `--read-only=live`, or with any
    /// `--read-only` sighting beside a durable target) are caught at
    /// parse time ([`crate::load::parse_conflict`]), so this resolution
    /// never sees them. The only open-mode resolution the query path has:
    /// callers never re-derive the default or the implication.
    pub(crate) fn open_mode(&self) -> relate::OpenMode {
        crate::load::access_mode(&self.db_path, self.load.is_some(), self.access)
    }
}

/// Advance past a flag to its value. A missing value exits through
/// [`usage_error`] for `scope`. The next token is the value even when it
/// starts with `-`.
fn take_value<'a>(
    args: &'a [String],
    i: &mut usize,
    arg: &str,
    scope: HelpScope,
    what: &str,
) -> &'a str {
    *i += 1;
    if *i >= args.len() {
        usage_error(scope, &format!("{arg} requires {what}"), None);
    }
    &args[*i]
}

/// The single writer of the query slot: `--command` and the positional
/// `QUERY` are mutually exclusive in either argument order, so both route
/// through here.
fn set_query(query: &mut Option<String>, value: &str) {
    if query.is_some() {
        print_error("Error: --command cannot be combined with a positional query");
        std::process::exit(2);
    }
    *query = Some(value.to_string());
}

/// The env-var default for `--logical-graph-size-limit`, the fallible core
/// (ADR-0390): reads `RELATE_LOGICAL_GRAPH_SIZE_LIMIT` when
/// `--logical-graph-size-limit` was omitted; a bad value is the caller's
/// error (`invalid RELATE_LOGICAL_GRAPH_SIZE_LIMIT: …`), so the query
/// path prints and exits 2 through [`parse_value`]'s message shape while
/// `serve`'s setup fails through its own `Err(code)` contract (never a bare
/// `process::exit` that bypasses the command's telemetry flush). Call this
/// only after help, version, and lifecycle commands have exited, so those
/// paths do not read the variable.
pub(crate) fn logical_graph_size_limit_from_env_checked(
    logical_graph_size_limit: Option<u64>,
) -> Result<Option<u64>, String> {
    if logical_graph_size_limit.is_some() {
        return Ok(logical_graph_size_limit);
    }
    // Every malformed env state surfaces through the Result channel (round
    // 2, F-2-2): NotPresent is the unset default (Ok(None)), NotUnicode — a
    // present-but-malformed value — is the same Err class as a parse
    // failure, never a silent fall-through to unlimited.
    let raw = match std::env::var("RELATE_LOGICAL_GRAPH_SIZE_LIMIT") {
        Ok(raw) => raw,
        Err(std::env::VarError::NotPresent) => return Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err("invalid RELATE_LOGICAL_GRAPH_SIZE_LIMIT: value is not valid UTF-8".into());
        }
    };
    if raw.trim().is_empty() {
        return Ok(None);
    }
    relate::parse_memory_limit(&raw)
        .map(Some)
        .map_err(|e| format!("invalid RELATE_LOGICAL_GRAPH_SIZE_LIMIT: {e}"))
}

/// The env-var default for `--expression-memory-limit` (ADR-0463): reads
/// `RELATE_EXPRESSION_MEMORY_LIMIT` when the flag was omitted. Unset or
/// blank keeps the engine default (handled by the engine's resolver); a bad
/// value is the caller's error. Call this only after help, version, and
/// lifecycle commands have exited.
pub(crate) fn expression_memory_limit_from_env_checked(
    expression_memory_limit: Option<u64>,
) -> Result<Option<u64>, String> {
    if expression_memory_limit.is_some() {
        return Ok(expression_memory_limit);
    }
    // The engine owns the env contract: blank is unset, and a present but
    // unusable value (bad UTF-8 or a bad size) fails open, never silently
    // falling back to the default. This runs after help/version/lifecycle
    // commands have exited, so those paths do not read the variable.
    relate::expression_limit_from_setting(std::env::var("RELATE_EXPRESSION_MEMORY_LIMIT"))
        .map_err(semantic_message)
}

/// The env-var default for `--concurrent-logical-workspace-limit`
/// (ADR-0452), the sibling of [`logical_graph_size_limit_from_env_checked`].
/// A positive value turns admission on. Unset or blank leaves it off.
/// `0` and a value that does not parse are the caller's error. Call this
/// only after help, version, and lifecycle commands have exited.
pub(crate) fn concurrent_logical_workspace_limit_from_env_checked(
    concurrent_logical_workspace_limit: Option<u64>,
) -> Result<Option<u64>, String> {
    if let Some(n) = concurrent_logical_workspace_limit {
        return relate::require_positive_memory_limit("--concurrent-logical-workspace-limit", n)
            .map(Some)
            .map_err(semantic_message);
    }
    // The engine owns the env contract (blank is off, a bad or non-UTF-8
    // value fails open). This runs only after help, version, and lifecycle
    // commands have exited, so those paths do not read the variable.
    relate::workspace_limit_from_setting(std::env::var("RELATE_CONCURRENT_LOGICAL_WORKSPACE_LIMIT"))
        .map_err(semantic_message)
}

/// The env-var default for `--max-traversal-depth` (LABS-610), the sibling
/// of the two limit resolvers above. The flag's value wins; otherwise
/// `RELATE_MAX_TRAVERSAL_DEPTH`; otherwise the engine default. A value
/// outside 1 through `MAX_TRAVERSAL_DEPTH_CEILING` (2000) is the caller's
/// error, as is a value that does not parse. Unset or blank is the default.
/// Call this only after help, version, and lifecycle commands have exited.
pub(crate) fn max_traversal_depth_from_env_checked(
    max_traversal_depth: Option<u32>,
) -> Result<Option<u32>, String> {
    if let Some(n) = max_traversal_depth {
        return relate::require_traversal_depth("--max-traversal-depth", n)
            .map(Some)
            .map_err(semantic_message);
    }
    match std::env::var("RELATE_MAX_TRAVERSAL_DEPTH") {
        Ok(raw) if raw.trim().is_empty() => Ok(None),
        Ok(raw) => {
            let n = raw.trim().parse::<u32>().map_err(|_| {
                format!("invalid RELATE_MAX_TRAVERSAL_DEPTH: '{raw}' is not a number")
            })?;
            relate::require_traversal_depth("RELATE_MAX_TRAVERSAL_DEPTH", n)
                .map(Some)
                .map_err(semantic_message)
        }
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err("invalid RELATE_MAX_TRAVERSAL_DEPTH: value is not valid UTF-8".to_string())
        }
    }
}

/// The safe-mode ladder (LABS-680, ADR-0477): the explicit `--safe` /
/// `--no-safe` wins, then `RELATE_SAFE_MODE` (`1` or `0`), then off.
/// Unset or blank is off; any other value is a usage error naming the
/// variable, the same contract as the sibling limit resolvers. Call this
/// only after help, version, and lifecycle commands have exited.
pub(crate) fn safe_mode_from_env_checked(safe: Option<bool>) -> Result<bool, String> {
    if let Some(on) = safe {
        return Ok(on);
    }
    match std::env::var("RELATE_SAFE_MODE") {
        Ok(raw) if raw.trim().is_empty() => Ok(false),
        Ok(raw) => match raw.trim() {
            "1" => Ok(true),
            "0" => Ok(false),
            other => Err(format!("invalid RELATE_SAFE_MODE: '{other}' is not 1 or 0")),
        },
        Err(std::env::VarError::NotPresent) => Ok(false),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err("invalid RELATE_SAFE_MODE: value is not valid UTF-8".to_string())
        }
    }
}

/// The env-var default for `--durability-mode` (ADR-0470), the sibling of
/// [`max_traversal_depth_from_env_checked`]: the flag's value wins;
/// otherwise `RELATE_DURABILITY_MODE`; otherwise the option stays
/// `None` and the engine's own resolution applies the default (`full`) at
/// the open. The engine owns the setting contract (blank is unset, a bad
/// or non-UTF-8 value fails open naming the variable) —
/// [`relate::durability_mode_from_setting`] is that one parse, so the
/// CLI wrapper never re-states the mode vocabulary. Call this only after
/// help, version, and lifecycle commands have exited, so those paths do
/// not read the variable.
pub(crate) fn durability_mode_from_env_checked(
    durability_mode: Option<relate::DurabilityMode>,
) -> Result<Option<relate::DurabilityMode>, String> {
    if durability_mode.is_some() {
        return Ok(durability_mode);
    }
    relate::durability_mode_from_setting(std::env::var("RELATE_DURABILITY_MODE"))
        .map_err(semantic_message)
}

/// The engine's [`relate::Error::Semantic`] payload, without the
/// `semantic error:` display prefix the CLI's `Error:` line would repeat.
fn semantic_message(err: relate::Error) -> String {
    match err {
        relate::Error::Semantic(msg) => msg,
        other => other.to_string(),
    }
}

/// `--max-traversal-depth`'s value parse (LABS-610): an integer in
/// `1..=MAX_TRAVERSAL_DEPTH_CEILING`. The message is the `parse_value`
/// reason only, which prefixes `invalid --max-traversal-depth:`.
fn parse_traversal_depth(value: &str) -> Result<u32, String> {
    let n = value
        .parse::<u32>()
        .map_err(|_| format!("'{value}' is not a number"))?;
    if (1..=relate::MAX_TRAVERSAL_DEPTH_CEILING).contains(&n) {
        Ok(n)
    } else {
        Err(format!(
            "must be between 1 and {}",
            relate::MAX_TRAVERSAL_DEPTH_CEILING
        ))
    }
}

/// `take_value`'s sibling epilogue: parse a flag's value or die with the
/// shared "invalid" text.
fn parse_value<T, E: std::fmt::Display>(
    arg: &str,
    value: &str,
    parse: impl FnOnce(&str) -> Result<T, E>,
) -> T {
    match parse(value) {
        Ok(v) => v,
        Err(e) => {
            print_error(format!("Error: invalid {arg}: {e}"));
            std::process::exit(2);
        }
    }
}

/// Apply one session flag into `state`. Returns `true` when `arg` was a
/// known session flag. Unknown flags return `false` so the caller can emit
/// a scoped usage error. `scope` selects the synopsis for a missing or
/// invalid value.
fn apply_session_flag(
    arg: &str,
    args: &[String],
    i: &mut usize,
    state: &mut Flags,
    scope: HelpScope,
) -> bool {
    match arg {
        "-h" | "--help" => {
            // Help wins instantly: stop parsing (later args are never
            // validated) and let the shared telemetry handle below emit
            // the events. Callers that break on `show_help` honor this.
            state.show_help = true;
            true
        }
        "-version" | "--version" | "-v" => {
            state.show_version = true;
            true
        }
        "-json" | "--json" => {
            state.mode = Some(Mode::Json);
            true
        }
        "-csv" | "--csv" => {
            state.mode = Some(Mode::Csv);
            true
        }
        "-markdown" | "--markdown" => {
            state.mode = Some(Mode::Markdown);
            true
        }
        "-jsonl" | "--jsonl" => {
            state.mode = Some(Mode::Jsonl);
            true
        }
        "--toon" => {
            state.mode = Some(Mode::Toon);
            true
        }
        "-noheader" | "--noheader" => {
            state.no_header = true;
            true
        }
        "--pretty" => {
            state.pretty = true;
            true
        }
        "-d" | "--database" => {
            // ADR-0200: the store is a flag, not a positional. The error
            // names `--database` for both spellings. A value that starts
            // with `-` is still a path (`-d -dashed`).
            state.db_path = take_value(args, i, "--database", scope, "a path").to_string();
            true
        }
        "--wal-dir" => {
            state.wal_dir = Some(std::path::PathBuf::from(take_value(
                args, i, arg, scope, "a path",
            )));
            true
        }
        "--load-csv-dir" => {
            state.load_csv_dir = Some(std::path::PathBuf::from(take_value(
                args, i, arg, scope, "a path",
            )));
            true
        }
        "--checkpoint-interval-secs" => {
            let value = take_value(args, i, arg, scope, "a number");
            state.checkpoint_interval_secs = Some(parse_value(arg, value, |v| {
                v.parse::<u64>()
                    .map_err(|_| format!("'{v}' is not a number"))
            }));
            true
        }
        "--durability-mode" => {
            let value = take_value(
                args,
                i,
                arg,
                scope,
                "a value (full, everysecond, or snapshot)",
            );
            state.durability_mode = Some(parse_value(arg, value, |v| {
                v.parse::<relate::DurabilityMode>()
            }));
            true
        }
        token if is_access_sighting(token) => {
            // A sighting token (parse_error's one grammar) is parsed and
            // parked through the shared helpers, whose docs own the
            // freshness and once-only policy; a bad value dies through
            // `usage_error` like every other flag. The removed `--follow`
            // spelling matches nothing here and falls to the unknown-flag
            // arm.
            let next = args.get(*i + 1).map(String::as_str);
            let die = |e: crate::parse_error::ParseError| usage_error(scope, &e.to_string(), None);
            parse_access_sighting(token, next)
                .and_then(|freshness| set_access_once("relate", &mut state.access, freshness))
                .unwrap_or_else(die);
            true
        }
        "--logical-graph-size-limit" => {
            let value = take_value(args, i, arg, scope, "a SIZE (bytes or mb/mib/gb/gib)");
            state.logical_graph_size_limit =
                Some(parse_value(arg, value, relate::parse_memory_limit));
            true
        }
        "--expression-memory-limit" => {
            let value = take_value(
                args,
                i,
                arg,
                scope,
                "a SIZE (bytes, mb/mib/gb/gib, or none)",
            );
            state.expression_memory_limit = Some(parse_value(
                arg,
                value,
                relate::parse_expression_memory_limit,
            ));
            true
        }
        "--concurrent-logical-workspace-limit" => {
            let value = take_value(args, i, arg, scope, "a SIZE (bytes or mb/mib/gb/gib)");
            state.concurrent_logical_workspace_limit =
                Some(parse_value(arg, value, relate::parse_memory_limit));
            true
        }
        "--max-traversal-depth" => {
            let value = take_value(args, i, arg, scope, "a hop count (1 through 2000)");
            state.max_traversal_depth = Some(parse_value(arg, value, parse_traversal_depth));
            true
        }
        "--load" => {
            // LABS-903 (ADR-0500): the store-content source — a `.rl`
            // path, a directory, or an `https` URL — parsed in every
            // build (the remote fetch is the `remote-database` feature's,
            // failing closed without it, ADR-0104). Value-taking and
            // last-one-wins, like every session value flag; the
            // target/disposition resolution is `crate::load`'s.
            state.load = Some(
                take_value(
                    args,
                    i,
                    arg,
                    scope,
                    "a store source (a .rl path, a directory, or an https URL)",
                )
                .to_string(),
            );
            true
        }
        "--safe" | "--no-safe" => {
            // LABS-680 (ADR-0477): value-less, last one wins, resolved with
            // the environment after the no-side-effect exits.
            state.safe = Some(arg == "--safe");
            true
        }
        "--namespace" => {
            // ADR-0118: an unknown name fails here, before the store is
            // opened, with the caller's usage scope.
            let value = take_value(
                args,
                i,
                arg,
                scope,
                "a value (neo4j, system or system_ontologies)",
            );
            match relate::Namespace::parse(value) {
                Some(ns) => state.namespace = Some(ns),
                None => usage_error(
                    scope,
                    &format!(
                        "invalid --namespace: unknown namespace '{value}' (expected neo4j, system or system_ontologies)"
                    ),
                    None,
                ),
            }
            true
        }
        "-c" | "--command" => {
            let value = take_value(args, i, arg, scope, "a Cypher statement");
            set_query(&mut state.query, value);
            true
        }
        "--width" => {
            let value = take_value(args, i, arg, scope, "a value (a terminal width in columns)");
            state.width = Some(parse_value(arg, value, crate::width::parse_width));
            true
        }
        "--max-rows" => {
            let value = take_value(args, i, arg, scope, "a value (a row count; 0 = unlimited)");
            state.max_rows = Some(parse_value(arg, value, crate::row_cap::parse_max_rows));
            true
        }
        "--agent-mode" | "--no-agent-mode" => {
            // ADR-0407: the explicit agent-mode override — value-less
            // flags, last one wins, resolved in `agent_vars` after the
            // no-side-effect exits. The `-mode` suffix keeps the spelling
            // clear of `skill install --agent NAME`'s harness selector.
            state.agent = Some(arg == "--agent-mode");
            true
        }
        "--timeout" => {
            // ADR-0241: 0 (any unit) is no bound, matching --max-rows 0.
            let value = take_value(args, i, arg, scope, "a duration (e.g. 500ms, 10s, 2m)");
            let parsed = parse_value(arg, value, crate::timeout::parse_timeout);
            state.timeout = (!parsed.is_zero()).then_some(parsed);
            true
        }
        "--snapshot-format" => {
            let value = take_value(
                args,
                i,
                arg,
                scope,
                &format!("a value ({})", snapshot_format_values(" or ")),
            );
            state.snapshot_format = Some(parse_value(arg, value, |v| v.parse::<SnapshotFormat>()));
            true
        }
        "--markdown-body-properties" => {
            let value = take_value(
                args,
                i,
                arg,
                scope,
                "a value (property list, or Label=property list; entries separated by ';')",
            );
            state.markdown_body_properties = Some(parse_value(arg, value, |v| {
                relate::MarkdownBodyProperties::parse(v)
            }));
            true
        }
        // ADR-0239: `--format` is the documented alias of `--mode` (the
        // neo4j-cli spelling); `-mode` is the legacy single-dash form.
        "-mode" | "--mode" | "--format" => {
            let value = take_value(
                args,
                i,
                arg,
                scope,
                &format!("a value (one of: {})", Mode::names()),
            );
            match Mode::parse(value) {
                Some(m) => state.mode = Some(m),
                None => {
                    print_error(format!(
                        "Error: invalid mode '{value}' (expected one of: {})",
                        Mode::names()
                    ));
                    std::process::exit(2);
                }
            }
            true
        }
        "-param" | "--param" => {
            let value = take_value(args, i, arg, scope, "a value (name=value)");
            match params::parse_param_pair(value) {
                Ok((k, v)) => {
                    state.assignments.insert(k, params::Assignment::Param(v));
                }
                Err(e) => {
                    print_error(format!("Error: invalid --param value: {e}"));
                    std::process::exit(2);
                }
            }
            true
        }
        // Every flag parses in every build (ADR-0104): splitting and
        // classification are ungated (ADR-0270); only the fetch is
        // feature-gated, and resolution fails closed without it.
        // `--fetch-params` only: per AGENTS.md, single-dash spellings are
        // legacy aliases, not a pattern to extend.
        "--fetch-params" => {
            let value = take_value(
                args,
                i,
                arg,
                scope,
                "a value (NAME=SOURCE, a URL or file path)",
            );
            match params::parse_fetch_assignment(value) {
                Ok((name, source)) => {
                    state
                        .assignments
                        .insert(name, params::Assignment::Source(source));
                }
                Err(e) => {
                    print_error(format!("Error: invalid --fetch-params value: {e}"));
                    std::process::exit(2);
                }
            }
            true
        }
        _ => false,
    }
}

/// Parse `args` (including argv[0], which is skipped) into [`Flags`]. Usage
/// errors print and exit(2) from inside the loop and from the final
/// lifecycle/query conflict check.
pub(crate) fn parse(args: &[String]) -> Flags {
    let mut state = Flags::default();
    // Parameter assignments: the last flag for a name wins, whether it was
    // bound by --param or --fetch-params (IndexMap::insert overwrites in place).
    // --param values parse now (cheap, no I/O); --fetch-params sources only
    // resolve after the help/version/lifecycle exits, so those no-side-effect
    // paths never fetch.

    // `query` stays in this loop (ADR-0200). After the verb the scope
    // switches so the tail shares the same walk as the path-first form,
    // with the query synopsis and suggestion list.
    let mut scope = HelpScope::TopLevel;

    let mut end_of_options = false;
    let mut i = 1;
    let mut positional_count = 0;
    while i < args.len() {
        // Borrow the current argument; only values stored into parse state
        // (db_path, query, …) are cloned, at the point they are stored.
        let arg = &args[i];

        if !end_of_options && arg == "--" {
            end_of_options = true;
            i += 1;
            continue;
        }

        if !end_of_options && arg.starts_with('-') && arg != "-" {
            if apply_session_flag(arg.as_str(), args, &mut i, &mut state, scope) {
                if state.show_help {
                    // Help wins instantly: stop parsing (later args are
                    // never validated) and let the shared telemetry handle
                    // below emit the events.
                    break;
                }
            } else {
                // ADR-0518: this exit(2) means the pending statement never
                // reaches the engine, so ADR-0230's fallback never runs — a
                // mistyped command word followed by a bad flag keeps its
                // suggestion by carrying it on the exit itself.
                // `unknown_flag_suggestion` owns the command-over-flag
                // precedence and its rationale.
                usage_error(
                    scope,
                    &format!("unknown flag: {arg}"),
                    usage::unknown_flag_suggestion(scope, arg, state.query.as_deref()),
                );
            }
        } else {
            // ADR-0069 / ADR-0200: command words are recognized only as the
            // FIRST positional, never after `--`, and never in the `query`
            // tail. A store whose name is a command word is `--database
            // <word>`. The static word found here travels into the command
            // parse, so a verb whose feature is compiled out keeps its
            // &'static str (NotIncluded) without a second COMMAND_WORDS lookup.
            if positional_count == 0 && !end_of_options && matches!(scope, HelpScope::TopLevel) {
                if let Some(word) = lifecycle::COMMAND_WORDS
                    .iter()
                    .copied()
                    .find(|w| *w == arg.as_str())
                {
                    if word == "query" {
                        // Statement-session verb. Help prints and exits;
                        // otherwise the same loop fills the session. The
                        // verb itself opts into stdin or the shell when no
                        // statement is given.
                        if lifecycle::wants_help(&args[i + 1..]) {
                            state.lifecycle = Some(LifecycleCommand::Help {
                                topic: HelpTopic::Query,
                            });
                            break;
                        }
                        state.explicit_query = true;
                        scope = HelpScope::Command(HelpTopic::Query);
                        i += 1;
                        continue;
                    }
                    state.lifecycle = Some(lifecycle::parse_lifecycle_command(
                        word,
                        &args[i + 1..],
                        state.namespace,
                    ));
                    break;
                }
            }
            // The only positional on a statement session is the statement.
            // The store is `--database` (default `:memory:`).
            match positional_count {
                0 => set_query(&mut state.query, arg),
                // ADR-0518: same cut-off as the unknown-flag exit — the
                // pending statement (the first positional, the mistyped
                // command) never reaches the engine, so its suggestion
                // rides this exit.
                _ => usage_error(
                    scope,
                    &format!("unexpected argument: {arg}"),
                    usage::statement_usage_suggestion(state.query.as_deref()),
                ),
            }
            positional_count += 1;
        }
        i += 1;
    }

    // The one end-of-parse conflict stage (round 4, F-2-6): the
    // no-side-effect exits still win — `--help` and `--version` (the
    // caller owns both) and a lifecycle verb's own help page (`Help`,
    // `RunHelp`, `NotIncluded` — `is_help_shaped`'s set, one gate with
    // the flags') — so no conflict can precede any of them — and the two
    // policies, the lifecycle's and `--load`'s, each contribute their
    // answer here, one precedence for both. (`--help` breaks the parse
    // loop above, so this stage is the first thing that can fire after
    // it.) The lifecycle policy is `conflict_error`'s doc (ADR-0306 §6) —
    // the sub-parsers consume everything after the verb, so nothing can
    // change past this point; the `--load` policy is
    // `load::parse_conflict`'s, stated once (LABS-903, ADR-0500) —
    // refused here at parse time, before any post-parse resolution (a
    // `--fetch-params` source would otherwise be fetched before the usage
    // error). This site only routes each answer through `usage_error`
    // like every other usage error.
    if !(state.show_help
        || state.show_version
        || state
            .lifecycle
            .as_ref()
            .is_some_and(LifecycleCommand::is_help_shaped))
    {
        if let Some(cmd) = &state.lifecycle {
            if let Some((topic, msg)) = cmd.conflict_error(state.query.as_deref(), state.access) {
                usage_error(HelpScope::Command(topic), &msg, None);
            }
        }
        if let Some(msg) =
            crate::load::parse_conflict(&state.db_path, state.load.is_some(), state.access)
        {
            usage_error(HelpScope::TopLevel, &msg, None);
        }
    }

    state
}

// flags' unit tests live in `tests.rs` (the skill_cmd/serve_cmd
// dir-module pattern) so this file stays under the repo's 1k-line file budget.
#[cfg(test)]
mod tests;
