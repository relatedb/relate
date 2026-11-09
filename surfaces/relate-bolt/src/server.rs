//! Standalone listener: open the database, accept connections, and serve
//! each one through the transport peek (ADR-0109, ADR-0266).
//!
//! ADR-0233: thread-per-connection on std primitives — no async runtime.
//! The accept loop itself (accept-then-close cap, accept-error
//! log-and-retry, explicitly stacked connection threads) is the shared
//! `relate_serve::serve_capped` (ADR-0238); this crate passes its log
//! label, its 64 MiB connection stack, and the per-connection handler.

use std::error::Error;
use std::sync::Arc;

use relate_serve::ConnectionCap;

use relate::{Database, OpenOptions};
use std::net::TcpListener;

use crate::auth;
use crate::cli_args::Args;
use crate::protocol::{ConnConfig, ConnLimits};
use crate::tls::{bind_is_loopback, load_tls_config, startup_plaintext_policy, StartupAction};
use crate::transport::{serve_conn, Transport};

/// The connection thread's stack (ADR-0233): the engine query worker's
/// own budget, derived from the one canonical constant
/// (`relate::QUERY_WORKER_STACK_BYTES`, ADR-0259) rather than
/// duplicated — the fast-path fact `serve_listener_with_cap` sets below
/// is true exactly because this is that number (ADR-0504). `pub(crate)`:
/// the test fixtures' one-connection server threads mirror the production
/// thread exactly.
pub(crate) const CONNECTION_STACK_BYTES: usize = relate::QUERY_WORKER_STACK_BYTES;

impl Args {
    fn conn_limits(&self) -> ConnLimits {
        let handshake_timeout = crate::protocol::secs_to_opt(self.handshake_timeout_secs);
        let idle_timeout = crate::protocol::secs_to_opt(self.idle_timeout_secs);
        let query_timeout = crate::protocol::secs_to_opt(self.query_timeout_secs);
        ConnLimits {
            handshake_timeout,
            idle_timeout,
            query_timeout,
            max_result_bytes: self.max_result_bytes,
            max_connections: self.max_connections,
            ..ConnLimits::default()
        }
    }

    fn open_options(&self) -> OpenOptions {
        OpenOptions {
            wide_columns: false,
            wal_dir: self.wal_dir.clone(),
            checkpoint_interval: self
                .checkpoint_interval_secs
                .map(std::time::Duration::from_secs),
            durability_mode: self.durability_mode,
            logical_graph_size_limit: self.logical_graph_size_limit,
            snapshot_codec: None,
            // The markdown body-property knob resolves through
            // RELATE_MARKDOWN_BODY_PROPERTIES (the open preflight);
            // the server Args carries no flag of its own.
            markdown_body_properties: None,
            load_csv_dir: self.load_csv_dir.clone(),
            load_csv_max_bytes: None,
            mode: if self.read_only {
                relate::OpenMode::ReadOnly
            } else {
                relate::OpenMode::Writable
            },
            concurrent_logical_workspace_limit: self.concurrent_logical_workspace_limit,
            max_traversal_depth: self.max_traversal_depth,
            expression_memory_limit: self.expression_memory_limit,
            // The standalone Bolt crate's Args has no safe-mode flag; the
            // product path (`relate serve --bolt --safe`) resolves it
            // in serve_cmd's own open. A host embedding this server opts in
            // through `OpenOptions::safe_mode` directly.
            safe_mode: false,
            follow_gap: Default::default(),
        }
    }

    /// The WebSocket serving policy (ADR-0266): `None` after
    /// `--no-websocket`, else the `WsConfig` whose Origin policy keys off
    /// the resolved auth mode and whose opt-in session cap comes from
    /// `--ws-max-connections` (ADR-0459; `0` means no separate cap). Shared
    /// behind an `Arc` so no connection deep-clones the Origin list, and so
    /// every session shares one cap instance.
    fn ws_policy(&self, auth_enabled: bool) -> Option<Arc<crate::websocket::WsConfig>> {
        if self.no_websocket {
            return None;
        }
        Some(Arc::new(crate::websocket::WsConfig::resolve(
            &self.ws_allow_origin,
            auth_enabled,
            self.ws_max_connections,
        )))
    }
}

pub fn serve(args: &Args) -> Result<(), Box<dyn Error + Send + Sync>> {
    serve_on(args, None)
}

/// [`serve`] on a listener the caller already bound. `None` binds `args.bind`
/// at the same point `serve` always has. The e2e harness uses `Some` so it
/// holds its port from boot to serve: a probed-then-dropped port can be taken
/// by another process before the server binds, and the readiness probe would
/// then talk to the wrong listener.
pub fn serve_on(
    args: &Args,
    listener: Option<TcpListener>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let tls = load_tls_config(
        args.tls_cert.as_deref(),
        args.tls_key.as_deref(),
        args.tls_client_ca.as_deref(),
    )?;
    let bind_loopback = bind_is_loopback(&args.bind)?;
    let policy = startup_plaintext_policy(bind_loopback, tls.is_some(), args.allow_plaintext);

    match policy.action {
        StartupAction::Refuse => {
            return Err(format!(
                "refusing to serve plaintext Bolt on non-loopback bind {}: configure --tls-cert and --tls-key, or pass --allow-plaintext to accept the risk",
                args.bind
            )
            .into());
        }
        StartupAction::WarnAndServe => {
            eprintln!(
                "WARNING: serving plaintext Bolt on non-loopback bind {}; traffic is unencrypted (--allow-plaintext)",
                args.bind
            );
        }
        StartupAction::Serve => {}
    }
    // The ADR-0333 unlimited-cap warning — the shared policy beside
    // `relate_serve::resolve_connection_cap` (round 2, F-2-1), its
    // one canonical line named with this server's bind description
    // (round 3, F-3-1).
    if let Some(warning) = relate_serve::unlimited_cap_warning(
        args.max_connections,
        bind_loopback,
        &format!("bind {}", args.bind),
    ) {
        eprintln!("{warning}");
    }

    let env_auth = auth::env_credential()?;
    let (auth_config, banner) =
        auth::resolve(args.auth.as_deref(), env_auth.as_deref(), &args.uri)?;
    let auth_config = Arc::new(auth_config);
    for line in banner {
        eprintln!("{line}");
    }
    let db = Arc::new(Database::open_with(&args.uri, args.open_options())?);
    let listener = match listener {
        Some(listener) => listener,
        None => TcpListener::bind(&args.bind)?,
    };
    let transport = Transport {
        plaintext_allowed: policy.plaintext_allowed,
        websocket: args.ws_policy(auth_config.is_enabled()),
        tls,
    };
    eprintln!(
        "Relate Bolt listening on {} (uri={}, tls={}, auth={}, ws={})",
        args.bind,
        args.uri,
        if transport.tls.is_some() { "on" } else { "off" },
        auth_config.banner_label(),
        if transport.websocket.is_some() {
            "on"
        } else {
            "off"
        },
    );
    serve_listener(
        listener,
        transport,
        db,
        ConnConfig {
            limits: args.conn_limits(),
            auth: auth_config,
            local_address: String::new(),
            // ADR-0293: the operator's --identity/RELATE_IDENTITY,
            // else the honest default.
            server_identity: args
                .identity
                .clone()
                .unwrap_or_else(crate::protocol::default_server_identity),
            ..ConnConfig::default()
        },
    )
}

/// Accept loop: the shared [`relate_serve::serve_capped`] (ADR-0238) —
/// thread-per-connection under the `max_connections` cap (accept-then-close
/// over cap), accept errors logged and retried, each connection on its own
/// [`CONNECTION_STACK_BYTES`] thread with spawn failures logged, never
/// silently dropped. `serve_conn`, the transport peek, and the connection
/// config stay here; only the scaffolding is shared.
pub fn serve_listener(
    listener: TcpListener,
    transport: Transport,
    db: Arc<Database>,
    config: ConnConfig,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let cap = Arc::new(ConnectionCap::new(config.limits.max_connections));
    serve_listener_with_cap(listener, transport, db, config, cap)
}

/// [`serve_listener`] on a cap the caller already owns. `relate serve`
/// passes one process-wide cap into Bolt, HTTP, and MCP (ADR-0333).
///
/// ADR-0504: the shared accept loop spawns every connection thread with
/// [`CONNECTION_STACK_BYTES`] — the engine query worker's own budget — so
/// this listener sets [`ConnConfig::caller_thread_carries_worker_stack`]
/// on its config, derived from that same number rather than asserted as a
/// literal: the fast-path fact is true exactly while the budget handed to
/// the accept loop *is* the engine's, and a future divergence flips the
/// assertion off instead of lying about it. Each session then asserts the
/// worker stack on its engine connection, and bounded-work statements run
/// on the connection thread (the fast path) while everything else
/// escalates to the worker.
pub fn serve_listener_with_cap(
    listener: TcpListener,
    transport: Transport,
    db: Arc<Database>,
    mut config: ConnConfig,
    cap: Arc<ConnectionCap>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    // The fast-path fact, anchored to the budget this body actually hands
    // the accept loop: `stack == QUERY_WORKER_STACK_BYTES` is
    // constant-true today (the alias), and the anchor is what makes it
    // self-disarm — a future edit that spawns with any other budget
    // flips the assertion off instead of running inline on an
    // under-stacked thread.
    let stack = CONNECTION_STACK_BYTES;
    config.caller_thread_carries_worker_stack = stack == relate::QUERY_WORKER_STACK_BYTES;
    relate_serve::serve_capped(listener, "bolt", stack, cap, move |stream| {
        serve_conn(stream, &transport, Arc::clone(&db), config.clone())
    })
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;
