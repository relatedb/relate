//! `relate serve --database PATH [--http PORT] [--mcp PORT|stdio] [--bolt PORT] [--read-only[=FRESHNESS]] [--auth USER:PASSWORD] [--concurrent-logical-workspace-limit SIZE] [--max-traversal-depth N] [--logical-graph-size-limit SIZE] [--durability-mode MODE]`
//! (ADR-0232): host Relate's network surfaces in-process on one open
//! store until SIGINT/SIGTERM.
//!
//! - `--http PORT` serves the HTTP Query API v2 statement endpoint
//!   (`surfaces/relate-http`), `--mcp PORT` the MCP server on its
//!   streamable-HTTP transport (`surfaces/relate-mcp`), `--bolt PORT`
//!   the Bolt server (`surfaces/relate-bolt`; LABS-298 — cheap since
//!   ADR-0233 removed bolt's async runtime). Each flag is compiled in with
//!   its Cargo feature; with no surface flag at all the command is a usage
//!   error — there is nothing to serve.
//! - `--mcp stdio` (LABS-381, ADR-0368) serves the MCP stdio transport in
//!   the foreground — newline-delimited JSON-RPC on the process's
//!   stdin/stdout, the way MCP hosts spawn a server — until the host
//!   closes stdin, then the store takes its final checkpoint and the
//!   process exits 0. The stdio transport is a foreground surface: it runs
//!   on the main thread after every listener surface is spawned, and
//!   stdin EOF (not SIGINT) is its shutdown. Stdio skips the auth story
//!   entirely (ADR-0232 §1 — the host spawned us, so there is no trust
//!   boundary): a `--mcp stdio` run with no listener surface and an
//!   explicit `--auth` is a usage error rather than an ignored flag.
//! - `--read-only[=FRESHNESS]` (ADR-0306, the ADR-0480 spelling) sets the
//!   open's access mode — the same opens as `relate query` (ADR-0136,
//!   ADR-0137): read-only takes no writer lease and never creates the
//!   store; `=live` (the former `--follow`) tails the WAL beside a
//!   writable open. Post-verb, the value attached only, and once-only; the
//!   banner names a non-writable mode, and the MCP surface hides
//!   `write-cypher` on a read-only handle.
//! - `--load SOURCE` (LABS-903, ADR-0500) is the two-axis store grammar's
//!   source half, resolved by the shared `crate::load` exactly as the
//!   query session resolves it: a `:memory:` target turns the source into
//!   the in-memory store (read-only by construction — the source is
//!   released after the load, no lock held, and a fetched copy is removed
//!   right after it), a path target is created from the source and opened
//!   normally. `--read-only` beside a durable `--load` target, and
//!   `--read-only=live` with any `--load`, are usage errors at parse time.
//! - `--logical-graph-size-limit SIZE` (ADR-0390, the ADR-0368 follow-up) caps the
//!   served store's live graph + index bytes — the query session's
//!   storage flag with its shared resolution (`--logical-graph-size-limit` >
//!   `RELATE_LOGICAL_GRAPH_SIZE_LIMIT` > unlimited), applied to the serve open
//!   exactly as `relate query` applies it to its own. The memory-shape
//!   loadtest's bolt cases pass it (LABS-424).
//! - `--concurrent-logical-workspace-limit SIZE` (ADR-0452) is that same
//!   shape for the executing-query pool: a positive value turns admission
//!   on, unset leaves it off, and the resolution is the query session's
//!   (`--concurrent-logical-workspace-limit` >
//!   `RELATE_CONCURRENT_LOGICAL_WORKSPACE_LIMIT` > off).
//! - `--max-traversal-depth N` (LABS-610, ADR-0466) is the served store's
//!   traversal-depth admission limit: an explicit `*1..N` / `{1,N}` hop
//!   bound above it fails the statement at admission. Values are 1 through
//!   2000; unset uses `RELATE_MAX_TRAVERSAL_DEPTH`, then 500. The
//!   resolution is the query session's.
//! - `--durability-mode MODE` (ADR-0470) is the served store's commit
//!   durability policy (ADR-0077): `full`, `everysecond`, or `snapshot`.
//!   Unset uses `RELATE_DURABILITY_MODE`, then `full`. The resolution
//!   is the query session's (`flags::durability_mode_from_env_checked`).
//! - Ports carry no defaults: the flag's value is the port. Every surface
//!   binds `--host` (default `127.0.0.1`, loopback only); a non-loopback
//!   host is deliberate plaintext exposure and gets a loud startup warning
//!   (ADR-0248). A taken port is a loud startup error before any banner
//!   prints.
//! - Both surfaces share one auth resolution (`relate_auth::resolve`:
//!   `--auth` > `RELATE_AUTH` > generated token) and enforce it
//!   identically — the RFC 7617 `Authorization: Basic` header on every
//!   request. The banner tells the token story once for the server. Auth
//!   is the last fallible setup step — after the open, the binds, and the
//!   Bolt identity read — so the generated token file is persisted only
//!   once every other setup step has succeeded.
//! - SIGINT/SIGTERM shut the server down: the accept loops stop with the
//!   process and a persistent, writable store takes its final checkpoint
//!   (the same `session::final_checkpoint` policy the query session applies).

#[cfg(feature = "surface")]
use std::net::TcpListener;
use std::sync::Arc;

#[cfg(feature = "mcp")]
use crate::serve_args::McpMode;
use crate::serve_args::ServeCmd;

#[cfg(feature = "surface")]
mod spawn;

#[cfg(feature = "bolt")]
use spawn::BoltSurface;
#[cfg(feature = "http")]
use spawn::HttpSurface;
#[cfg(feature = "mcp")]
use spawn::{McpRole, McpSurface};

/// True when `host` names a loopback interface — the shared
/// `relate-serve` predicate (parse-based; a `127.0.0.1.evil.com`
/// hostname must not pass), re-used by the container entrypoint so the two
/// loopback postures cannot drift.
use relate_serve::is_loopback_host;

/// The ADR-0306 §5 startup banner line for a non-writable access mode:
/// exactly one line naming the mode. `None` for a writable serve, which
/// adds no banner line. [`setup`] fronts the line onto the banner Vec;
/// [`run`]'s one banner loop prints it.
fn access_banner_line(access: relate::OpenMode) -> Option<&'static str> {
    match access {
        relate::OpenMode::Writable => None,
        relate::OpenMode::ReadOnly => Some(
            "serve: opened the store read-only — no writer lease and \
             never creates the store; every write fails",
        ),
        relate::OpenMode::Follow => Some(
            "serve: opened the store read-only and following the WAL — \
             the writer's committed frames appear as they land",
        ),
        // LABS-903 (ADR-0500): the `--load` memory turn-into — the source
        // loaded into the in-memory store, released after the load.
        relate::OpenMode::Ephemeral => Some(
            "serve: loaded the store into memory — read-only, the source \
             released after the load; every write fails",
        ),
    }
}

/// The loud plaintext-posture warning for a non-loopback `--host`
/// (ADR-0248): `serve` has no TLS, so binding beyond loopback exposes the
/// surfaces — under the one shared credential — to the network. `None` on
/// loopback.
fn non_loopback_warning(host: &str) -> Option<String> {
    if is_loopback_host(host) {
        return None;
    }
    Some(format!(
        "serve: WARNING: --host {host} exposes the PLAINTEXT HTTP/MCP/Bolt surfaces to the network — \
         serve has no TLS; anyone who can reach the ports can present credentials. \
         Keep 127.0.0.1 or front the server with TLS."
    ))
}

/// Render `--host` inside a URL — the three surface banner lines and the
/// discovery document's `bolt_direct` endpoint: an IPv6 literal must be
/// bracketed (`http://::1:7474` is not a URL; `http://[::1]:7474` is),
/// while a hostname or IPv4 renders as-is. The bind itself takes the bare
/// literal (`TcpListener::bind`), so only the rendering brackets. The
/// `:` test is the URL rule, not a shape sniff: a registered name never
/// contains a colon, and a scoped literal (`fe80::1%en0`) brackets too.
// Live only through a surface feature (the banner/discovery rendering is
// surface code); a serve-only build compiles no caller (LABS-638).
#[cfg(feature = "surface")]
fn url_host(host: &str) -> String {
    if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

/// The Bolt surface's HELLO `server` agent string (ADR-0293): the
/// `--bolt-identity` flag, else `RELATE_IDENTITY`, else the honest
/// `Relate/<version>` default — the same precedence as serve's auth
/// (flag > env > default). Pure on its inputs so the precedence is
/// unit-tested without touching the process environment.
#[cfg(feature = "bolt")]
fn resolve_bolt_identity(flag: Option<&str>, env: Option<&str>) -> String {
    flag.or(env)
        .map(str::to_string)
        .unwrap_or_else(relate_bolt::protocol::default_server_identity)
}

/// Bind a listener on `--host` for one surface, or report the bind failure
/// (EADDRINUSE and friends are loud startup errors, exit 1). On success the
/// **bound** port rides with the listener — for `port` 0 that is the
/// ephemeral port the OS assigned, so the banner never announces `:0`.
// Live only through a surface feature (every bind site is a surface's
// setup); a serve-only build binds nothing (LABS-638).
#[cfg(feature = "surface")]
fn bind(flag: &str, host: &str, port: u16) -> Result<(u16, TcpListener), i32> {
    match TcpListener::bind((host, port)) {
        Ok(listener) => {
            let bound = listener.local_addr().map_err(|e| {
                crate::print_error(format_args!(
                    "Error: serve {flag} {port}: cannot read the bound address: {e}"
                ));
                1
            })?;
            Ok((bound.port(), listener))
        }
        Err(e) => {
            crate::print_error(format_args!(
                "Error: serve {flag} {port}: cannot bind {host}:{port}: {e}"
            ));
            Err(1)
        }
    }
}

/// Unwrap a setup step or print its error and return the exit code: the one
/// shape every `serve` setup failure takes. Exit code 1 (a startup
/// failure); [`or_report_as`] is the same epilogue with a caller-chosen
/// code.
fn or_report<T>(result: Result<T, impl std::fmt::Display>) -> Result<T, i32> {
    or_report_as(1, result)
}

/// [`or_report`]'s epilogue with the exit code the failure carries — the
/// one print-and-return shape, parameterized (round 2, F-2-1): a usage-error
/// class failing through setup takes 2 here instead of hand-rolling a
/// second epilogue beside the one this module states once. Delegates to
/// [`or_report_with`], the one body.
fn or_report_as<T>(code: i32, result: Result<T, impl std::fmt::Display>) -> Result<T, i32> {
    or_report_with(result, |_| code)
}

/// [`or_report`]'s epilogue with the exit code taken from the error itself
/// (round 4, F-3-3): a resolution whose error classes split across codes —
/// [`crate::load::resolve`]'s usage-2 vs runtime-1 — keeps the module's
/// one print-and-return shape instead of a bespoke `.map_err` beside it.
/// The one body both fixed-code forms delegate to.
fn or_report_with<T, E: std::fmt::Display>(
    result: Result<T, E>,
    code: impl FnOnce(&E) -> i32,
) -> Result<T, i32> {
    result.map_err(|e| {
        crate::print_error(format_args!("Error: {e}"));
        code(&e)
    })
}

/// The serve [`setup`] prepared, and everything [`run`] needs for it: the
/// kind-invariant fields — the opened store and its banner — hoisted into
/// the struct, the kind carried in [`ReadyKind`] beside the fields only
/// one kind reads (round 2, F-2-5, so `banner` is plain field access and
/// no pass-through match re-states the shared fields per variant).
struct Ready {
    /// The opened store.
    db: Arc<relate::Database>,
    /// The banner lines, for run's one pre-dispatch print. On a Listening
    /// run the assembled listener prologue: the ADR-0248 host warning
    /// fronts the Vec — listener-prologue content, assembled in
    /// [`setup`] where the listeners bind — then the ADR-0306 §5
    /// access-mode line, then the auth lines, then the ADR-0333
    /// unlimited-cap warning when it fires (also [`setup`]'s, so run
    /// never re-derives a prologue input). A stdio-only run binds no
    /// listener, so its banner is the access-mode line alone and carries
    /// no warning (round 3, F-1-3).
    banner: Vec<String>,
    kind: ReadyKind,
}

/// The kind of serve [`setup`] prepared — the per-kind story beside
/// [`Ready`]'s shared fields. The stdio-only kind holds nothing extra: no
/// listener surface exists, so no auth, connection cap, or bound surface
/// does. The Listening kind holds the resolved auth and the connection
/// cap — non-optional, because every build that compiles a surface can
/// bind a listener that admits connections, and setup resolved the cap
/// before auth (a malformed RELATE_MAX_CONNECTIONS is a startup
/// failure, never a silent unlimited) — beside the bound surfaces, each
/// `None` when that surface was not requested. The bare-`serve` bundle
/// is the recorded exception (LABS-638): no surface flag even parses
/// there — parse_serve_args rejects every invocation with
/// nothing-to-serve — so no serve-only run ever reaches this kind; the
/// unconditional auth/admission fields and the named drop in [`run`]
/// keep the no-surface combination typechecking under the serve clippy
/// lane (LABS-648 records the now-vacuous auth question).
enum ReadyKind {
    /// A `--mcp stdio` serve with no listener surface
    /// ([`ServeCmd::is_stdio_only`]): the whole run is the foreground
    /// stdio transport — stdin EOF, not a signal, is the lifecycle — and
    /// the listener prologue (auth and the unlimited-cap warning) does not
    /// exist for it. The admission knobs still resolve before this kind is
    /// chosen, so their startup validation is run-kind independent.
    #[cfg(feature = "mcp")]
    StdioOnly,
    /// A serve with at least one listener surface bound. The auth and
    /// admission configs compile only beside a surface (their every
    /// reader is a listener spawner, LABS-638) — the bare `serve`
    /// bundle typechecks the variant fieldless and config-less, and
    /// never constructs it at runtime anyway: its parse rejects every
    /// invocation with nothing-to-serve.
    Listening {
        #[cfg(feature = "surface")]
        auth: relate_auth::AuthConfig,
        /// The resolved connection cap, WebSocket session cap and
        /// header/body admission deadlines (ADR-0333, ADR-0451, ADR-0459),
        /// carried to every listener spawner. Boxed so the resolved
        /// admission bundle does not inflate this variant beside the
        /// zero-sized [`Self::StdioOnly`].
        #[cfg(feature = "surface")]
        admission: Box<Admission>,
        /// The resolved HTTP managed-transaction limits (ADR-0435 §4),
        /// carried from the pre-auth resolution in [`setup`] to the HTTP
        /// surface's spawn, where the registry is constructed from them.
        #[cfg(feature = "http")]
        tx_limits: relate_http::TxLimits,
        #[cfg(feature = "http")]
        http: Option<HttpSurface>,
        /// The MCP role (round 1, F-1-5): absent, a bound listener, or the
        /// foreground stdio transport — one typed three-state value, so
        /// run's arm dispatches on the role and never reaches back into
        /// `cmd` for the serve shape.
        #[cfg(feature = "mcp")]
        mcp: McpRole,
        #[cfg(feature = "bolt")]
        bolt: Option<BoltSurface>,
    },
}

/// The resolved admission limits the listener spawners share: the
/// process-wide connection cap (ADR-0333), the opt-in WebSocket session cap
/// (ADR-0459), and the absolute header and body admission deadlines of the
/// HTTP-based listeners (ADR-0451). One value keeps the spawners' argument
/// lists small and makes "the cap every listener gets" and "the deadlines
/// every HTTP listener gets" travel together. Origin allowlists are command
/// policy, not admission limits: their spawners receive the parsed
/// `ServeCmd` values directly (`--origin` for HTTP, `--ws-allow-origin` for
/// Bolt).
struct Admission {
    connection_cap: Arc<relate_serve::ConnectionCap>,
    /// The opt-in WebSocket session cap (ADR-0459); `0` leaves WebSocket
    /// sessions uncapped beyond [`Self::connection_cap`]. Bolt-only, so the
    /// field compiles with the bolt surface.
    #[cfg(feature = "bolt")]
    ws_max_connections: usize,
    #[cfg(any(feature = "http", feature = "mcp"))]
    header_deadline: std::time::Duration,
    #[cfg(any(feature = "http", feature = "mcp"))]
    body_deadline: std::time::Duration,
}

impl Admission {
    /// Resolve every admission limit as one step, before auth: a malformed
    /// `RELATE_MAX_CONNECTIONS`, `RELATE_WS_MAX_CONNECTIONS`,
    /// `RELATE_HEADER_DEADLINE_SECS`, or `RELATE_BODY_DEADLINE_SECS`
    /// fails startup before the generated-token path persists anything. The
    /// single constructor makes "resolved together" structural; [`setup`]
    /// cannot resolve one and forget the other.
    fn resolve(cmd: &ServeCmd) -> Result<Self, i32> {
        // `--max-connections`, else `RELATE_MAX_CONNECTIONS`, else the
        // process-wide default (ADR-0451) — the ladder itself lives in
        // `relate_serve::resolve_connection_cap`.
        let max = or_report(relate_serve::resolve_connection_cap(cmd.max_connections))?;
        Ok(Self {
            connection_cap: Arc::new(relate_serve::ConnectionCap::new(max)),
            // `--ws-max-connections`, else `RELATE_WS_MAX_CONNECTIONS`,
            // else unlimited (ADR-0459).
            #[cfg(feature = "bolt")]
            ws_max_connections: or_report(relate_serve::resolve_ws_connection_cap(
                cmd.ws_max_connections,
            ))?,
            #[cfg(any(feature = "http", feature = "mcp"))]
            header_deadline: or_report(relate_serve::resolve_header_deadline(
                cmd.header_deadline_secs,
            ))?,
            #[cfg(any(feature = "http", feature = "mcp"))]
            body_deadline: or_report(relate_serve::resolve_body_deadline(cmd.body_deadline_secs))?,
        })
    }
}

/// The served store's `OpenOptions`, built as one fallible step
/// (ADR-0306 §2, ADR-0390, ADR-0452, ADR-0463, LABS-610, LABS-680,
/// ADR-0470): the parsed access mode — the same opens as `relate
/// query` (ADR-0136, ADR-0137) — plus every storage knob the command
/// carries, each resolved
/// through its one canonical `flags::*_from_env_checked` helper so serve
/// and the query session cannot drift on the same store. A bad value
/// (flag or env) fails through setup's own `Err(code)` contract (exit 2,
/// the query path's usage-error code, via `or_report_as`), so the serve
/// run's telemetry flush still runs — never a bare `process::exit` inside
/// setup. One constructor keeps "resolved together" structural: [`setup`]
/// cannot resolve one knob and forget another, and the tests assert the
/// built options directly.
fn store_options(cmd: &ServeCmd) -> Result<relate::OpenOptions, i32> {
    let mut options = relate::OpenOptions::with_mode(cmd.access);
    // ADR-0452: the workspace pool is the query path's flag, resolved the
    // same way as the live-graph cap below — flag over env over off — so
    // a bad value fails here (exit 2) rather than as a failed open.
    options.concurrent_logical_workspace_limit = or_report_as(
        2,
        crate::flags::concurrent_logical_workspace_limit_from_env_checked(
            cmd.concurrent_logical_workspace_limit,
        ),
    )?;
    // ADR-0390 (the ADR-0368 follow-up): the served store takes the query
    // path's logical-graph-size-limit flag with its shared resolution —
    // `--logical-graph-size-limit` over `RELATE_LOGICAL_GRAPH_SIZE_LIMIT`
    // over unlimited. The storage flag the memory-shape loadtest's bolt
    // cases pass (LABS-424).
    options.logical_graph_size_limit = or_report_as(
        2,
        crate::flags::logical_graph_size_limit_from_env_checked(cmd.logical_graph_size_limit),
    )?;
    // LABS-610: the traversal-depth admission limit resolves the same way
    // (`--max-traversal-depth` over `RELATE_MAX_TRAVERSAL_DEPTH` over
    // the engine default 500), so a bad value is a usage error here rather
    // than a failed open.
    options.max_traversal_depth = or_report_as(
        2,
        crate::flags::max_traversal_depth_from_env_checked(cmd.max_traversal_depth),
    )?;
    // ADR-0463: the per-allocation expression cap, the same open knob ladder
    // as the live-graph limit (flag, then env, then the engine default).
    options.expression_memory_limit = or_report_as(
        2,
        crate::flags::expression_memory_limit_from_env_checked(cmd.expression_memory_limit),
    )?;
    // LABS-680 (ADR-0477): safe mode resolves with the same ladder
    // (`--safe`/`--no-safe` over `RELATE_SAFE_MODE` over off), so a bad
    // environment value is a usage error here rather than a failed open.
    options.safe_mode = or_report_as(2, crate::flags::safe_mode_from_env_checked(cmd.safe))?;
    // ADR-0470: the commit durability policy resolves the same way
    // (`--durability-mode` over `RELATE_DURABILITY_MODE` over the
    // engine default `full`), so a bad value is a usage error here rather
    // than a failed open — the storage flag the Jepsen matrix passes
    // (LABS-653).
    options.durability_mode = or_report_as(
        2,
        crate::flags::durability_mode_from_env_checked(cmd.durability_mode),
    )?;
    // ADR-0163: the served store's snapshot format — the same grammar
    // the query session's flag parses, resolved onto the one open the
    // served surfaces share. `None` leaves the engine's own resolution
    // (`RELATE_SNAPSHOT_FORMAT`, then the build default).
    options.snapshot_codec = cmd.snapshot_format;
    Ok(options)
}

/// The fallible prologue of [`run`], one `?` per step: every failure has
/// already printed its message and carries the process exit code. The
/// steps run in the order that keeps the side-effectful steps last: the
/// store and admission knobs resolve, the listeners bind and the Bolt
/// identity resolves (side-effect-free, before the load), the `--load`
/// source resolves (a fetch or clone is side-effectful, but every
/// refusal — a bad value, safe mode, an existing durable target —
/// precedes its I/O, and so does the bind), and the store opens before
/// auth — because the
/// generated-token path persists `<store>.auth-token` (creating its
/// parent directory), a malformed knob, a failed bind, a taken port, a
/// failed load, a failed open, or a
/// malformed identity must fail first and leave no token file (or
/// downloaded source, or created target store) behind.
/// The result is the serve kind ([`Ready`]): a stdio-only run stops after
/// the open — no listener surface exists, so no bind or auth story does —
/// and a Listening run carries the resolved listener prologue.
fn setup(cmd: &ServeCmd) -> Result<Ready, i32> {
    let options = store_options(cmd)?;
    // The admission limits (ADR-0333, ADR-0451) resolve beside the store
    // options, before the open: they do not depend on the store, so a
    // malformed cap or deadline variable fails before the store is opened
    // (or created). One fallible step whose `or_report` epilogue prints the
    // resolution's own `Error: ...` and exits 1, so the failure is loud for
    // every run kind — never silently ignored by `--mcp stdio` — and it
    // runs before auth, so the generated-token path cannot persist
    // anything first. The deadline is resolved whenever a surface with
    // HTTP-style headers is compiled, not only when this run binds one:
    // the environment spelling is a startup contract in those builds, and
    // a valid value is simply unused by a bolt-only run.
    let admission = Admission::resolve(cmd)?;
    // Bind every listener before printing anything — before auth resolves,
    // and before the `--load` side effects below: a taken port fails
    // loudly instead of surfacing after a "listening" banner, leaves no
    // token file behind, and fails before a fetched source or a created
    // target store exists (a leftover created target would poison the
    // retry with the exists refusal). A stdio-only run binds nothing —
    // its listener flags are a parse-time usage error — so these blocks
    // are all no-ops for it.
    #[cfg(feature = "http")]
    let http = cmd
        .http_port
        .map(|port| bind("--http", &cmd.host, port))
        .transpose()?;
    // The MCP role composes where cmd.mcp folds (round 7, F-7-1; round 1,
    // F-1-5): the stdio transport binds nothing and is the mixed run's
    // foreground half, so it is a role of its own — carried in
    // [`ReadyKind::Listening`] as the typed [`McpRole`], never an
    // `Option<McpSurface>` plus a separate bool.
    #[cfg(feature = "mcp")]
    let mcp = match cmd.mcp {
        Some(McpMode::Http(port)) => {
            let (port, listener) = bind("--mcp", &cmd.host, port)?;
            McpRole::Listener(McpSurface { port, listener })
        }
        Some(McpMode::Stdio) => McpRole::StdioForeground,
        None => McpRole::Absent,
    };
    #[cfg(feature = "bolt")]
    let bolt = cmd
        .bolt_port
        .map(|port| bind("--bolt", &cmd.host, port))
        .transpose()?;
    // ADR-0293: the HELLO identity resolves at startup when bolt serves —
    // flag, then env, then the honest default — and folds into the bound
    // bolt surface ([`BoltSurface`]): one value, so [`spawn_bolt`]
    // destructures the listener and the identity together (round 6,
    // F-6-8). The env
    // fallback is the shared loud read (relate_auth::env_string): a
    // non-UTF-8 RELATE_IDENTITY is malformed input (startup error),
    // never a silent fall-through to the default. This is a fallible step,
    // so it runs before auth: it reads only the flag and the process
    // environment and adds no banner line, and it must not be able to
    // abort startup after the generated-token path has persisted the
    // token file.
    #[cfg(feature = "bolt")]
    let bolt = match bolt {
        Some((port, listener)) => Some(BoltSurface {
            port,
            listener,
            identity: resolve_bolt_identity(
                cmd.bolt_identity.as_deref(),
                or_report(relate_auth::env_string("RELATE_IDENTITY"))?.as_deref(),
            ),
        }),
        None => None,
    };
    // LABS-903 (ADR-0500): `--load SOURCE` resolves into the store this run
    // opens — the same shared resolution the query session rides
    // (`crate::load::resolve`): the memory turn-into for a `:memory:`
    // target (a fetched copy is released after the load, so nothing is
    // kept), a created-and-cloned store for a path target, opened
    // normally. The load's announcements print inside the resolution; its
    // errors keep their own exits through setup's `Err(code)` contract
    // (usage 2, runtime 1) — the module's one print-and-return epilogue in
    // its code-from-error form. A durable-target load creates the store
    // with the codec knobs this run's own open carries (R-M2), so the
    // created store and the open agree. It runs after the binds, so a
    // taken port refuses before any fetch or target-store creation.
    let create = crate::load::CreateCodecs {
        to_snapshot_format: options.snapshot_codec,
        markdown_body_properties: options.markdown_body_properties.clone(),
    };
    let store_path = or_report_with(
        crate::load::resolve(
            &cmd.database,
            cmd.load.as_deref(),
            options.safe_mode,
            create,
        ),
        |e| e.code(),
    )?;
    let db = relate::Database::open_with(&store_path, options).map_err(|e| {
        crate::print_error(format_args!("Error: Failed to open Relate: {e}"));
        1
    })?;
    // The turn-into released the source at the open — a fetched scratch
    // copy is unreferenced the moment the open returns, so it is removed
    // then instead of at exit. The rule is `load`'s one statement, keyed
    // on the access this run opened with — the same key the query
    // session's post-open site uses.
    crate::load::release_source_after_open(cmd.access);
    // A stdio-only serve ([`ServeCmd::is_stdio_only`], the parse's
    // stdio-only fact) has no listener surface, so the rest of the listener
    // prologue — the auth story, and with it the token-file side effect —
    // does not exist for it (LABS-381, ADR-0368); the binds above were its
    // no-ops. The early return sits directly after the open (round 2,
    // F-2-6): the doc's "a stdio-only run stops at the open" is the code's
    // shape, not a fast path that arrives after no-op setup steps. Its
    // banner is the ADR-0306 §5 access-mode line alone; run's StdioOnly
    // arm takes it from here.
    #[cfg(feature = "mcp")]
    if cmd.is_stdio_only() {
        // The stdio-only banner is the ADR-0306 §5 access-mode line
        // alone, pushed directly (round 3, F-1-3): nothing binds, so no
        // host warning and no listener prologue exist for this kind.
        let mut banner = Vec::new();
        banner.extend(access_banner_line(cmd.access).map(str::to_string));
        return Ok(Ready {
            db: Arc::new(db),
            banner,
            kind: ReadyKind::StdioOnly,
        });
    }
    // The HTTP surface's spawn input is one composed value (round 6,
    // F-6-2): beside its bound pair it carries the discovery document's
    // `bolt_direct` endpoint, composed HERE in setup — where both
    // surfaces have bound — from the BOUND bolt port, never the requested
    // `--bolt` value, which is ephemeral-shaped: advertising
    // `bolt://host:0` would point tooling nowhere. `None` advertises no
    // bolt endpoint: this run bound no bolt surface, or this build
    // compiles none.
    #[cfg(feature = "http")]
    let http = http.map(|(port, listener)| HttpSurface {
        port,
        listener,
        #[cfg(feature = "bolt")]
        bolt_direct: bolt
            .as_ref()
            .map(|surface| format!("bolt://{}:{}", url_host(&cmd.host), surface.port)),
        #[cfg(not(feature = "bolt"))]
        bolt_direct: None,
    });
    // The HTTP managed-transaction limits (ADR-0435 §4) resolve beside the
    // connection cap, before auth: a malformed
    // RELATE_TX_IDLE_TIMEOUT_SECS or RELATE_MAX_OPEN_TRANSACTIONS
    // — or an idle timeout the reply cannot render as a deadline, refused
    // by the limits' validating constructor — must fail startup before the
    // generated-token path persists anything.
    //
    // They resolve whenever the `http` feature is compiled, not only when
    // `--http` is requested. The flags themselves are rejected at the parse
    // when no HTTP surface is served (the one unbound-surface flag policy,
    // `serve_args::unbound::reject`), but the environment
    // variables are still resolved for every run: a malformed
    // RELATE_TX_IDLE_TIMEOUT_SECS must fail startup in an http-built
    // binary regardless of which surface this run bound, and a valid value
    // simply yields limits the registry never reads. The unguarded-registry
    // warning below stays keyed on the HTTP surface actually running,
    // because a registry that is never built has nothing to warn about.
    #[cfg(feature = "http")]
    let tx_limits = or_report(relate_http::resolve_tx_limits(
        cmd.tx_idle_timeout_secs,
        cmd.max_open_transactions,
    ))?;
    // One auth resolution for every LISTENER surface (ADR-0232 §1), the
    // last fallible setup step: the generated-token path persists the
    // token file, so it must not run while the open, a bind, the identity
    // read, or the connection-cap resolution could still abort startup
    // (the same ordering as `relate-mcp`'s open). A malformed
    // flag/env value or token file is a loud startup error, never a
    // silent fallback.
    let env = or_report(relate_auth::env_credential())?;
    let (auth, auth_banner) = or_report(relate_auth::resolve(
        cmd.auth.as_deref(),
        env.as_deref(),
        &cmd.database,
    ))?;
    // The config half is read only by the listener spawners; without a
    // compiled surface it is discarded at the source (the banner half
    // still assembles) — compile-only either way, since a serve-only
    // parse rejects every invocation before setup runs (LABS-638).
    #[cfg(not(feature = "surface"))]
    let _ = auth;
    // The banner assembles in render order (round 6, F-6-4): the ADR-0248
    // plaintext-posture warning first — listener-prologue content (round
    // 3, F-1-3), pushed here where the listeners have bound, so a
    // stdio-only run, which binds nothing, never warns about exposing
    // surfaces — then the ADR-0306 §5 access-mode line, then the auth
    // lines, then the ADR-0333 unlimited-cap warning when it fires
    // (round 4, F-4-1/F-4-3), appended here where the cap is resolved, so
    // run's one banner print carries the whole prologue and never
    // re-derives a prologue input.
    // The bind posture, computed once: both loopback-keyed warnings read
    // these two locals instead of re-deriving the predicate or the
    // description beside their own message.
    let bind_loopback = is_loopback_host(&cmd.host);
    let bind_description = format!("--host {}", cmd.host);
    let mut banner = Vec::new();
    banner.extend(non_loopback_warning(&cmd.host));
    banner.extend(access_banner_line(cmd.access).map(str::to_string));
    banner.extend(auth_banner);
    banner.extend(relate_serve::unlimited_cap_warning(
        admission.connection_cap.max(),
        bind_loopback,
        &bind_description,
    ));
    // ADR-0435 §4: the managed-transaction warning fires only when BOTH
    // protections are off on a non-loopback bind.
    // Only an HTTP-enabled serve owns a transaction registry — a
    // bolt-only serve must not warn about protections for one it will
    // never build.
    // The guard reads the composed bound-surface fact (the `Option` handed
    // to the spawner), not `cmd.http_port` re-derived: this run builds a
    // registry exactly when an HTTP surface actually bound.
    #[cfg(feature = "http")]
    banner.extend(http.as_ref().and_then(|_| {
        relate_http::unguarded_tx_registry_warning(&tx_limits, bind_loopback, &bind_description)
    }));
    Ok(Ready {
        db: Arc::new(db),
        banner,
        kind: ReadyKind::Listening {
            #[cfg(feature = "surface")]
            auth,
            #[cfg(feature = "surface")]
            admission: Box::new(admission),
            #[cfg(feature = "http")]
            tx_limits,
            #[cfg(feature = "http")]
            http,
            #[cfg(feature = "mcp")]
            mcp,
            #[cfg(feature = "bolt")]
            bolt,
        },
    })
}

/// Run `serve`: open the store, bind every requested surface, resolve auth
/// once, spawn the surfaces, then block until SIGINT/SIGTERM and shut down
/// with the shared final-checkpoint policy. Returns the process exit code.
pub(crate) fn run(cmd: ServeCmd) -> i32 {
    let ready = match setup(&cmd) {
        Ok(ready) => ready,
        Err(code) => return code,
    };

    // The banner [`setup`] assembled — its content and render order are
    // the assembly's to state ([`Ready::banner`]'s field doc), not this
    // print's. One print for both serve kinds; all of it to stderr so
    // stdout stays free of banners.
    let Ready { db, banner, kind } = ready;
    for line in &banner {
        crate::print_error(line);
    }
    match kind {
        // The stdio-only kind — the old fast path, now the type's own: no
        // listener surface bound (no cap was resolved, no surface spawned),
        // so stdin EOF, not a signal, is the whole lifecycle (LABS-381,
        // ADR-0368).
        #[cfg(feature = "mcp")]
        ReadyKind::StdioOnly => run_mcp_stdio(&db, &cmd),
        ReadyKind::Listening {
            #[cfg(feature = "surface")]
            auth,
            #[cfg(feature = "surface")]
            admission,
            #[cfg(feature = "http")]
            tx_limits,
            #[cfg(feature = "http")]
            http,
            #[cfg(feature = "mcp")]
            mcp,
            #[cfg(feature = "bolt")]
            bolt,
        } => {
            // The spawners take their bound surface as the one value
            // [`setup`] composed (round 6, F-6-2, F-6-8): no port
            // extraction and no cfg-shadow in this dispatch.
            #[cfg(feature = "http")]
            if let Some(surface) = http {
                spawn::spawn_http(
                    &db,
                    surface,
                    &auth,
                    cmd.host.as_str(),
                    &cmd.origins,
                    &admission,
                    tx_limits,
                );
            }
            #[cfg(feature = "bolt")]
            if let Some(surface) = bolt {
                spawn::spawn_bolt(
                    &db,
                    surface,
                    &auth,
                    cmd.host.as_str(),
                    &cmd.ws_allow_origins,
                    &admission,
                );
            }
            // The MCP role decides the continuation in place (round 1,
            // F-1-5): a bound listener spawns here; the foreground stdio
            // transport IS the main-thread continuation, so it returns. The
            // typed role makes the two mutually exclusive, so no
            // `Some(listener)` + stdio state exists to run both. A mixed
            // run (LABS-381, ADR-0368) has spawned its listeners above and
            // now serves stdio until the host closes stdin — stdin EOF, not
            // a signal, is its shutdown. A stdio-only run never reached this
            // arm.
            #[cfg(feature = "mcp")]
            match mcp {
                McpRole::Absent => {}
                McpRole::Listener(surface) => {
                    spawn::spawn_mcp(&db, surface, &auth, cmd.host.as_str(), &admission);
                }
                McpRole::StdioForeground => return run_mcp_stdio(&db, &cmd),
            }

            // The workspace's one shutdown-signal wait (ADR-0233, ADR-0238):
            // signal-hook's blocking iterator on unix, a handler-notified
            // condvar elsewhere. The main thread owns the shutdown, so the
            // final checkpoint runs off the signal context.
            // The shared wait-or-exit policy (ADR-0238): a
            // handler-installation failure logs and exits 1 rather than
            // serving unwatched.
            relate_serve::signal::wait_or_exit("Error: serve");
            crate::print_error("serve: shutting down");
            let storage = crate::telemetry::StorageKind::for_uri(&cmd.database);
            crate::session::final_checkpoint(&db, storage, 0)
        }
    }
}

/// Run the MCP stdio transport in the foreground (LABS-381, ADR-0368): the
/// [`mcp_config`] helper's default transport is `Transport::Stdio`, which
/// is exactly the arm [`relate_mcp::serve_shared`] dispatches on — the
/// stdio framing over the process's real stdin/stdout (the HTTP listener
/// path, `spawn_mcp`, owns its transport separately through
/// `serve_http_on`). Auth is skipped (ADR-0232 §1: the host spawned us, so
/// there is no trust boundary to cross — `auth` is
/// [`relate_auth::AuthConfig::None`]). Blocks until the host closes
/// stdin, then the store takes its final checkpoint and the process exits
/// with the transport's code.
#[cfg(feature = "mcp")]
fn run_mcp_stdio(db: &Arc<relate::Database>, cmd: &ServeCmd) -> i32 {
    crate::print_error("serve: MCP (stdio) serving on stdin/stdout (auth: none)");
    // The stdio transport has no sockets, so the admission deadlines are
    // inert; the listener defaults still ride along from `Default`.
    let config = relate_mcp::McpServerConfig {
        auth: relate_auth::AuthConfig::None,
        read_only: db.is_read_only(),
        ..relate_mcp::McpServerConfig::default()
    };
    let storage = crate::telemetry::StorageKind::for_uri(&cmd.database);
    match relate_mcp::serve_shared(Arc::clone(db), config) {
        Ok(()) => {
            crate::print_error("serve: stdin closed, shutting down");
            crate::session::final_checkpoint(db, storage, 0)
        }
        Err(e) => {
            crate::print_error(format_args!(
                "Error: serve: MCP stdio transport failed: {e}"
            ));
            crate::session::final_checkpoint(db, storage, 1)
        }
    }
}

// serve_cmd's unit tests live in `tests.rs` (the skill_cmd dir-module
// pattern) so this file stays under the repo's 1k-line budget.
#[cfg(test)]
mod tests;
