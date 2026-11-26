//! The server's clap definition, shared by the crate's two remaining
//! consumers: the container `entrypoint` includes it via `#[path]` to derive
//! which flags are valueless booleans (its argv tokenizer must not let a
//! bool swallow the next flag's value), and the e2e test harness parses
//! the same flag vocabulary to drive [`crate::server::serve`] in-process
//! (the standalone `relate-bolt` binary is retired — LABS-381, ADR-0368;
//! `relate serve` is the one product binary). Keep engine-facing
//! behavior (`impl Args`) in server.rs — this module must stay compilable
//! from the entrypoint crate root too.

use clap::Parser;
use relate_serve::origin::OriginFlagValue;

/// Default result byte cap: 256 MiB of logical bytes (ADR-0110).
pub const DEFAULT_MAX_RESULT_BYTES: u64 = 256 * 1024 * 1024;
/// The `--max-result-bytes` default as written on the CLI; must parse to
/// [`DEFAULT_MAX_RESULT_BYTES`] (a test keeps the two in lockstep). Clap's
/// `default_value` needs a string literal, so the attribute cannot reference
/// this const directly.
pub const DEFAULT_MAX_RESULT_BYTES_STR: &str = "256mib";
pub const DEFAULT_HANDSHAKE_TIMEOUT_SECS: u64 = 5;
pub const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 300;
/// `0` disables the per-query timeout.
pub const DEFAULT_QUERY_TIMEOUT_SECS: u64 = relate_core::deadline::DEFAULT_STATEMENT_TIMEOUT_SECS;
/// `0` is unlimited (ADR-0333). A positive value is the process cap.
pub const DEFAULT_MAX_CONNECTIONS: usize = 0;

#[derive(Parser, Debug)]
#[command(name = "relate-bolt")]
pub struct Args {
    /// Listen address
    #[arg(long, default_value = "127.0.0.1:7687")]
    pub bind: String,
    /// Database URI (`:memory:`, path, `file://`)
    #[arg(long, default_value = ":memory:")]
    pub uri: String,
    /// Open an existing database without writing, checkpointing, or creating a missing store (ADR-0136)
    #[arg(long, action = clap::ArgAction::SetTrue)]
    pub read_only: bool,
    /// Local WAL directory for persistent URIs
    #[arg(long)]
    pub wal_dir: Option<std::path::PathBuf>,
    /// Seconds between timed checkpoints when the WAL has work (default 300, RELATE_CHECKPOINT_INTERVAL_SECS; 0 disables). Persistent URIs only.
    #[arg(long)]
    pub checkpoint_interval_secs: Option<u64>,
    /// WAL durability: full, everysecond, or snapshot (default full; RELATE_DURABILITY_MODE)
    #[arg(long)]
    pub durability_mode: Option<relate::DurabilityMode>,
    /// Live graph + index cap: integer bytes, or a size with mb/mib/gb/gib (RELATE_LOGICAL_GRAPH_SIZE_LIMIT; 0 or omit is unlimited). A bad env value is rejected like a bad --logical-graph-size-limit.
    #[arg(long, env = "RELATE_LOGICAL_GRAPH_SIZE_LIMIT", value_name = "SIZE", value_parser = parse_cli_size)]
    pub logical_graph_size_limit: Option<u64>,
    /// Allow local LOAD CSV under this directory (default denied; overrides RELATE_LOAD_CSV_DIR)
    #[arg(long, value_name = "PATH")]
    pub load_csv_dir: Option<std::path::PathBuf>,
    /// Bolt auth: `none`, or a preset `user:password` credential (RELATE_AUTH; unset auto-generates a token, printed at startup and persisted next to a persistent database)
    #[arg(long, value_name = "USER:PASSWORD|none")]
    pub auth: Option<String>,
    /// PEM certificate chain to present to TLS clients (server cert + intermediates; requires --tls-key)
    #[arg(long, value_name = "PATH")]
    pub tls_cert: Option<std::path::PathBuf>,
    /// PEM private key for --tls-cert (requires --tls-cert)
    #[arg(long, value_name = "PATH")]
    pub tls_key: Option<std::path::PathBuf>,
    /// PEM CA bundle; when set, TLS clients must present a CA-signed certificate (mTLS; requires --tls-cert and --tls-key)
    #[arg(long, value_name = "PATH")]
    pub tls_client_ca: Option<std::path::PathBuf>,
    /// Allow plaintext Bolt on a non-loopback bind (insecure; prints a warning)
    #[arg(long)]
    pub allow_plaintext: bool,
    /// Disable Bolt-over-WebSocket serving (default on; a WebSocket upgrade request then closes like any other unrecognized first byte)
    #[arg(long)]
    pub no_websocket: bool,
    /// Allow this Origin on WebSocket upgrades (repeatable; an `http(s)://` origin URL or `*` for any Origin; the URL is validated and stored canonically; overrides the auth-dependent default: loopback Origins plus https://browser.neo4j.io and https://console.neo4j.io with auth on, loopback Origins only with --auth none)
    #[arg(long, value_name = "ORIGIN", value_parser = OriginFlagValue::parse)]
    pub ws_allow_origin: Vec<OriginFlagValue>,
    /// Maximum concurrent WebSocket sessions (ADR-0459). `0`, omit, or an unset RELATE_WS_MAX_CONNECTIONS is unlimited — the default, so bulk data transfers are not capped without asking. A positive value is an additional cap beside --max-connections, which keeps bounding every accepted connection.
    #[arg(
        long,
        env = "RELATE_WS_MAX_CONNECTIONS",
        default_value_t = relate_serve::DEFAULT_WS_MAX_CONNECTIONS,
        value_parser = parse_ws_max_connections
    )]
    pub ws_max_connections: usize,
    /// Maximum concurrent connections in this process. Over-cap connections are accepted then immediately closed. `0` is unlimited (ADR-0333, RELATE_MAX_CONNECTIONS). A blank environment value is unlimited, the same as omitting it.
    #[arg(
        long,
        env = "RELATE_MAX_CONNECTIONS",
        default_value_t = crate::cli_args::DEFAULT_MAX_CONNECTIONS,
        value_parser = parse_max_connections
    )]
    pub max_connections: usize,
    /// Concurrent logical-workspace pool (ADR-0452). A positive SIZE turns
    /// admission on. Omit, or an unset or blank
    /// `RELATE_CONCURRENT_LOGICAL_WORKSPACE_LIMIT`, leaves it off.
    /// `0` is rejected. Same SIZE grammar as `--logical-graph-size-limit`.
    // No `env =`. Clap would feed a set-but-blank value to the parser and
    // reject it, while the CLI and the engine treat blank as unset. When
    // this flag is absent the engine reads the variable.
    #[arg(long, value_name = "SIZE", value_parser = parse_workspace_limit)]
    pub concurrent_logical_workspace_limit: Option<u64>,
    /// Traversal-depth admission limit (LABS-610): the largest explicit
    /// `*1..N` / `{1,N}` hop bound a statement may ask for. Values are 1
    /// through 2000; omit or an unset `RELATE_MAX_TRAVERSAL_DEPTH`
    /// uses 500.
    // No `env =`, for the same blank-is-unset reason as the workspace
    // limit above; the engine reads the variable when this is `None`.
    #[arg(long, value_name = "HOPS", value_parser = parse_traversal_depth)]
    pub max_traversal_depth: Option<u32>,
    /// Per-allocation expression cap (ADR-0463): integer bytes or a size
    /// with mb/mib/gb/gib; `none`/`0` disables the cap. Omit, or an unset
    /// `RELATE_EXPRESSION_MEMORY_LIMIT`, keeps the engine default (the
    /// numeric default is in docs/api/bolt/spec.md's flag table, where it
    /// cites the constant).
    // No `env =`, for the same blank-is-unset reason as the workspace and
    // traversal limits; the engine reads the variable when this is `None`.
    #[arg(long, value_name = "SIZE", value_parser = parse_expression_limit)]
    pub expression_memory_limit: Option<u64>,
    /// Seconds allowed for the whole connection negotiation (first byte, TLS handshake, WebSocket upgrade, Bolt magic/version) before the connection is closed (default 5)
    #[arg(long, default_value_t = crate::cli_args::DEFAULT_HANDSHAKE_TIMEOUT_SECS)]
    pub handshake_timeout_secs: u64,
    /// Seconds a connection may stall mid-message before it is closed (default 300; 0 disables)
    #[arg(long, default_value_t = crate::cli_args::DEFAULT_IDLE_TIMEOUT_SECS)]
    pub idle_timeout_secs: u64,
    /// Seconds a statement may run before it is cancelled, when the request names no `tx_timeout` (default 30; 0 disables the default)
    #[arg(long, default_value_t = crate::cli_args::DEFAULT_QUERY_TIMEOUT_SECS)]
    pub query_timeout_secs: u64,
    /// Ceiling in seconds over every statement budget, including a request's `tx_timeout` and its "no bound" (default 0: no ceiling)
    #[arg(long, default_value_t = 0)]
    pub query_timeout_max_secs: u64,
    /// Maximum logical bytes in one query result before FAILURE (default 256 MiB; 0 disables)
    #[arg(long, value_name = "SIZE", default_value = crate::cli_args::DEFAULT_MAX_RESULT_BYTES_STR, value_parser = parse_cli_size)]
    pub max_result_bytes: u64,
    /// HELLO SUCCESS `server` agent string (default `Relate/<version>`, RELATE_IDENTITY; ADR-0293). Set a Neo4j-shaped value (e.g. `Neo4j/5.26.0`) for tooling that hard-checks the identity; the value is sent verbatim.
    #[arg(long, env = "RELATE_IDENTITY", value_name = "STRING")]
    pub identity: Option<String>,
}

pub fn parse_cli_size(s: &str) -> std::result::Result<u64, String> {
    relate::parse_memory_limit(s).map_err(|e| e.to_string())
}

fn parse_workspace_limit(s: &str) -> std::result::Result<u64, String> {
    relate::parse_positive_memory_limit("--concurrent-logical-workspace-limit", s).map_err(|e| {
        match e {
            relate::Error::Semantic(msg) => msg,
            other => other.to_string(),
        }
    })
}

/// `--max-traversal-depth`'s value (LABS-610): an integer in
/// `1..=MAX_TRAVERSAL_DEPTH_CEILING`, naming the flag on rejection.
fn parse_traversal_depth(s: &str) -> std::result::Result<u32, String> {
    let n = s
        .parse::<u32>()
        .map_err(|_| format!("invalid --max-traversal-depth '{s}': not a number"))?;
    relate::require_traversal_depth("--max-traversal-depth", n).map_err(|e| match e {
        relate::Error::Semantic(msg) => msg,
        other => other.to_string(),
    })
}

/// `--expression-memory-limit`'s value (ADR-0463): the query path's
/// `parse_expression_memory_limit` grammar — bytes, mb/mib/gb/gib, or
/// `none`/`0` for no cap.
fn parse_expression_limit(s: &str) -> std::result::Result<u64, String> {
    relate::parse_expression_memory_limit(s).map_err(|e| e.to_string())
}

pub fn parse_cli_count(s: &str) -> std::result::Result<u64, String> {
    let s = s.trim();
    // A set-but-blank env value is unlimited, matching `serve` and the
    // engine's `env_setting`. Clap would otherwise reject `""`.
    if s.is_empty() {
        return Ok(0);
    }
    s.parse::<u64>().map_err(|_| format!("invalid count '{s}'"))
}

/// The count caps' one parser, parameterized by flag name the way
/// [`relate_serve::parse_port`] is: the shared count grammar plus
/// [`relate_serve::count_cap_usize`] (the platform conversion and its
/// overflow message are the serve crate's, shared with its resolvers).
fn parse_flag_count(flag: &str, s: &str) -> std::result::Result<usize, String> {
    let count = parse_cli_count(s)?;
    relate_serve::count_cap_usize(flag, count)
}

fn parse_max_connections(s: &str) -> std::result::Result<usize, String> {
    parse_flag_count("max-connections", s)
}

fn parse_ws_max_connections(s: &str) -> std::result::Result<usize, String> {
    parse_flag_count("ws-max-connections", s)
}
