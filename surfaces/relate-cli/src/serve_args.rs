//! The `serve` argument layer (ADR-0232, LABS-381, ADR-0368): the
//! `ServeCmd` value, its `parse_serve_args` grammar, the compiled-in
//! surface-flag facts, and the shared value/once parsers — the parse half
//! of the `serve` command, split from `serve_cmd`'s process orchestration
//! (LABS-392).

/// The default `--host`: loopback only (ADR-0248).
use crate::parse_error::{
    flag_value, is_access_sighting, parse_access_sighting, set_access_once, set_once, ParseError,
};

// The unbound-surface policy only exists where a surface can be compiled.
// A `serve`-only build has no surface to reject, so the module (and its
// call below) is absent rather than an empty enum.
#[cfg(feature = "surface")]
mod unbound;

#[cfg(all(test, feature = "mcp"))]
pub(crate) use unbound::listener_flag_spellings;

/// The parsed origin-flag value type (round 4, F-4-2): the shared
/// grammar's typed product ([`OriginFlagValue::parse`]), stored in
/// [`ServeCmd::origins`] / [`ServeCmd::ws_allow_origins`] and built into
/// the surface policies, never re-tested as a string. One import for both
/// browser-facing origin flags (`--origin`, `--ws-allow-origin`).
#[cfg(any(feature = "http", feature = "bolt"))]
use relate_serve::origin::OriginFlagValue;

pub(crate) const DEFAULT_HOST: &str = "127.0.0.1";

/// The `--host` value's one normalization (round 4, F-4-7): the URL
/// spelling of an IPv6 literal — `[::1]` — strips to the bare literal,
/// the one spelling the bind, the loopback predicate, and the rendered
/// URLs all agree on, so `--host [::1]` and `--host ::1` are the same
/// command. A bracketed value around anything else is not a host
/// spelling and passes through unchanged; the bind is the loud failure
/// for a name that never resolves.
fn normalize_host(value: &str) -> String {
    match value
        .strip_prefix('[')
        .and_then(|bare| bare.strip_suffix(']'))
    {
        Some(bare) if bare.parse::<std::net::Ipv6Addr>().is_ok() => bare.to_string(),
        _ => value.to_string(),
    }
}

/// The `--mcp` value (LABS-381, ADR-0368): a typed union, not a port-shaped
/// string — `stdio` serves the MCP stdio transport in the foreground (the way MCP
/// hosts spawn the server), a port serves the streamable-HTTP transport
/// (ADR-0232 §4). One `--mcp` flag picks exactly one transport: the
/// once-only policy rejects combining the two.
#[cfg(feature = "mcp")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum McpMode {
    /// `--mcp stdio`: newline-delimited JSON-RPC on the process's
    /// stdin/stdout until the host closes stdin.
    Stdio,
    /// `--mcp PORT`: the streamable-HTTP transport on `--host:PORT`.
    Http(u16),
}

/// Parsed `serve` command. Each surface port field exists only when its
/// feature is compiled in; the module itself compiles when either is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServeCmd {
    /// The store to serve: a directory, a `.rl` bundle, or `:memory:`.
    pub database: String,
    /// `--host HOST`: the bind host for every served surface (ADR-0248).
    /// Default `127.0.0.1` (loopback only); anything else is deliberate
    /// plaintext exposure and is loudly warned about at startup. The
    /// value's grammar (round 4, F-4-7): a bare IPv4 or IPv6 literal or
    /// a hostname — plus the URL spelling of an IPv6 literal, `[::1]`,
    /// which the parse normalizes to the bare literal
    /// ([`normalize_host`]), so the bind, the loopback predicate, and
    /// every rendered URL see one spelling.
    pub host: String,
    /// `--http PORT`: bind the HTTP Query API v2 endpoint on
    /// `127.0.0.1:PORT`.
    #[cfg(feature = "http")]
    pub http_port: Option<u16>,
    /// `--mcp PORT|stdio`: the MCP transport — the streamable-HTTP
    /// listener on `--host:PORT`, or the foreground stdio transport.
    #[cfg(feature = "mcp")]
    pub mcp: Option<McpMode>,
    /// `--bolt PORT`: bind the Bolt server on `127.0.0.1:PORT`.
    #[cfg(feature = "bolt")]
    pub bolt_port: Option<u16>,
    /// `--auth USER:PASSWORD` (or `none`); `None` defers to
    /// `RELATE_AUTH` and then the generated token.
    pub auth: Option<String>,
    /// `--origin ORIGIN` (repeatable): the browser-Origin allowlist for
    /// the HTTP surface's guard and CORS echo — the grammar's typed
    /// values ([`OriginFlagValue`]: an exact origin URL or the
    /// wildcard), flowed from [`OriginFlagValue::parse`] through
    /// [`parse_origin`] (round 4, F-4-2).
    /// Empty = the built-in default (loopback plus the hosted
    /// `https://browser.neo4j.io` and `https://console.neo4j.io`).
    #[cfg(feature = "http")]
    pub origins: Vec<OriginFlagValue>,
    /// `--bolt-identity STRING`: the HELLO `server` agent string for the
    /// Bolt surface (ADR-0293). `None` defers to `RELATE_IDENTITY`
    /// and then the honest `Relate/<version>` default.
    #[cfg(feature = "bolt")]
    pub bolt_identity: Option<String>,
    /// `--max-connections N`. `None` reads `RELATE_MAX_CONNECTIONS`,
    /// then the non-zero process-wide default (ADR-0451). `Some(0)` is the
    /// explicit unlimited spelling (ADR-0333).
    pub max_connections: Option<u64>,
    /// `--ws-max-connections N` (ADR-0459): the opt-in WebSocket session
    /// cap. `None` reads `RELATE_WS_MAX_CONNECTIONS`, then unlimited
    /// (`0`) — the default, so bulk data-transfer deployments are not
    /// capped without asking. The process-wide `--max-connections` cap
    /// still bounds every accepted connection regardless.
    #[cfg(feature = "bolt")]
    pub ws_max_connections: Option<u64>,
    /// `--ws-allow-origin ORIGIN` (repeatable; ADR-0459): widen the
    /// WebSocket Origin default beyond loopback plus the hosted Neo4j
    /// Browser/Console. The grammar's typed values (an exact origin URL or
    /// the wildcard), parsed by the shared origin grammar. Empty = the
    /// auth-dependent default.
    #[cfg(feature = "bolt")]
    pub ws_allow_origins: Vec<OriginFlagValue>,
    /// `--header-deadline-secs N`: the absolute deadline for a complete
    /// HTTP/MCP request header block (ADR-0451). `None` reads
    /// `RELATE_HEADER_DEADLINE_SECS`, then the 10-second default;
    /// `Some(0)` disables the deadline.
    #[cfg(any(feature = "http", feature = "mcp"))]
    pub header_deadline_secs: Option<u64>,
    /// `--body-deadline-secs N`: the absolute deadline for a complete
    /// HTTP/MCP request body (ADR-0451). `None` reads
    /// `RELATE_BODY_DEADLINE_SECS`, then the 60-second default;
    /// `Some(0)` disables the deadline.
    #[cfg(any(feature = "http", feature = "mcp"))]
    pub body_deadline_secs: Option<u64>,
    /// `--tx-idle-timeout-secs N`: how long an HTTP managed transaction
    /// may sit idle before the next registry operation expires it
    /// (ADR-0435 §4). `None` reads `RELATE_TX_IDLE_TIMEOUT_SECS`, then
    /// the 60-second default; `Some(0)` disables expiry.
    #[cfg(feature = "http")]
    pub tx_idle_timeout_secs: Option<u64>,
    /// `--max-open-transactions N`: the cap on open HTTP managed
    /// transactions (ADR-0435 §4). `None` reads
    /// `RELATE_MAX_OPEN_TRANSACTIONS`, then the 128 default;
    /// `Some(0)` is the explicit unlimited spelling.
    #[cfg(feature = "http")]
    pub max_open_transactions: Option<u64>,
    /// `--concurrent-logical-workspace-limit SIZE` (ADR-0452). A positive
    /// value turns admission on. `None` defers to
    /// `RELATE_CONCURRENT_LOGICAL_WORKSPACE_LIMIT`, then off. Same
    /// `SIZE` grammar and shared resolution as `--logical-graph-size-limit`.
    pub concurrent_logical_workspace_limit: Option<u64>,
    /// `--max-traversal-depth N` (LABS-610): the largest explicit traversal
    /// hop bound a served statement may ask for. `None` defers to
    /// `RELATE_MAX_TRAVERSAL_DEPTH`, then the engine default (500).
    /// Valid values are 1 through 2000.
    pub max_traversal_depth: Option<u32>,
    /// `--safe` / `--no-safe` (LABS-680, ADR-0477): the served store's
    /// opt-in safe mode. `None` defers to `RELATE_SAFE_MODE` (`1`/`0`),
    /// then off. Last flag wins.
    pub safe: Option<bool>,
    /// `--logical-graph-size-limit SIZE`: the served store's live graph + index cap,
    /// the storage flag the memory-shape loadtest's bolt cases pass
    /// (ADR-0390, the ADR-0368 follow-up). Same value grammar as the
    /// query path's flag; `None` defers to `RELATE_LOGICAL_GRAPH_SIZE_LIMIT`
    /// (the shared resolution, `flags::logical_graph_size_limit_from_env_checked`), then
    /// unlimited.
    pub logical_graph_size_limit: Option<u64>,
    /// `--expression-memory-limit SIZE`: the served store's per-allocation
    /// expression cap (ADR-0463). Unset defers to
    /// `RELATE_EXPRESSION_MEMORY_LIMIT`, then the engine default;
    /// `none`/`0` disables the cap.
    pub expression_memory_limit: Option<u64>,
    /// `--durability-mode MODE` (ADR-0470): the served store's commit
    /// durability policy (ADR-0077) — `full`, `everysecond`, or
    /// `snapshot`, the query session's value grammar. `None` defers to
    /// `RELATE_DURABILITY_MODE`, then the engine default (`full`) —
    /// the shared resolution (`flags::durability_mode_from_env_checked`),
    /// so serve and the query session cannot drift on the same store.
    pub durability_mode: Option<relate::DurabilityMode>,
    /// `--snapshot-format F` (ADR-0163, ADR-0534): the served store's
    /// snapshot format — a codec name, or `parquet-wide` for the Parquet
    /// codec's wide-column part encoding, the same grammar the query
    /// session's flag parses (flags.rs). `None` defers to the engine's
    /// own resolution (`RELATE_SNAPSHOT_FORMAT`, then the build default).
    pub snapshot_format: Option<crate::flags::SnapshotFormat>,
    /// The store's open mode, resolved from the post-verb access slot
    /// through `parse_error::resolve_access` (`None` = the no-flag
    /// `OpenMode::Writable` default) — with the one `--load` implication
    /// layered on it (LABS-903, ADR-0500): a `--load` whose target is
    /// `:memory:` opens as `OpenMode::Ephemeral`, the memory turn-into.
    /// The freshness grammar and the once-only policy live in
    /// parse_error's `Freshness` / `parse_access_sighting` /
    /// `set_access_once` (ADR-0480).
    pub access: relate::OpenMode,
    /// `--load SOURCE` (LABS-903, ADR-0500): the store-content source — a
    /// `.rl` path, a directory, or an `https` URL — resolved by the shared
    /// `crate::load` against `--database`'s target at setup: the memory
    /// turn-into for `:memory:`, a created-and-cloned store for a path
    /// target. `None` when not given. Parses in every serve build (the
    /// remote fetch fails closed without the `remote-database` feature).
    pub load: Option<String>,
}

impl ServeCmd {
    /// True when the command binds at least one listener surface — every
    /// surface except the mcp stdio transport: the fold over the compiled
    /// [`SURFACE_FLAGS`] rows' own `binds` predicates, the one derivation
    /// of the listener fact. The stdio-only inert-flag policy (through
    /// `ServeCmd::is_stdio_only`, an mcp-build method) and the any-surface
    /// fold ([`ServeCmd::has_surface`], the parse's nothing-to-serve
    /// validation, which extends this one with the stdio transport)
    /// consult it; never re-derive the fold at a use site. Compiled only
    /// beside a surface: the fold reads the compiled-surfaces table,
    /// which the bare `serve` bundle does not carry (LABS-638).
    #[cfg(feature = "surface")]
    pub(crate) fn has_listener(&self) -> bool {
        SURFACE_FLAGS.iter().any(|surface| (surface.binds)(self))
    }

    /// True when `--http` was given: the HTTP surface's one derivation,
    /// consulted by [`has_listener`] and by the unbound-surface flag policy
    /// ([`unbound::reject`]), never re-derived from
    /// `http_port` at a use site.
    #[cfg(feature = "http")]
    pub(crate) fn binds_http(&self) -> bool {
        self.http_port.is_some()
    }

    /// True when `--bolt` was given: the Bolt surface's one derivation,
    /// the sibling of [`binds_http`](Self::binds_http).
    #[cfg(feature = "bolt")]
    pub(crate) fn binds_bolt(&self) -> bool {
        self.bolt_port.is_some()
    }

    /// True when `--mcp PORT` was given: the MCP listener's one derivation,
    /// the sibling of [`binds_http`](Self::binds_http). `--mcp stdio` binds
    /// nothing, so it is not this fact.
    #[cfg(feature = "mcp")]
    pub(crate) fn binds_mcp_listener(&self) -> bool {
        matches!(self.mcp, Some(McpMode::Http(_)))
    }

    /// True when this run binds a listener with an HTTP request phase — the
    /// HTTP Query API or the MCP streamable-HTTP transport. The admission
    /// deadlines' binding fact (ADR-0451): `--mcp stdio` and a bolt-only run
    /// bind neither, however many surfaces are compiled in. The
    /// classification lives on the [`SurfaceFlag`] rows, so this folds the
    /// table instead of naming surface flags again. Compiled only where an
    /// HTTP-based deadline flag exists; a bolt-only build has no consumer.
    #[cfg(any(feature = "http", feature = "mcp"))]
    pub(crate) fn binds_http_based_listener(&self) -> bool {
        SURFACE_FLAGS
            .iter()
            .any(|surface| surface.http_header_phase && (surface.binds)(self))
    }

    /// True when the command serves any surface at all — the any-surface
    /// fold (round 8, F-8-1): [`has_listener`]'s listener half extended
    /// with the stdio transport (`mcp.is_some()`, either transport).
    /// The parse's nothing-to-serve validation consults it; never
    /// re-derive the fold at a use site.
    pub(crate) fn has_surface(&self) -> bool {
        #[cfg(feature = "mcp")]
        let serves_stdio = self.is_mcp_stdio();
        #[cfg(not(feature = "mcp"))]
        let serves_stdio = false;
        // the listener half folds the compiled-surfaces table; a build
        // with no surface (which also has no stdio transport) serves
        // nothing, so the nothing-to-serve parse rejects every run
        #[cfg(feature = "surface")]
        let listener = self.has_listener();
        #[cfg(not(feature = "surface"))]
        let listener = false;
        listener || serves_stdio
    }

    /// True when the mcp stdio transport is selected — the foreground
    /// surface, the non-listener half of the surface fold. Compiled
    /// with the mcp feature only: without it no stdio transport
    /// exists, so neither the fact nor its consumers compile (round 3,
    /// F-3-7).
    #[cfg(feature = "mcp")]
    pub(crate) fn is_mcp_stdio(&self) -> bool {
        matches!(self.mcp, Some(McpMode::Stdio))
    }

    /// The stdio-only fact, derived exactly once: the mcp stdio transport
    /// AND no listener surface — such a run never touches the network, so
    /// setup stops at the open (round 2, F-2-6): the connection-cap
    /// resolution, the auth story, and the whole listener prologue never
    /// run. setup's `ReadyKind::StdioOnly` construction and run's stdio
    /// dispatch consult this method; the unbound-surface flag policy
    /// ([`unbound::reject`]) instead reads the more general
    /// no-listener fact. The `mcp`-stdio && no-listener conjunction is
    /// never hand-derived at a use site. Compiled with the mcp feature
    /// only: without it no stdio transport exists, so no serve can be
    /// stdio-only and neither this fact nor its consumers compile
    /// (round 3, F-3-7).
    #[cfg(feature = "mcp")]
    pub(crate) fn is_stdio_only(&self) -> bool {
        self.is_mcp_stdio() && !self.has_listener()
    }
}

/// Parse the argument tail after the `serve` verb. Usage errors map onto
/// the shared convention (`Error: ...`, the `serve` synopsis and hint;
/// exit 2) at the dispatch site.
pub(crate) fn parse_serve_args(rest: &[String]) -> Result<ServeCmd, ParseError> {
    let mut database = None;
    let mut host = None;
    #[cfg(feature = "http")]
    let mut http_port = None;
    #[cfg(feature = "http")]
    let mut origins = Vec::new();
    #[cfg(feature = "mcp")]
    let mut mcp = None;
    #[cfg(feature = "bolt")]
    let mut bolt_port = None;
    #[cfg(feature = "bolt")]
    let mut bolt_identity = None;
    let mut auth = None;
    let mut max_connections = None;
    #[cfg(feature = "bolt")]
    let mut ws_max_connections = None;
    #[cfg(feature = "bolt")]
    let mut ws_allow_origins = Vec::new();
    #[cfg(any(feature = "http", feature = "mcp"))]
    let mut header_deadline_secs = None;
    #[cfg(any(feature = "http", feature = "mcp"))]
    let mut body_deadline_secs = None;
    #[cfg(feature = "http")]
    let mut tx_idle_timeout_secs = None;
    #[cfg(feature = "http")]
    let mut max_open_transactions = None;
    let mut concurrent_logical_workspace_limit = None;
    let mut max_traversal_depth = None;
    let mut safe = None;
    let mut logical_graph_size_limit = None;
    let mut expression_memory_limit = None;
    let mut durability_mode = None;
    let mut snapshot_format = None;
    // The access flag is a post-verb arm here, and the local `access`
    // slot parks through the shared helpers — `parse_access_sighting` and
    // `set_access_once` in parse_error.rs own the grammar and the
    // once-only policy (ADR-0480).
    let mut access: Option<relate::OpenMode> = None;
    // `--load` (LABS-903, ADR-0500) parks the same way: once-only, its
    // resolution is the shared `crate::load`'s, never re-derived here.
    let mut load: Option<String> = None;
    let mut args = rest.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-d" | "--database" => {
                // one `value_flag` call like every value flag (round 1,
                // F-1-4, round 2): the once-only park, the missing-value
                // error, and the pass-through parse are the helper's
                value_flag(
                    &mut database,
                    "--database",
                    &mut args,
                    "a path",
                    parse_string,
                )?;
            }
            token if is_access_sighting(token) => {
                let next = args.clone().next().map(String::as_str);
                let freshness = parse_access_sighting(token, next)?;
                set_access_once("serve", &mut access, freshness)?;
            }
            "--load" => {
                value_flag(
                    &mut load,
                    "--load",
                    &mut args,
                    "a store source (a .rl path, a directory, or an https URL)",
                    parse_string,
                )?;
            }
            "--host" => {
                value_flag(&mut host, "--host", &mut args, "a host or IP", parse_host)?;
            }
            #[cfg(feature = "http")]
            "--http" => {
                value_flag(&mut http_port, "--http", &mut args, "a port", parse_port)?;
            }
            #[cfg(feature = "mcp")]
            "--mcp" => {
                value_flag(&mut mcp, "--mcp", &mut args, "stdio or a port", parse_mcp)?;
            }
            #[cfg(feature = "bolt")]
            "--bolt" => {
                value_flag(&mut bolt_port, "--bolt", &mut args, "a port", parse_port)?;
            }
            "--auth" => {
                value_flag(
                    &mut auth,
                    "--auth",
                    &mut args,
                    "USER:PASSWORD or none",
                    parse_string,
                )?;
            }
            "--max-connections" => {
                value_flag(
                    &mut max_connections,
                    "--max-connections",
                    &mut args,
                    "a count",
                    parse_count,
                )?;
            }
            #[cfg(feature = "bolt")]
            "--ws-max-connections" => {
                value_flag(
                    &mut ws_max_connections,
                    "--ws-max-connections",
                    &mut args,
                    "a count",
                    parse_count,
                )?;
            }
            #[cfg(feature = "bolt")]
            "--ws-allow-origin" => {
                repeatable_origin_flag(&mut args, "--ws-allow-origin", &mut ws_allow_origins)?;
            }
            #[cfg(any(feature = "http", feature = "mcp"))]
            "--header-deadline-secs" => {
                value_flag(
                    &mut header_deadline_secs,
                    "--header-deadline-secs",
                    &mut args,
                    "a count of seconds",
                    parse_count,
                )?;
            }
            #[cfg(any(feature = "http", feature = "mcp"))]
            "--body-deadline-secs" => {
                value_flag(
                    &mut body_deadline_secs,
                    "--body-deadline-secs",
                    &mut args,
                    "a count of seconds",
                    parse_count,
                )?;
            }
            #[cfg(feature = "http")]
            "--tx-idle-timeout-secs" => {
                value_flag(
                    &mut tx_idle_timeout_secs,
                    "--tx-idle-timeout-secs",
                    &mut args,
                    "a count of seconds",
                    parse_count,
                )?;
            }
            #[cfg(feature = "http")]
            "--max-open-transactions" => {
                value_flag(
                    &mut max_open_transactions,
                    "--max-open-transactions",
                    &mut args,
                    "a count",
                    parse_count,
                )?;
            }
            "--concurrent-logical-workspace-limit" => {
                value_flag(
                    &mut concurrent_logical_workspace_limit,
                    "--concurrent-logical-workspace-limit",
                    &mut args,
                    "a size",
                    parse_size,
                )?;
            }
            "--max-traversal-depth" => {
                value_flag(
                    &mut max_traversal_depth,
                    "--max-traversal-depth",
                    &mut args,
                    "a hop count (1 through 2000)",
                    parse_traversal_depth,
                )?;
            }
            "--expression-memory-limit" => {
                value_flag(
                    &mut expression_memory_limit,
                    "--expression-memory-limit",
                    &mut args,
                    "a size, or none",
                    parse_expression_size,
                )?;
            }
            "--safe" | "--no-safe" => {
                // LABS-680 (ADR-0477): value-less, last one wins.
                safe = Some(arg == "--safe");
            }
            "--logical-graph-size-limit" => {
                value_flag(
                    &mut logical_graph_size_limit,
                    "--logical-graph-size-limit",
                    &mut args,
                    "a size",
                    parse_size,
                )?;
            }
            "--durability-mode" => {
                value_flag(
                    &mut durability_mode,
                    "--durability-mode",
                    &mut args,
                    "a durability mode",
                    parse_durability_mode,
                )?;
            }
            "--snapshot-format" => {
                value_flag(
                    &mut snapshot_format,
                    "--snapshot-format",
                    &mut args,
                    &format!("a value ({})", crate::flags::snapshot_format_values(" or ")),
                    parse_snapshot_format,
                )?;
            }
            #[cfg(feature = "bolt")]
            "--bolt-identity" => {
                value_flag(
                    &mut bolt_identity,
                    "--bolt-identity",
                    &mut args,
                    "an agent string",
                    parse_string,
                )?;
            }
            #[cfg(feature = "http")]
            "--origin" => {
                repeatable_origin_flag(&mut args, "--origin", &mut origins)?;
            }
            other => {
                return Err(ParseError::unexpected("serve", other));
            }
        }
    }
    let Some(database) = database else {
        return Err("serve requires --database".into());
    };
    // LABS-903 (ADR-0500): the `--load` static flag conflicts, refused at
    // parse time — the shared policy's one statement
    // (`crate::load::parse_conflict`, the query path's own conflict
    // check), so the two surfaces refuse identically before any I/O.
    if let Some(msg) = crate::load::parse_conflict(&database, load.is_some(), access) {
        return Err(msg.into());
    }
    // The access build with the one `--load` implication
    // (`crate::load::access_mode`, the one open-mode resolution — the
    // query path's `Flags::open_mode` its sibling caller): a memory
    // turn-into opens as the ephemeral mode whatever the access slot said.
    let access = crate::load::access_mode(&database, load.is_some(), access);
    let cmd = ServeCmd {
        database,
        host: host.unwrap_or_else(|| DEFAULT_HOST.to_string()),
        #[cfg(feature = "http")]
        http_port,
        #[cfg(feature = "mcp")]
        mcp,
        #[cfg(feature = "bolt")]
        bolt_port,
        auth,
        #[cfg(feature = "http")]
        origins,
        #[cfg(feature = "bolt")]
        bolt_identity,
        max_connections,
        #[cfg(feature = "bolt")]
        ws_max_connections,
        #[cfg(feature = "bolt")]
        ws_allow_origins,
        #[cfg(any(feature = "http", feature = "mcp"))]
        header_deadline_secs,
        #[cfg(any(feature = "http", feature = "mcp"))]
        body_deadline_secs,
        #[cfg(feature = "http")]
        tx_idle_timeout_secs,
        #[cfg(feature = "http")]
        max_open_transactions,
        concurrent_logical_workspace_limit,
        max_traversal_depth,
        safe,
        logical_graph_size_limit,
        expression_memory_limit,
        durability_mode,
        snapshot_format,
        access,
        load,
    };
    // The parse's two cross-flag validations, on the built value through
    // its methods — the surface facts' one derivations. Nothing-to-serve
    // first: no surface at all ([`ServeCmd::has_surface`]) is nothing
    // to serve.
    if !cmd.has_surface() {
        return Err(format!("serve has nothing to serve: {}", nothing_to_serve_hint()).into());
    }
    // Then the unbound-surface flag policy ([`unbound::reject`],
    // the one statement of it): a flag whose surface this run does not serve
    // is rejected loudly rather than silently ignored — the stdio-only
    // rejection generalized to every serve (round 1, F-1-6). A serve-only
    // build has no surface, so there is nothing to reject.
    #[cfg(feature = "surface")]
    unbound::reject(&cmd)?;
    Ok(cmd)
}

/// One compiled surface flag's facts (round 3, F-3-2): its command-line
/// spelling, how the messages name the listener it binds, and the value it
/// takes in the "nothing to serve" hint. The row is the one place a
/// surface flag's specialness is stated — [`surface_hint`] and the tests
/// project from `flag`/`value_hint` in every build, and the mcp build's
/// inert-flag messages project from `listener_spelling`
/// ([`listener_flag_spellings`]) — no use site re-derives a fact with its
/// own `if flag == "--mcp"` conditional.
// Compiled only beside a surface (LABS-638): the bare `serve` bundle's
// vocabulary is empty and its nothing-to-serve message is authored, so
// neither the type nor the table exists there.
#[cfg(feature = "surface")]
pub(crate) struct SurfaceFlag {
    /// The flag's command-line spelling.
    pub flag: &'static str,
    /// How the inert-flag and unbound-value messages name the listener this
    /// flag binds: a port surface renders as itself with its value
    /// (`--http PORT`, `--mcp PORT`, `--bolt`); `--mcp` spells out its port
    /// form, disambiguating the stdio transport, which is not a listener.
    /// Compiled with the surfaces that render it into a message.
    #[cfg(any(feature = "http", feature = "mcp"))]
    pub listener_spelling: &'static str,
    /// The value the flag takes in the "nothing to serve" hint: a port
    /// for a listener; `--mcp` also takes the `stdio` keyword (LABS-381,
    /// ADR-0368), so its hint names both spellings.
    pub value_hint: &'static str,
    /// Whether this surface flag binds a listener on `cmd` — the row's own
    /// derivation, delegating to the surface's one predicate
    /// ([`ServeCmd::binds_http`] / [`ServeCmd::binds_bolt`] /
    /// [`ServeCmd::binds_mcp_listener`]). [`ServeCmd::has_listener`] folds
    /// these predicates, so the listener vocabulary lives in one table
    /// instead of a cfg-shadowed parallel fold.
    pub binds: fn(&ServeCmd) -> bool,
    /// Whether this listener reads an HTTP request header/body phase — the
    /// admission-deadline classification (ADR-0451). `--http` and the MCP
    /// listener are true; Bolt's handshake deadline is its own (ADR-0141).
    /// Both the "binds an HTTP-based listener" predicate and the deadline
    /// flags' unbound message derive from this column.
    #[cfg(any(feature = "http", feature = "mcp"))]
    pub http_header_phase: bool,
}

/// The surface flags compiled into this build — the one compiled-surfaces
/// fact, consulted at every use site: the "nothing to serve" hint renders
/// the entries and the tests' any-surface-flag helper returns from the
/// list. Each surface feature implies `serve`, but the bare `serve`
/// bundle (ADR-0232) is a serve with no surface flag at all — the
/// compile gate without a surface — so the list is empty there and the
/// surface-flag tests that depend on it are compiled out (see
/// [`any_surface_flag`]'s gating).
#[cfg(feature = "surface")]
pub(crate) const SURFACE_FLAGS: &[SurfaceFlag] = &[
    #[cfg(feature = "http")]
    SurfaceFlag {
        flag: "--http",
        #[cfg(any(feature = "http", feature = "mcp"))]
        listener_spelling: "--http PORT",
        value_hint: "PORT",
        binds: ServeCmd::binds_http,
        #[cfg(any(feature = "http", feature = "mcp"))]
        http_header_phase: true,
    },
    #[cfg(feature = "mcp")]
    SurfaceFlag {
        flag: "--mcp",
        #[cfg(any(feature = "http", feature = "mcp"))]
        listener_spelling: "--mcp PORT",
        value_hint: "PORT|stdio",
        binds: ServeCmd::binds_mcp_listener,
        #[cfg(any(feature = "http", feature = "mcp"))]
        http_header_phase: true,
    },
    #[cfg(feature = "bolt")]
    SurfaceFlag {
        flag: "--bolt",
        #[cfg(any(feature = "http", feature = "mcp"))]
        listener_spelling: "--bolt",
        value_hint: "PORT",
        binds: ServeCmd::binds_bolt,
        #[cfg(any(feature = "http", feature = "mcp"))]
        http_header_phase: false,
    },
];

/// The nothing-to-serve message's tail: the compiled vocabulary when
/// non-empty, else the authored no-surface text — data-driven, so no
/// feature combination can interpolate an empty list (not even the
/// out-of-contract `serve,surface` marker-alone build, whose table is
/// empty while the marker is on; LABS-638).
fn nothing_to_serve_hint() -> String {
    #[cfg(feature = "surface")]
    // clippy evaluates this per feature combination, where the table is
    // a known-size const; the guard exists for the out-of-contract
    // `serve,surface` build (marker on, table empty) that no single
    // combination's lint can see
    #[allow(clippy::const_is_empty)]
    if !SURFACE_FLAGS.is_empty() {
        return format!("pass {}", surface_hint());
    }
    "this build compiled no serve surface (enable the http, mcp, or bolt feature)".to_string()
}

/// The "nothing to serve" hint, naming exactly the surface flags compiled
/// into this build, each with the value it takes. Compiled only beside a
/// surface: the bare `serve` bundle has an empty vocabulary, and its
/// nothing-to-serve message is authored, not interpolated (LABS-638).
#[cfg(feature = "surface")]
pub(crate) fn surface_hint() -> String {
    SURFACE_FLAGS
        .iter()
        .map(|surface| format!("{} {}", surface.flag, surface.value_hint))
        .collect::<Vec<_>>()
        .join(" and/or ")
}

/// The surface flag of whichever surface this build compiled in, for
/// tests that need any one surface flag: the first entry of the one
/// compiled-surfaces list ([`SURFACE_FLAGS`]) — the canonical answer,
/// shared by this module's tests, the flags tests, and the serve_cmd
/// tests. Compiled only beside a surface (the same gating `mod unbound`
/// carries): the bare `serve` bundle (ADR-0232) has no surface flag, so
/// the helper and every test that needs one are absent from that
/// bundle rather than skipping at runtime.
#[cfg(all(test, feature = "surface"))]
pub(crate) fn any_surface_flag() -> &'static str {
    SURFACE_FLAGS
        .first()
        .expect("the cfg gate compiles a surface flag")
        .flag
}

/// The flag of an HTTP-based surface — the `http_header_phase` row,
/// `--http` or `--mcp` — for tests that need a listener with request
/// phases: selected by the table's own classification, not by table
/// order (the same field cfg gates this helper's compilation).
#[cfg(all(test, any(feature = "http", feature = "mcp")))]
pub(crate) fn http_based_surface_flag() -> &'static str {
    SURFACE_FLAGS
        .iter()
        .find(|surface| surface.http_header_phase)
        .expect("the cfg gate compiles an HTTP-based surface")
        .flag
}

/// `--expression-memory-limit`'s value parse (ADR-0463): the query path's
/// `parse_expression_memory_limit` grammar — bytes, mb/mib/gb/gib, or the
/// `none`/`0` opt-out — with the module's "serve {flag} expects …" policy.
fn parse_expression_size(flag: &str, value: &str) -> Result<u64, ParseError> {
    relate::parse_expression_memory_limit(value).map_err(|e| {
        format!("serve {flag} expects a size (bytes, mb/mib/gb/gib, or none), got {value:?}: {e}")
            .into()
    })
}

/// `--logical-graph-size-limit`'s value parse (ADR-0390): the query path's size
/// grammar through `relate::parse_memory_limit`, with the module's
/// "serve {flag} expects …, got {value:?}" message policy stated once —
/// the named sibling of [`parse_count`], so every value-taking serve flag
/// routes through a named parser.
fn parse_size(flag: &str, value: &str) -> Result<u64, ParseError> {
    relate::parse_memory_limit(value).map_err(|e| {
        format!("serve {flag} expects a size (bytes or mb/mib/gb/gib), got {value:?}: {e}").into()
    })
}

fn parse_count(flag: &str, value: &str) -> Result<u64, ParseError> {
    value
        .parse::<u64>()
        .map_err(|_| format!("serve {flag} expects a non-negative integer, got {value:?}").into())
}

/// `--max-traversal-depth`'s value (LABS-610): an integer in
/// `1..=MAX_TRAVERSAL_DEPTH_CEILING`, with the same named-parser message
/// policy as [`parse_count`].
fn parse_traversal_depth(flag: &str, value: &str) -> Result<u32, ParseError> {
    let n = value
        .parse::<u32>()
        .map_err(|_| format!("serve {flag} expects a non-negative integer, got {value:?}"))?;
    if (1..=relate::MAX_TRAVERSAL_DEPTH_CEILING).contains(&n) {
        Ok(n)
    } else {
        Err(format!(
            "serve {flag} expects 1 through {}, got {value:?}",
            relate::MAX_TRAVERSAL_DEPTH_CEILING
        )
        .into())
    }
}

/// `--durability-mode`'s value (ADR-0470): the engine's mode grammar
/// through `DurabilityMode::from_str` — the one statement of the
/// `full`/`everysecond`/`snapshot` vocabulary — with the module's
/// named-parser message policy: the engine's own reason names the
/// expected values, so this wrapper only names the flag and the value.
fn parse_durability_mode(flag: &str, value: &str) -> Result<relate::DurabilityMode, ParseError> {
    value
        .parse::<relate::DurabilityMode>()
        .map_err(|e| format!("serve {flag} expects a durability mode, got {value:?}: {e}").into())
}

// ADR-0534: the flag's values are the codec names plus `parquet-wide`,
// the same grammar the query session's `--snapshot-format` parses
// (flags.rs), so serve and the query session cannot drift on one store.
fn parse_snapshot_format(
    flag: &str,
    value: &str,
) -> Result<crate::flags::SnapshotFormat, ParseError> {
    value
        .parse::<crate::flags::SnapshotFormat>()
        .map_err(|e| format!("serve {flag} expects a snapshot format, got {value:?}: {e}").into())
}

// Live only through a surface feature (the `--http`/`--bolt` port flags
// and mcp's keyword/port split); a serve-only build knows no port flag
// (LABS-638).
#[cfg(feature = "surface")]
fn parse_port(flag: &str, value: &str) -> Result<u16, ParseError> {
    // The parser proper is relate-serve's, shared with the container
    // entrypoint; here only the error type adapts.
    relate_serve::parse_port(flag, value).map_err(Into::into)
}

/// The `--mcp` value (LABS-381, ADR-0368): the `stdio` keyword selects the
/// foreground stdio transport; anything else is the streamable-HTTP transport's port
/// through the shared port parser — a typo like `stio` or `standrd` is a
/// loud usage error, never a silent default. The parse is pure so the
/// keyword/port split is unit-testable without a process.
#[cfg(feature = "mcp")]
fn parse_mcp(flag: &str, value: &str) -> Result<McpMode, ParseError> {
    if value == "stdio" {
        return Ok(McpMode::Stdio);
    }
    match parse_port(flag, value) {
        Ok(port) => Ok(McpMode::Http(port)),
        // The shared port parser's own reason (non-numeric, out of range) is
        // kept verbatim; the wrapper only adds the keyword the port-shaped
        // error cannot know about.
        Err(err) => Err(format!("{err} (or stdio)").into()),
    }
}

/// [`value_flag`]'s pass-through parser for a string-valued flag (`--auth`,
/// `--bolt-identity`).
fn parse_string(_flag: &str, value: &str) -> Result<String, ParseError> {
    Ok(value.to_string())
}

/// [`value_flag`]'s host parser: the one `--host` normalization.
fn parse_host(_flag: &str, value: &str) -> Result<String, ParseError> {
    Ok(normalize_host(value))
}

/// The one browser-Origin value parse both origin flags share —
/// `--origin` and `--ws-allow-origin` (round 3, F-1-5; ADR-0459): the
/// module's named-parser pattern, the `parse_size`/`parse_count`/`parse_mcp`
/// sibling. The validity grammar is the shared
/// [`OriginFlagValue::parse`] (round 3, F-1-9); this wrapper only names the
/// flag in the error, the module's message policy, and yields the grammar's
/// typed value so the parse's classification flows into
/// [`ServeCmd::origins`] / [`ServeCmd::ws_allow_origins`] and the policies
/// they build — no builder re-tests the strings (round 4, F-4-2). Pure so
/// the grammar is unit-testable without a process.
#[cfg(any(feature = "http", feature = "bolt"))]
fn parse_origin(flag: &str, value: &str) -> Result<OriginFlagValue, ParseError> {
    OriginFlagValue::parse(value).map_err(|reason| format!("serve {flag} {reason}").into())
}

/// The repeatable origin flags' one parse-and-push — the accumulating
/// sibling of [`value_flag`] (`--origin` and `--ws-allow-origin`): take the
/// flag's value, parse it through [`parse_origin`], and append it. The
/// flags accumulate by design, so there is no once-only check here: an
/// explicit list replaces the surface's default at the policy builder.
#[cfg(any(feature = "http", feature = "bolt"))]
fn repeatable_origin_flag<'a>(
    args: &mut impl Iterator<Item = &'a String>,
    flag: &str,
    values: &mut Vec<OriginFlagValue>,
) -> Result<(), ParseError> {
    let value = flag_value(args, flag, "an origin URL or *")?;
    values.push(parse_origin(flag, value)?);
    Ok(())
}

/// The value-taking flags' one parse-and-set, generic over the slot type:
/// take the flag's value, parse it with the grammar the caller names
/// ([`parse_count`], [`parse_size`], [`parse_port`], …), and park it through
/// the once-only policy.
fn value_flag<'a, T>(
    slot: &mut Option<T>,
    flag: &str,
    args: &mut impl Iterator<Item = &'a String>,
    value_hint: &str,
    parse: fn(&str, &str) -> Result<T, ParseError>,
) -> Result<(), ParseError> {
    let value = flag_value(args, flag, value_hint)?;
    set_once("serve", slot, flag, parse(flag, value)?)
}

// The once-only policy for `serve`'s flags is parse_error's one
// `set_once` (LABS-465's canonical helper, reached from here since
// LABS-638) and, for the one access flag, `access_once` (ADR-0480 §4):
// a repeat is a usage error, value-taking flags park their value in the
// slot — no local copy of either policy.

// serve_args's tests live in `tests/` (the skill_cmd dir-module pattern),
// split by policy into grammar and unbound-flag submodules so each file stays
// under the repo's 1k-line budget.
#[cfg(test)]
mod tests;
