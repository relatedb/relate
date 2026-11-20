//! The spawner cluster: the bound-surface value types `setup` composes and
//! the one-per-surface spawners `run` hands them to. Kept beside `serve_cmd`
//! (the `serve_args/unbound.rs` and `serve_cmd/tests.rs` precedent) so the
//! orchestration file stays focused on setup/run.

use std::net::TcpListener;
use std::sync::Arc;

use super::{url_host, Admission};

/// Spawn one surface's serve loop on its own thread and log a failure: the
/// one statement of the spawner logging policy (`serve: <label> failed: …`).
fn spawn_logged<E: std::fmt::Display>(
    label: &'static str,
    serve: impl FnOnce() -> Result<(), E> + Send + 'static,
) {
    std::thread::spawn(move || {
        if let Err(e) = serve() {
            crate::print_error(format_args!("serve: {label} failed: {e}"));
        }
    });
}

/// The HTTP surface's startup data (round 6, F-6-2): the **bound**
/// `(port, listener)` pair (for the port flag 0 that is the ephemeral
/// port the OS assigned, so the banner never announces `:0`) plus the
/// discovery document's `bolt_direct` endpoint, composed in setup
/// where both surfaces bind from the **bound** bolt port — `None`
/// advertises no bolt endpoint (this run bound no bolt surface, or this
/// build compiles none). One value, so run's http arm hands over one
/// composed fact and no port is extracted in the dispatch.
#[cfg(feature = "http")]
pub(super) struct HttpSurface {
    pub(super) port: u16,
    pub(super) listener: TcpListener,
    pub(super) bolt_direct: Option<String>,
}

/// The MCP surface's (streamable-HTTP transport) startup data: the
/// **bound** `(port, listener)` pair (for the port flag 0 that is the
/// ephemeral port the OS assigned, so the banner never announces `:0`).
/// One bound-surface value, the same shape as the sibling spawners'
/// (round 6, F-6-8).
#[cfg(feature = "mcp")]
pub(super) struct McpSurface {
    pub(super) port: u16,
    pub(super) listener: TcpListener,
}

/// The MCP role of one serve run (round 1, F-1-5): the three states the
/// `--mcp` fold can produce, so run dispatches on the typed role
/// instead of re-combining an `Option<McpSurface>` with a separate stdio
/// flag — a listener and a foreground stdio transport are mutually
/// exclusive by construction.
#[cfg(feature = "mcp")]
pub(super) enum McpRole {
    /// No `--mcp` flag: this run serves no MCP surface.
    Absent,
    /// `--mcp PORT`: the streamable-HTTP listener, bound in setup.
    Listener(McpSurface),
    /// `--mcp stdio`: the foreground stdio transport, run on the main
    /// thread after every listener (LABS-381, ADR-0368).
    StdioForeground,
}

/// The Bolt surface's startup data: the **bound** `(port, listener)` pair
/// (for `port` 0 that is the ephemeral port the OS assigned, so the banner
/// never announces `:0`) plus the resolved HELLO identity (ADR-0293). One
/// value, so the identity cannot drift from the listener it belongs to:
/// run's bolt arm hands it to [`spawn_bolt`] whole (round 6, F-6-8), and
/// no fabricated default exists.
#[cfg(feature = "bolt")]
pub(super) struct BoltSurface {
    pub(super) port: u16,
    pub(super) listener: TcpListener,
    pub(super) identity: String,
}

/// Spawn the HTTP Query API v2 surface on its own thread: the startup
/// line, then the config and the diverging serve loop (accept errors are
/// retried, session errors logged per connection; the thread runs until
/// the process exits). The startup line is Neo4j's `HTTP enabled on
/// host:port.` shape (REL8-1762): bare — no `serve:` prefix, no auth
/// label, trailing period — so the Docker image's logs match what
/// tooling built for the Neo4j image (testcontainers and friends waiting
/// on `HTTP enabled on`) reads; the auth story stays on the banner's own
/// lines. `surface` is the bound surface setup composed
/// (round 6, F-6-2): the **bound** `(port, listener)` pair (for a port
/// flag of 0 the OS-assigned ephemeral port, the same one the banner
/// announces) plus the discovery document's `bolt_direct` endpoint —
/// `None` advertising no bolt endpoint. `host` is the `--host` value and
/// `origins` the parsed `--origin` values — the two facts of the command
/// this surface reads, passed one by one like the sibling spawners
/// (round 4, F-4-6). `admission` is the resolved ADR-0333 connection cap
/// and both ADR-0451 admission deadlines; `tx_limits` the resolved ADR-0435
/// managed-transaction configuration, from which the registry is
/// constructed here, so `HttpConfig` carries no second copy.
#[cfg(feature = "http")]
pub(super) fn spawn_http(
    db: &Arc<relate::Database>,
    surface: HttpSurface,
    auth: &relate_auth::AuthConfig,
    host: &str,
    origins: &[relate_http::http::OriginFlagValue],
    admission: &Admission,
    tx_limits: relate_http::TxLimits,
) {
    let HttpSurface {
        port,
        listener,
        bolt_direct,
    } = surface;
    crate::print_notice(format_args!("HTTP enabled on {}:{}.", url_host(host), port));
    let config = relate_http::HttpConfig {
        allowed_origins: relate_http::http::OriginAllowlist::from_flag_values(origins),
        bolt_direct,
        connection_cap: Some(Arc::clone(&admission.connection_cap)),
        header_deadline: admission.header_deadline,
        body_deadline: admission.body_deadline,
        transactions: relate_http::TxRegistry::with_limits(tx_limits),
        ..relate_http::HttpConfig::new(auth.clone())
    };
    let db = Arc::clone(db);
    spawn_logged("HTTP surface", move || {
        relate_http::serve(db, listener, config)
    });
}

/// The MCP surface's config from the opened handle (ADR-0306 §3): the
/// surface advertises the handle's mode — `read_only` follows
/// `Database::is_read_only`, so `write_cypher` is listed exactly when the
/// store accepts writes (an always-failing tool would be a dead end for
/// hosts that auto-approve `readOnlyHint` tools). `admission` supplies the
/// two ADR-0451 HTTP admission deadlines.
#[cfg(feature = "mcp")]
pub(super) fn mcp_config(
    db: &Arc<relate::Database>,
    auth: &relate_auth::AuthConfig,
    admission: &Admission,
) -> relate_mcp::McpServerConfig {
    relate_mcp::McpServerConfig {
        auth: auth.clone(),
        read_only: db.is_read_only(),
        connection_cap: Some(Arc::clone(&admission.connection_cap)),
        header_deadline: admission.header_deadline,
        body_deadline: admission.body_deadline,
        ..Default::default()
    }
}

/// Spawn the MCP surface (streamable HTTP transport) on its own thread.
/// `surface` is the bound surface setup composed — the **bound**
/// `(port, listener)` pair, one bound-surface value like the sibling
/// spawners (round 6, F-6-8). `admission` supplies the shared connection
/// cap (ADR-0333) and both ADR-0451 deadlines.
#[cfg(feature = "mcp")]
pub(super) fn spawn_mcp(
    db: &Arc<relate::Database>,
    surface: McpSurface,
    auth: &relate_auth::AuthConfig,
    host: &str,
    admission: &Admission,
) {
    let McpSurface { port, listener } = surface;
    crate::print_notice(format_args!(
        "serve: MCP (streamable HTTP) listening on http://{}:{} (auth: {})",
        url_host(host),
        port,
        auth.banner_label()
    ));
    let server = relate_mcp::McpServer::new_shared(Arc::clone(db), mcp_config(db, auth, admission));
    spawn_logged("MCP surface", move || {
        relate_mcp::serve_http_on(server, listener)
    });
}

/// Spawn the Bolt surface on its own thread. Embedded shape: the CLI bound
/// the socket (a taken port is a loud startup error) and shares the
/// already-open store — bolt's standalone `serve` would open it a second
/// time. `surface` is the bound surface setup composed — the **bound**
/// `(port, listener)` pair plus the resolved HELLO identity (ADR-0293),
/// one bound-surface value like the sibling spawners (round 6, F-6-8).
/// The startup line is Neo4j's `Bolt enabled on host:port.` shape
/// (REL8-1762) — bare, no `serve:` prefix and no auth label, for the same
/// docker drop-in reason as the HTTP spawner's line; the auth story stays
/// on the banner's own lines. The transport policy is bolt's own
/// embedded-loopback default with the
/// WebSocket policy resolved here through the one funnel,
/// [`relate_bolt::websocket::WsConfig::resolve`] — the origin list,
/// auth mode, and admission cap — mirroring how the HTTP spawner builds its
/// policy through `OriginAllowlist::from_flag_values`; `ws_allow_origins`
/// is the command policy list, passed the way the HTTP spawner receives
/// `--origin`; default limits (ADR-0110).
#[cfg(feature = "bolt")]
pub(super) fn spawn_bolt(
    db: &Arc<relate::Database>,
    surface: BoltSurface,
    auth: &relate_auth::AuthConfig,
    host: &str,
    ws_allow_origins: &[relate_serve::origin::OriginFlagValue],
    admission: &Admission,
) {
    let BoltSurface {
        port,
        listener,
        identity,
    } = surface;
    crate::print_notice(format_args!("Bolt enabled on {}:{}.", url_host(host), port));
    let transport = relate_bolt::transport::Transport::loopback_embedded(
        relate_bolt::websocket::WsConfig::resolve(
            ws_allow_origins,
            auth.is_enabled(),
            admission.ws_max_connections,
        ),
    );
    let bolt_config = relate_bolt::protocol::ConnConfig {
        auth: std::sync::Arc::new(auth.clone()),
        server_identity: identity,
        ..Default::default()
    };
    let db = Arc::clone(db);
    let connection_cap = Arc::clone(&admission.connection_cap);
    // The serve loop diverges; a startup/loop error is logged like the other
    // surfaces' threads.
    spawn_logged("Bolt serve loop", move || {
        relate_bolt::serve_listener_with_cap(listener, transport, db, bolt_config, connection_cap)
    });
}
