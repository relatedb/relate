use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use relate::Database;

use crate::auth::AuthConfig;
use crate::framing::Framer;
use crate::packstream::encode_error;
use crate::session::{Flow, Session};
use crate::version::BoltVersion;

/// The chunk framing (cap, NOOPs, the exact client-side reader) lives in
/// `crate::framing` (ADR-0316, ADR-0317); `read_message` keeps its public
/// path here. The ROUTE response builder lives in `crate::summary` with
/// every other SUCCESS metadata builder (ADR-0309); re-exported here so the
/// ADR-0281 library surface keeps its `protocol::routing_table` path.
pub use crate::framing::read_message;
pub use crate::summary::routing_table;

// The CLI-facing defaults live with the clap definition in the lib's
// cli_args (single source — no re-export shim).
use crate::cli_args::{
    DEFAULT_HANDSHAKE_TIMEOUT_SECS, DEFAULT_IDLE_TIMEOUT_SECS, DEFAULT_MAX_CONNECTIONS,
    DEFAULT_MAX_RESULT_BYTES, DEFAULT_QUERY_TIMEOUT_SECS,
};

/// Convert a seconds value into an optional duration: `0` disables the
/// limit, anything else is `Some(Duration::from_secs(secs))`. Single source
/// for the "0 disables" convention shared by the CLI args and the served
/// defaults (ADR-0110).
pub fn secs_to_opt(secs: u64) -> Option<Duration> {
    (secs != 0).then(|| Duration::from_secs(secs))
}

/// Per-connection governance limits (ADR-0110).
#[derive(Debug, Clone, Copy)]
pub struct ConnLimits {
    /// Bound on the magic/version handshake. `None` disables it.
    pub handshake_timeout: Option<Duration>,
    /// Bound on each blocking read. `None` disables it.
    pub idle_timeout: Option<Duration>,
    /// Bound on one `RUN` query, cancelled through the engine's `CancelToken`
    /// (ADR-0111). `None` leaves queries unbounded (the default).
    pub query_timeout: Option<Duration>,
    /// Maximum logical bytes in one result. `0` disables the cap.
    pub max_result_bytes: u64,
    /// Maximum concurrent Bolt connections. `0` is unlimited (ADR-0333).
    /// An over-cap connection is accepted then immediately closed.
    pub max_connections: usize,
    /// The NOOP keep-alive interval (ADR-0312): while a request's engine
    /// work runs this long without its reply, the server writes a NOOP
    /// chunk, and repeats each interval. The HELLO
    /// `connection.recv_timeout_seconds` hint is twice this. Default 60 s;
    /// a library knob (tests inject a short one), not a CLI flag. Must be
    /// non-zero; a zero value is rejected when the heartbeat is built
    /// (ADR-0334).
    pub keep_alive_interval: Duration,
}

impl Default for ConnLimits {
    /// Served defaults (ADR-0110), single-sourced in the `DEFAULT_*` consts
    /// above: the clap `Args` on [`crate::cli_args::Args`] reference the same consts,
    /// except `--max-result-bytes`, whose clap default is the string
    /// [`crate::cli_args::DEFAULT_MAX_RESULT_BYTES_STR`] pinned to
    /// [`DEFAULT_MAX_RESULT_BYTES`] by test.
    fn default() -> Self {
        Self {
            handshake_timeout: secs_to_opt(DEFAULT_HANDSHAKE_TIMEOUT_SECS),
            idle_timeout: secs_to_opt(DEFAULT_IDLE_TIMEOUT_SECS),
            query_timeout: secs_to_opt(DEFAULT_QUERY_TIMEOUT_SECS),
            max_result_bytes: DEFAULT_MAX_RESULT_BYTES,
            max_connections: DEFAULT_MAX_CONNECTIONS,
            keep_alive_interval: crate::keepalive::DEFAULT_KEEP_ALIVE_INTERVAL,
        }
    }
}

// The socket-deadline cluster (SocketTimeouts + impls, is_timeout,
// negotiation_deadline, arm_phase) lives in `transport.rs`, which owns the
// ADR-0141/0226 deadline story; re-exported so this module's bound and the
// test modules' paths keep resolving through `crate::protocol`.
pub(crate) use crate::transport::{arm_or_timeout, arm_phase, is_timeout, SocketTimeouts};

/// A query the session finished, for a host that counts capacity.
///
/// `Success` is the ending `SUCCESS` of a result, not a `RUN` header and
/// not `HELLO`. A refusal is only `concurrent-logical-workspace-limit`.
/// A syntax error, a graph-cap failure, and `writer-draining` are none of
/// these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryOutcome {
    Success,
    RetryableRefusal,
    NonRetryableRefusal,
}

/// One connection's configuration: the ADR-0110 governance limits plus the
/// ADR-0126 auth — one bundle so the serve chain stops widening with every
/// cross-cutting concern.
#[derive(Clone)]
pub struct ConnConfig {
    pub limits: ConnLimits,
    pub auth: Arc<AuthConfig>,
    /// The connection's local socket address: the ROUTE fallback when the
    /// routing context carries no usable `address` (ADR-0140). Set by
    /// `serve_conn` from `stream.local_addr()` while the socket is still a
    /// `TcpStream` — a failed lookup closes the connection there, so a
    /// served connection always carries a real address. Tests driving
    /// `handle_conn` directly set a test address.
    pub local_address: String,
    /// The HELLO SUCCESS `server` metadata — the server agent string
    /// (ADR-0293). Default is the honest [`default_server_identity`]; an
    /// operator may configure a Neo4j-shaped value for strict tooling.
    pub server_identity: String,
    /// The per-server connection counter behind the HELLO SUCCESS
    /// `connection_id` metadata (ADR-0293): `serve_listener` clones this
    /// config per accepted connection, so the shared `Arc` keeps the ids
    /// unique within one listener. Public so embedders (the CLI's
    /// `serve --bolt`) can build a config with struct-update syntax.
    pub connection_ids: Arc<AtomicU64>,
    /// Whether the thread serving this connection carries the engine
    /// query worker's own stack budget (64 MiB, ADR-0259) — the fact
    /// behind the connection-thread fast path (ADR-0504): when true, the
    /// session asserts it on its engine `Connection`
    /// (`assume_worker_stack_on_caller_thread`), bounded-work statements
    /// run directly on the connection thread, and everything else
    /// escalates to the worker as before. Set automatically by
    /// `serve_listener`/`serve_listener_with_cap`, whose accept loop
    /// spawns every connection thread with exactly that budget — an
    /// embedder driving `handle_conn` on its own thread sets it **only**
    /// if that thread carries the budget too; the default (`false`)
    /// keeps every statement on the worker.
    pub caller_thread_carries_worker_stack: bool,
    /// Called after a query result ends in `SUCCESS`, and after a
    /// workspace-limit `FAILURE` is written. `None` counts nothing.
    pub on_query: Option<Arc<dyn Fn(QueryOutcome) + Send + Sync>>,
}

impl ConnConfig {
    /// Allocate the next connection's id (`conn-N`, unique within this
    /// config's listener). The Java driver requires a `connection_id`
    /// key in the HELLO SUCCESS metadata; any string value satisfies it.
    pub fn next_connection_id(&self) -> String {
        let n = self.connection_ids.fetch_add(1, Ordering::Relaxed);
        format!("conn-{}", n + 1)
    }
}

/// The default HELLO `server` agent string (ADR-0293):
/// `Relate/<crate version>`, single-sourced from CARGO_PKG_VERSION.
pub fn default_server_identity() -> String {
    concat!("Relate/", env!("CARGO_PKG_VERSION")).to_string()
}

impl Default for ConnConfig {
    fn default() -> Self {
        Self {
            limits: ConnLimits::default(),
            auth: Arc::new(AuthConfig::None),
            local_address: String::new(),
            server_identity: default_server_identity(),
            connection_ids: Arc::new(AtomicU64::new(0)),
            caller_thread_carries_worker_stack: false,
            on_query: None,
        }
    }
}

/// Serve one Bolt connection under `config` (ADR-0110 limits, ADR-0126 auth).
///
/// ADR-0233: this runs on the connection's own thread; the engine
/// `Connection` is a plain local, owned here for the session's lifetime, and
/// engine calls are plain calls. `deadline` is the ADR-0141 negotiation
/// deadline as plain data: `Some` is the shared accept-time deadline
/// computed by `serve_conn`, `None` disables the bound (direct-call tests).
/// Nothing here recomputes a budget.
pub fn handle_conn<S: Read + Write + SocketTimeouts>(
    mut stream: S,
    db: Arc<Database>,
    config: ConnConfig,
    deadline: Option<Instant>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // A negotiation failure (timeout, or no budget left) aborts the
    // connection — the session loop must never start without a completed
    // handshake, or the ADR-0141 handshake timeout stops killing stalled
    // pre-auth connections. A client that proposed no spoken version has
    // been answered `00 00 00 00` and is closed the same way (ADR-0303).
    let version = match negotiate_magic(&mut stream, deadline)? {
        Negotiation::Serve(version) => version,
        Negotiation::Unsupported | Negotiation::Aborted => return Ok(()),
    };
    // Negotiation is done: the per-read idle timeout governs the session
    // (ADR-0110); a read that stalls past it fails with a timeout error.
    // The installs propagate (the same policy the HTTP surface's
    // serve_handler documents): a failed guard must not leave the session
    // unbounded.
    stream.set_read_timeout(config.limits.idle_timeout)?;
    stream.set_write_timeout(None)?;

    let mut conn = db.connect()?;
    // ADR-0504: when the serving thread carries the query worker's own
    // stack budget (the production accept loop's explicitly stacked
    // connection threads, ADR-0233), the session asserts it and
    // bounded-work statements run on this very thread — no worker
    // handoff, no context switches. Statements with data-proportional or
    // unbounded-row potential still escalate to the worker, keeping
    // ADR-0312's NOOP keep-alive for the statements that can outlive its
    // interval. A directly-driven `handle_conn` on an unstacked thread
    // keeps the flag unset and every statement on the worker.
    if config.caller_thread_carries_worker_stack {
        conn.assume_worker_stack_on_caller_thread();
    }
    let mut session = Session::new(conn, &config, version);
    let mut framer = Framer::new();
    loop {
        // A yielded follower is waiting for the host to drop this engine
        // (ADR-0441). Close on the next message, or when the idle read
        // returns, so this connection releases its `Arc`.
        if db.needs_reopen() {
            eprintln!("bolt: follower reopening; closing connection");
            break;
        }
        // Idle governance (ADR-0110): the per-read idle timeout is armed
        // on the socket, and the wait for a message shares the same budget
        // as a wall-clock deadline (re-armed per read in the framer) so a
        // trickling client cannot hold one message open forever. Every
        // complete message already received is framed into one batch so a
        // RESET in it can interrupt the work queued ahead of it (ADR-0317).
        let batch = match framer.next_batch(&mut stream, config.limits.idle_timeout) {
            Ok(Some(batch)) => batch,
            Ok(None) => break,
            Err(e) if is_timeout(&e) => {
                eprintln!("bolt: idle timeout; closing connection");
                break;
            }
            Err(_) => break,
        };
        if let Flow::Close = session.serve_batch(&mut stream, batch, &config)? {
            break;
        }
    }
    Ok(())
}

/// How the magic + version negotiation ended.
enum Negotiation {
    /// A proposal covered a spoken version and its `00 00 mm MM` reply
    /// was sent: the session may start, speaking the negotiated version
    /// (ADR-0484).
    Serve(BoltVersion),
    /// No proposal covered a spoken version: `00 00 00 00` was sent
    /// (ADR-0303) and the caller closes the connection without reading
    /// further.
    Unsupported,
    /// Timeout / no budget left (ADR-0141): already logged, and to be
    /// closed by the caller.
    Aborted,
}

/// The handshake reply when no proposal covers a version the server speaks
/// (ADR-0303); the connection closes after it.
const NO_VERSION: [u8; 4] = [0x00; 4];

/// Whether one `00 RR mm MM` version proposal covers the spoken version
/// (ADR-0303, generalized by ADR-0484 decision 2): the proposal's major
/// equals the version's major and `mm - RR <= m <= mm`, the subtraction
/// saturating at minor 0. The reserved first byte is not inspected, and a
/// manifest proposal (`00 00 01 FF`) never matches: its major byte is
/// `0xFF`.
fn covers(proposal: &[u8; 4], spoken: BoltVersion) -> bool {
    let [_, range, minor, major] = *proposal;
    let [_, _, want_minor, want_major] = spoken.wire();
    major == want_major && minor.saturating_sub(range) <= want_minor && want_minor <= minor
}

/// The version reply for the client's four proposals, read in client order
/// (ADR-0303, ADR-0484 decision 2): the first proposal covering a spoken
/// version selects that version; `None` when no proposal covers one
/// (zero-filled slots never do). A range never spans majors and each
/// spoken major has one minor, so a covering proposal names exactly one
/// spoken version.
fn select_version(proposals: &[[u8; 4]; 4]) -> Option<BoltVersion> {
    proposals.iter().find_map(|proposal| {
        BoltVersion::SPOKEN
            .into_iter()
            .find(|v| covers(proposal, *v))
    })
}

/// The magic + version exchange itself, under the ADR-0141 deadline: the
/// magic, the four proposals, the version reply.
fn handshake<S: Read + Write + SocketTimeouts>(
    stream: &mut S,
    deadline: Option<Instant>,
) -> std::io::Result<Negotiation> {
    let mut magic = [0u8; 4];
    // Re-arm per read: the exchange is bounded by the wall-clock
    // deadline, not by syscall count (a byte-per-window trickle cannot
    // outlive it).
    arm_or_timeout(stream, deadline)?;
    stream.read_exact(&mut magic)?;
    if magic != [0x60, 0x60, 0xB0, 0x17] {
        return Err(encode_error("bad bolt magic".into()));
    }
    let mut proposals = [[0u8; 4]; 4];
    arm_or_timeout(stream, deadline)?;
    stream.read_exact(proposals.as_flattened_mut())?;
    let (reply, outcome) = match select_version(&proposals) {
        Some(version) => (version.wire(), Negotiation::Serve(version)),
        None => {
            let offered = proposals.map(|p| format!("{:08X}", u32::from_be_bytes(p)));
            // The spoken-version clause comes from the SPOKEN table via
            // `name`, so a new spoken version updates the diagnostic with
            // the table, never by hand here.
            let spoken = BoltVersion::SPOKEN
                .into_iter()
                .map(|v| v.name())
                .collect::<Vec<_>>()
                .join(" or ");
            eprintln!(
                "bolt: no version proposal covers {spoken} ({}); replying 00000000 and closing connection",
                offered.join(" ")
            );
            (NO_VERSION, Negotiation::Unsupported)
        }
    };
    stream.write_all(&reply)?;
    stream.flush()?;
    Ok(outcome)
}

/// The magic + version negotiation phase, under the ADR-0141 accept-time
/// deadline passed down from `serve_conn`. A timeout uses the same failure
/// semantic as the transport.rs peek and TLS accept sites: log and close
/// only this connection. The version reply follows the client's proposals
/// ([`select_version`], ADR-0303).
fn negotiate_magic<S: Read + Write + SocketTimeouts>(
    stream: &mut S,
    deadline: Option<Instant>,
) -> Result<Negotiation, Box<dyn std::error::Error + Send + Sync>> {
    // Every negotiation phase's arm + I/O + timeout mapping lives in
    // transport::phase; a spent deadline or a mid-exchange timeout logs and
    // aborts the connection there.
    match crate::transport::phase(
        stream,
        deadline,
        "bolt: handshake timeout during magic/version exchange; closing connection",
        |stream| handshake(stream, deadline),
    )? {
        Some(outcome) => Ok(outcome),
        None => Ok(Negotiation::Aborted),
    }
}

#[cfg(test)]
#[path = "protocol_breach_tests.rs"]
mod breach_tests;
#[cfg(test)]
#[path = "protocol_handshake_tests.rs"]
mod handshake_tests;
#[cfg(test)]
#[path = "protocol_hello_tests.rs"]
mod hello_tests;
#[cfg(test)]
#[path = "protocol_logon_tests.rs"]
mod logon_tests;
#[cfg(test)]
#[path = "protocol_namespace_tests.rs"]
mod namespace_tests;
#[cfg(test)]
#[path = "protocol_notification_tests.rs"]
mod notification_tests;
#[cfg(test)]
#[path = "protocol_param_tests.rs"]
mod param_tests;
#[cfg(test)]
#[path = "protocol_result_metadata_tests.rs"]
mod result_metadata_tests;
#[cfg(test)]
#[path = "protocol_route_auth_tests.rs"]
mod route_auth;
#[cfg(test)]
#[path = "protocol_runtime_error_tests.rs"]
mod runtime_error_tests;
#[cfg(test)]
#[path = "protocol_summary_tests.rs"]
mod summary_tests;
#[cfg(test)]
#[path = "protocol_telemetry_tests.rs"]
mod telemetry_tests;
#[cfg(test)]
#[path = "protocol_tests.rs"]
mod tests;
#[cfg(test)]
#[path = "protocol_tx_results_tests.rs"]
mod tx_results_tests;
#[cfg(test)]
#[path = "protocol_value_limit_tests.rs"]
mod value_limit_tests;
#[cfg(test)]
#[path = "protocol_values_tests.rs"]
mod values_tests;
