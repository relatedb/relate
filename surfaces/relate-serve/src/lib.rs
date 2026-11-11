//! The one accept loop every Relate network surface runs (ADR-0238):
//! thread-per-connection under a [`ConnectionCap`] (over-cap is
//! accept-then-close), accept errors logged and retried, each connection
//! handed to the surface's handler on its own explicitly sized thread.
//! Std-only and protocol-free — the surfaces (Bolt, HTTP Query API, MCP)
//! own their wire semantics; this crate owns the ADR-0110 accept policy on
//! sync primitives so it cannot drift between them.
//!
//! Public API:
//!
//! - [`serve_capped`] — the accept loop proper: bind the [`TcpListener`]
//!   yourself, then call with a log label, a per-connection stack size, a
//!   connection cap, and the handler closure (its `Err` is logged as the
//!   session error). Diverges by design: there is no shutdown condition;
//!   the process owns the listener's lifetime.
//! - [`ConnectionCap`] / [`ConnectionPermit`] — the cap primitive, exported
//!   for surfaces that accept on their own loop but share the policy.
//! - [`accept_resilient`] — one accept with the log-and-retry backoff.
//! - [`is_loopback_host`] / [`parse_port`] — the shared bind-posture
//!   predicate and port-flag parser (ADR-0248), so the CLI's `serve` and
//!   the container entrypoint cannot drift on either.
//! - [`origin`] — the ADR-0266 browser-Origin classification both network
//!   surfaces run (bolt's WebSocket upgrade guard, the HTTP surfaces'
//!   browser guard): the origin-shape parse, the loopback set, and the
//!   wildcard/exact allowlist matcher, stated once so the surfaces cannot
//!   drift on it.
//! - [`unlimited_cap_warning`] — the ADR-0333 unlimited-cap startup
//!   warning, the policy beside [`resolve_connection_cap`]'s resolution,
//!   shared by the CLI's `serve` and bolt's standalone server.
//! - [`resolve_flag_or_env`] — the one flag → environment → default ladder
//!   behind both serve knobs (ADR-0333), shared with the HTTP surface's
//!   managed-transaction limits (ADR-0435 §4).
//! - [`DEFAULT_MAX_CONNECTIONS`] / [`DEFAULT_HEADER_DEADLINE`] and their
//!   resolvers — the ADR-0451 admission defaults (a non-zero cap and an
//!   absolute header deadline) stated once for every surface.
//! - [`DEFAULT_CONNECTION_STACK_BYTES`] — a sane connection-thread stack
//!   for surfaces whose heavy work (query execution) already rides the
//!   engine's own 64 MiB worker thread; a surface that parses deeply on
//!   the connection thread (Bolt's protocol work) passes its own size.
//! - [`signal::wait_shutdown_signal`] — the one shutdown-signal wait
//!   (SIGINT/SIGTERM on unix, Ctrl-C elsewhere), so no surface hand-rolls
//!   its own. The accept loop itself stays std-only; this is the crate's
//!   only dependency beyond std.

pub mod origin;
pub mod signal;

use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// The default per-connection thread stack: 8 MiB. Query execution runs on
/// the engine's own 64 MiB query-worker thread (`relate`'s
/// `query_worker`), so a connection thread only covers request parsing and
/// response encoding; 8 MiB heads off deep JSON/document nesting with
/// room to spare. Bolt passes its own 64 MiB (ADR-0233) because its
/// protocol layer works the connection thread harder.
pub const DEFAULT_CONNECTION_STACK_BYTES: usize = 8 * 1024 * 1024;

/// The accept-error backoff (ADR-0110): log, wait, retry — accept errors
/// never terminate the process.
const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_millis(100);

/// The default process-wide connection cap: 128 open connections. One
/// shared cap (ADR-0333) with a non-zero default (ADR-0451): the cap bounds
/// unauthenticated session admission — one thread and one file descriptor
/// per connection before any credential is checked — while staying inside a
/// common `ulimit -n` of 1024 and at half the engine's 256-slot
/// snapshot-pin table, the ratio ADR-0435 chose for managed transactions,
/// so admitted connections leave pin headroom for auto-commit and the other
/// pin consumers. `0` is the explicit unlimited spelling.
pub const DEFAULT_MAX_CONNECTIONS: usize = 128;

/// The default WebSocket session cap: `0` is unlimited (ADR-0459). This
/// opt-in knob sits beside the process-wide [`DEFAULT_MAX_CONNECTIONS`] so
/// the resolver, the crate CLI args, and their tests state one value: a
/// database has legitimate bulk data-transfer workloads with many parallel
/// sessions, so an unconfigured WebSocket listener is not capped beyond the
/// accept-time connection cap.
pub const DEFAULT_WS_MAX_CONNECTIONS: usize = 0;

/// The default request-body cap: 16 MiB, shared by both HTTP-based
/// listeners (the Query API and the MCP streamable-HTTP transport).
pub const DEFAULT_MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// The default per-read timeout for the HTTP-based listeners: a stalled
/// read is dropped instead of pinning its connection thread (slowloris
/// protection; Bolt's idle bar is 300 s, but one-request-per-connection
/// needs far less). This bounds one blocking read; the header and body
/// deadlines bound whole phases, so a peer dribbling one byte per timeout
/// window still cannot hold a connection.
pub const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// The default absolute deadline for receiving a complete request header
/// block: 10 seconds from the start of the connection handler. A peer that
/// has not terminated its headers by then is dropped however slowly it
/// trickles bytes; the surfaces re-arm the per-read socket timeout to the
/// remaining deadline on every header read (ADR-0451). `0` disables the
/// deadline.
pub const DEFAULT_HEADER_DEADLINE: Duration = Duration::from_secs(10);

/// The default absolute deadline for receiving a complete request body:
/// 60 seconds from the start of the body read. The body cap is 16 MiB, so
/// this leaves headroom for a full-cap upload at roughly 2.2 Mbit/s while
/// still dropping a trickled body at a fixed wall-clock bound (ADR-0451).
/// Measured loopback uploads of the full 16 MiB cap complete in ~30 ms, so
/// the default is generous rather than tight. The surfaces re-arm the
/// per-read socket timeout to the remaining deadline on every body read.
/// `0` disables the deadline.
pub const DEFAULT_BODY_DEADLINE: Duration = Duration::from_secs(60);

/// The largest accepted admission deadline: one day. A larger value is a
/// configuration mistake (and `Instant::now() + duration` can overflow), so
/// both deadline resolvers reject it loudly instead of panicking per
/// connection at request time (ADR-0451).
pub const MAX_ADMISSION_DEADLINE: Duration = Duration::from_secs(24 * 60 * 60);

/// True when `host` names a loopback interface — the one definition every
/// network surface and the container entrypoint shares (the ADR-0248
/// loopback-default posture). Parse-based, not a prefix test: the strict
/// std parser keeps `127.0.0.1.evil.com` (a hostname, NOT loopback) from
/// slipping through a `starts_with("127.")` check, while any real
/// 127.0.0.0/8 or `::1` address is caught. An unparseable host is a
/// hostname and answers `false` — the warning posture errs toward treating
/// it as potentially remote.
pub fn is_loopback_host(host: &str) -> bool {
    if matches!(host, "localhost" | "[::1]") {
        return true;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(addr) => addr.is_loopback(),
        Err(_) => false,
    }
}

/// Parse a `--flag PORT` value (the surfaces' port flags carry bare ports),
/// naming the flag in the error — the one port parser the CLI's `serve`
/// and the container entrypoint share.
pub fn parse_port(flag: &str, value: &str) -> Result<u16, String> {
    value
        .parse::<u16>()
        .map_err(|_| format!("{flag} expects a port number (0-65535), got {value:?}"))
}

/// The one flag → environment → default ladder behind the serve knobs
/// (ADR-0333's resolution policy, reused by ADR-0435 §4): the flag wins, a
/// blank environment value counts as omitted and yields the caller's
/// `default` — whatever it is, zero/unlimited included — and a malformed
/// value is a loud error naming the variable, never a silent fallback.
pub fn resolve_flag_or_env(flag: Option<u64>, var: &str, default: u64) -> Result<u64, String> {
    if let Some(value) = flag {
        return Ok(value);
    }
    match std::env::var(var) {
        Ok(value) if value.trim().is_empty() => Ok(default),
        Ok(value) => value
            .trim()
            .parse::<u64>()
            .map_err(|_| format!("invalid {var} '{}'", value.trim())),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(err) => Err(format!("invalid {var}: {err}")),
    }
}

/// A count cap's `u64` → platform `usize` conversion, naming `knob` in the
/// one overflow error. Public so the Bolt crate's clap parsers share it
/// with [`resolve_connection_cap`] / [`resolve_ws_connection_cap`] instead
/// of restating the conversion and its message.
pub fn count_cap_usize(knob: &str, raw: u64) -> Result<usize, String> {
    usize::try_from(raw).map_err(|_| format!("{knob} does not fit this platform"))
}

/// The two count caps' one resolution: the flag → environment → default
/// ladder ([`resolve_flag_or_env`]) plus the platform `usize` conversion
/// ([`count_cap_usize`]), with `knob` naming the flag in the one overflow
/// error. The sibling of the deadline pair's shared core, so the caps
/// cannot drift on either the resolution order or the error shape.
fn resolve_count_cap(
    flag: Option<u64>,
    var: &str,
    default: usize,
    knob: &str,
) -> Result<usize, String> {
    let raw = resolve_flag_or_env(flag, var, default as u64)?;
    count_cap_usize(knob, raw)
}

/// `--max-connections`, else `RELATE_MAX_CONNECTIONS`, else
/// [`DEFAULT_MAX_CONNECTIONS`] (128; ADR-0451). A blank environment value
/// is omitted, matching the engine's other knobs. `0` is the explicit
/// unlimited spelling, which still warns on a non-loopback bind
/// ([`unlimited_cap_warning`]).
pub fn resolve_connection_cap(flag: Option<u64>) -> Result<usize, String> {
    resolve_count_cap(
        flag,
        "RELATE_MAX_CONNECTIONS",
        DEFAULT_MAX_CONNECTIONS,
        "max-connections",
    )
}

/// `--ws-max-connections`, else `RELATE_WS_MAX_CONNECTIONS`, else
/// unlimited (`0`; ADR-0459). The opt-in WebSocket session cap: unset means
/// WebSocket sessions are not capped beyond the accept-time
/// [`resolve_connection_cap`] bound, because bulk data-transfer deployments
/// legitimately run many parallel sessions. A blank environment value is
/// omitted, matching the sibling knobs.
pub fn resolve_ws_connection_cap(flag: Option<u64>) -> Result<usize, String> {
    resolve_count_cap(
        flag,
        "RELATE_WS_MAX_CONNECTIONS",
        DEFAULT_WS_MAX_CONNECTIONS,
        "ws-max-connections",
    )
}

/// `--header-deadline-secs`, else `RELATE_HEADER_DEADLINE_SECS`, else
/// [`DEFAULT_HEADER_DEADLINE`] (10 s; ADR-0451). A blank environment value
/// is omitted. `0` disables the absolute header deadline.
pub fn resolve_header_deadline(flag: Option<u64>) -> Result<Duration, String> {
    resolve_deadline(
        flag,
        "RELATE_HEADER_DEADLINE_SECS",
        "--header-deadline-secs",
        "header",
        DEFAULT_HEADER_DEADLINE,
    )
}

/// `--body-deadline-secs`, else `RELATE_BODY_DEADLINE_SECS`, else
/// [`DEFAULT_BODY_DEADLINE`] (60 s; ADR-0451). A blank environment value is
/// omitted. `0` disables the absolute body deadline.
pub fn resolve_body_deadline(flag: Option<u64>) -> Result<Duration, String> {
    resolve_deadline(
        flag,
        "RELATE_BODY_DEADLINE_SECS",
        "--body-deadline-secs",
        "body",
        DEFAULT_BODY_DEADLINE,
    )
}

/// The two admission deadlines' one resolution: flag → environment →
/// default, then the shared [`MAX_ADMISSION_DEADLINE`] range check. The
/// validator owns the bound's text; this adds only the flag/environment
/// context the CLI needs, so the message's bound is stated once.
fn resolve_deadline(
    flag: Option<u64>,
    var: &str,
    flag_name: &str,
    phase: &str,
    default: Duration,
) -> Result<Duration, String> {
    let raw = resolve_flag_or_env(flag, var, default.as_secs())?;
    let configured = Duration::from_secs(raw);
    validate_admission_deadline(phase, configured)
        .map_err(|e| format!("{flag_name} ({var}): {e}"))?;
    Ok(configured)
}

/// Validate one configured admission deadline against
/// [`MAX_ADMISSION_DEADLINE`]. The resolvers enforce this for the CLI; the
/// library entry points call this for a directly constructed config, so the
/// `Instant::now() + duration` arithmetic behind the read budgets never sees
/// an out-of-range value.
pub fn validate_admission_deadline(phase: &str, configured: Duration) -> Result<(), String> {
    if configured > MAX_ADMISSION_DEADLINE {
        return Err(format!(
            "{phase} deadline {configured:?} exceeds the maximum of {MAX_ADMISSION_DEADLINE:?}"
        ));
    }
    Ok(())
}

/// Validate both HTTP-phase admission deadlines — the one library-level check
/// the HTTP and MCP entry points run before serving.
pub fn validate_admission_deadlines(header: Duration, body: Duration) -> Result<(), String> {
    validate_admission_deadline("header", header)?;
    validate_admission_deadline("body", body)
}

/// The ADR-0333 unlimited-cap startup warning, the one statement of the
/// policy beside [`resolve_connection_cap`]'s resolution: a serve whose
/// connection cap is unlimited (`max == 0`) on a non-loopback bind warns
/// loudly at startup — each accepted connection takes a thread before
/// authentication — and a bounded cap or a loopback-only bind stays
/// silent. `None` is the silent case.
///
/// The loopback verdict is the caller's, not re-derived here: the two
/// callers classify different bind shapes (the CLI's bare `--host` through
/// [`is_loopback_host`], bolt's `host:port` bind through its own
/// socket-addr resolution), so this folds the condition over the verdict.
/// The line itself is the one canonical text (round 3, F-3-1), carried in
/// full here and parameterized only by the genuinely per-surface
/// `bind_description` (the CLI's `--host HOST`, bolt's `bind SPEC`): both
/// callers print the returned line as-is, with no per-caller prefix or
/// capitalization to reassemble.
pub fn unlimited_cap_warning(
    max: usize,
    bind_loopback: bool,
    bind_description: &str,
) -> Option<String> {
    if max != 0 || bind_loopback {
        return None;
    }
    Some(format!(
        "WARNING: --max-connections is unlimited on non-loopback {bind_description}. \
         Each accepted connection takes a thread before authentication. \
         Set --max-connections to bound it."
    ))
}

/// The one over-cap close line every cap consumer prints verbatim
/// (ADR-0110, ADR-0459): the accept-time `--max-connections` refusal in
/// [`serve_capped`] and Bolt's opt-in `--ws-max-connections` refusal
/// render it, parameterized only by the surface label, the flag that set
/// the cap, and the cap. One statement of the text, the sibling of
/// [`unlimited_cap_warning`].
pub fn over_cap_warning(surface: &str, flag: &str, max: usize) -> String {
    format!("{surface}: {flag} {max} reached; closing connection")
}

/// A shared connection cap (ADR-0110's policy on sync primitives): one slot
/// per open connection, released when the returned [`ConnectionPermit`]
/// drops at the end of the connection's thread.
#[derive(Debug)]
pub struct ConnectionCap {
    open: AtomicUsize,
    max: usize,
}

impl ConnectionCap {
    /// A cap admitting at most `max` open connections.
    pub fn new(max: usize) -> Self {
        Self {
            open: AtomicUsize::new(0),
            max,
        }
    }

    /// The configured maximum.
    pub fn max(&self) -> usize {
        self.max
    }

    /// Admit one connection, or return `None` at cap (the caller's policy
    /// is accept-then-close). `max == 0` is unlimited and never refuses
    /// (ADR-0333). The permit holds the slot until dropped.
    pub fn try_acquire(self: &Arc<Self>) -> Option<ConnectionPermit> {
        if self.max == 0 {
            self.open.fetch_add(1, Ordering::AcqRel);
            return Some(ConnectionPermit(Arc::clone(self)));
        }
        // CAS loop: claim a slot only while below the cap.
        let mut open = self.open.load(Ordering::Relaxed);
        loop {
            if open >= self.max {
                return None;
            }
            match self.open.compare_exchange_weak(
                open,
                open + 1,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(ConnectionPermit(Arc::clone(self))),
                Err(now) => open = now,
            }
        }
    }
}

/// One admitted connection's slot in a [`ConnectionCap`].
#[derive(Debug)]
pub struct ConnectionPermit(Arc<ConnectionCap>);

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Accept one connection, logging and retrying accept errors (they never
/// terminate the server — ADR-0110's policy, on sync primitives), with
/// Nagle's algorithm disabled on the accepted stream (LABS-911): every
/// surface's responses are small — a Bolt SUCCESS, one JSON-RPC body — and
/// with Nagle the kernel holds the final segment back waiting for the
/// peer's ACK, stretching a sub-millisecond response to tens of
/// milliseconds on Linux (45 ms for `RETURN 1` over Bolt in a Docker VM,
/// 0.12 ms with the option set). The one seam every surface's stream
/// passes through — bolt, http, mcp, and the WebSocket upgrade that rides
/// the http stream — so the option is stated once here.
pub fn accept_resilient(listener: &TcpListener, surface: &str) -> TcpStream {
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                // A failure to set the option degrades to Nagle's behavior,
                // never worse — log it and keep the connection.
                if let Err(e) = stream.set_nodelay(true) {
                    eprintln!(
                        "{surface}: set_nodelay failed: {e}; \
                         small responses may be delayed"
                    );
                }
                return stream;
            }
            Err(e) => {
                eprintln!("{surface}: accept error: {e}; retrying");
                std::thread::sleep(ACCEPT_ERROR_BACKOFF);
            }
        }
    }
}

/// The shared accept loop every network surface runs: thread-per-connection
/// under a [`ConnectionCap`] (over-cap is accept-then-close), accept errors
/// retried via [`accept_resilient`], each connection handed to `handler` on
/// its own thread spawned with an explicit `stack_bytes`. A thread-spawn
/// failure is logged and the connection closed — never silently dropped.
/// `surface` labels the log lines (`"bolt"`, `"http"`, `"mcp"`). Diverges
/// by design — there is no shutdown condition; the process owns the
/// listener's lifetime.
pub fn serve_capped<E: std::fmt::Display>(
    listener: TcpListener,
    surface: &'static str,
    stack_bytes: usize,
    cap: Arc<ConnectionCap>,
    handler: impl Fn(TcpStream) -> Result<(), E> + Send + Sync + 'static,
) -> ! {
    let handler = Arc::new(handler);
    loop {
        let stream = accept_resilient(&listener, surface);
        let Some(permit) = cap.try_acquire() else {
            eprintln!(
                "{}",
                over_cap_warning(surface, "--max-connections", cap.max())
            );
            drop(stream);
            continue;
        };
        let handler = Arc::clone(&handler);
        let spawned = std::thread::Builder::new()
            .stack_size(stack_bytes)
            .spawn(move || {
                let _permit = permit;
                // The loop owns the `surface` label, so it owns the
                // per-session error line too — callers return the result.
                if let Err(e) = handler(stream) {
                    eprintln!("{surface} session: {e}");
                }
            });
        if let Err(e) = spawned {
            eprintln!("{surface}: failed to spawn the connection thread: {e}; closing connection");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use relate_test_support::EnvRestore;

    #[test]
    fn loopback_hosts_parse_and_hostnames_do_not_pass_as_loopback() {
        for host in ["127.0.0.1", "127.0.1.1", "::1", "[::1]", "localhost"] {
            assert!(is_loopback_host(host), "{host}");
        }
        for host in [
            "0.0.0.0",
            "192.168.1.10",
            "127.0.0.1.evil.com",
            "docker0",
            "::",
        ] {
            assert!(!is_loopback_host(host), "{host}");
        }
    }

    #[test]
    fn parse_port_names_its_flag_in_the_error() {
        assert_eq!(parse_port("--http", "7474").unwrap(), 7474);
        let err = parse_port("--mcp", "abc").unwrap_err();
        assert!(err.contains("--mcp"), "{err}");
        assert!(parse_port("--mcp", "70000").is_err());
    }

    #[test]
    fn count_cap_usize_converts_the_count() {
        assert_eq!(count_cap_usize("max-connections", 3).unwrap(), 3);
        assert_eq!(
            count_cap_usize("ws-max-connections", 0).unwrap(),
            DEFAULT_WS_MAX_CONNECTIONS
        );
    }

    /// The overflow branch exists only where `usize` is narrower than
    /// `u64`; on a 64-bit target the conversion is lossless.
    #[cfg(target_pointer_width = "32")]
    #[test]
    fn count_cap_usize_names_its_knob_on_overflow() {
        let err = count_cap_usize("ws-max-connections", u64::MAX).unwrap_err();
        assert!(err.contains("ws-max-connections"), "{err}");
        assert!(err.contains("does not fit this platform"), "{err}");
    }

    #[test]
    fn resolve_connection_cap_flag_then_env_then_default() {
        // The flag path is pure — the flag wins over everything, and
        // `0` is the unlimited spelling passing through as the
        // unlimited cap (ADR-0333).
        assert_eq!(resolve_connection_cap(Some(0)).unwrap(), 0);
        assert_eq!(resolve_connection_cap(Some(3)).unwrap(), 3);
        // The env path reads the process environment, which parallel
        // tests share — the shared EnvRestore guard
        // (relate-test-support): restore on drop, panic path
        // included, and no other test in this binary touches the
        // variable.
        let _restore = EnvRestore::new("RELATE_MAX_CONNECTIONS");
        std::env::set_var("RELATE_MAX_CONNECTIONS", "5");
        assert_eq!(resolve_connection_cap(None).unwrap(), 5);
        // A blank environment value is omitted, matching the engine's
        // other knobs: unlimited.
        std::env::set_var("RELATE_MAX_CONNECTIONS", "  ");
        assert_eq!(
            resolve_connection_cap(None).unwrap(),
            DEFAULT_MAX_CONNECTIONS
        );
        std::env::remove_var("RELATE_MAX_CONNECTIONS");
        assert_eq!(
            resolve_connection_cap(None).unwrap(),
            DEFAULT_MAX_CONNECTIONS
        );
        // A malformed value is a loud error, never a silent unlimited.
        std::env::set_var("RELATE_MAX_CONNECTIONS", "wat");
        let err = resolve_connection_cap(None).unwrap_err();
        assert!(err.contains("RELATE_MAX_CONNECTIONS"), "{err}");
    }

    #[test]
    fn resolve_ws_connection_cap_unset_is_unlimited() {
        // The opt-in WS cap (ADR-0459): no flag, no env is the shared
        // default (unlimited), not the process cap's 128.
        assert_eq!(DEFAULT_WS_MAX_CONNECTIONS, 0);
        assert_eq!(resolve_ws_connection_cap(Some(0)).unwrap(), 0);
        assert_eq!(resolve_ws_connection_cap(Some(3)).unwrap(), 3);
        let _restore = EnvRestore::new("RELATE_WS_MAX_CONNECTIONS");
        std::env::set_var("RELATE_WS_MAX_CONNECTIONS", "5");
        assert_eq!(resolve_ws_connection_cap(None).unwrap(), 5);
        std::env::set_var("RELATE_WS_MAX_CONNECTIONS", "  ");
        assert_eq!(
            resolve_ws_connection_cap(None).unwrap(),
            DEFAULT_WS_MAX_CONNECTIONS
        );
        std::env::remove_var("RELATE_WS_MAX_CONNECTIONS");
        assert_eq!(
            resolve_ws_connection_cap(None).unwrap(),
            DEFAULT_WS_MAX_CONNECTIONS
        );
        std::env::set_var("RELATE_WS_MAX_CONNECTIONS", "wat");
        let err = resolve_ws_connection_cap(None).unwrap_err();
        assert!(err.contains("RELATE_WS_MAX_CONNECTIONS"), "{err}");
    }

    #[test]
    fn resolve_header_deadline_flag_then_env_then_default() {
        assert_eq!(
            resolve_header_deadline(Some(3)).unwrap(),
            Duration::from_secs(3)
        );
        // `0` disables the deadline explicitly.
        assert_eq!(resolve_header_deadline(Some(0)).unwrap(), Duration::ZERO);
        let _restore = EnvRestore::new("RELATE_HEADER_DEADLINE_SECS");
        std::env::set_var("RELATE_HEADER_DEADLINE_SECS", "7");
        assert_eq!(
            resolve_header_deadline(None).unwrap(),
            Duration::from_secs(7)
        );
        std::env::set_var("RELATE_HEADER_DEADLINE_SECS", "  ");
        assert_eq!(
            resolve_header_deadline(None).unwrap(),
            DEFAULT_HEADER_DEADLINE
        );
        std::env::set_var("RELATE_HEADER_DEADLINE_SECS", "soon");
        let err = resolve_header_deadline(None).unwrap_err();
        assert!(err.contains("RELATE_HEADER_DEADLINE_SECS"), "{err}");
    }

    #[test]
    fn resolve_body_deadline_has_its_own_default_and_range() {
        assert_eq!(
            resolve_body_deadline(Some(30)).unwrap(),
            Duration::from_secs(30)
        );
        assert_eq!(resolve_body_deadline(Some(0)).unwrap(), Duration::ZERO);
        assert_eq!(resolve_body_deadline(None).unwrap(), DEFAULT_BODY_DEADLINE);
        // The overflow guard: a value that would overflow `Instant::now() +
        // duration` is a loud resolution error naming the accepted range,
        // never a per-connection panic. Both resolvers share the check.
        type Resolver = fn(Option<u64>) -> Result<Duration, String>;
        for (resolve, flag, var) in [
            (
                resolve_header_deadline as Resolver,
                "--header-deadline-secs",
                "RELATE_HEADER_DEADLINE_SECS",
            ),
            (
                resolve_body_deadline as Resolver,
                "--body-deadline-secs",
                "RELATE_BODY_DEADLINE_SECS",
            ),
        ] {
            let max = MAX_ADMISSION_DEADLINE.as_secs();
            let err = resolve(Some(u64::MAX)).unwrap_err();
            assert!(err.contains(flag), "{flag}: {err}");
            assert!(err.contains(var), "{flag}: {err}");
            assert!(err.contains(&max.to_string()), "{flag}: {err}");
            // The bound itself is accepted.
            assert_eq!(resolve(Some(max)).unwrap(), MAX_ADMISSION_DEADLINE);
            let _restore = EnvRestore::new(var);
            std::env::set_var(var, (max + 1).to_string());
            let env_err = resolve(None).unwrap_err();
            assert!(env_err.contains(var), "{var}: {env_err}");
            std::env::remove_var(var);
        }
    }

    #[test]
    fn the_admission_defaults_are_non_zero() {
        // The ADR-0451 security default: an unconfigured serve is bounded.
        // Const blocks keep the claim a compile-time invariant.
        const { assert!(DEFAULT_MAX_CONNECTIONS > 0) };
        const { assert!(!DEFAULT_HEADER_DEADLINE.is_zero()) };
        const { assert!(!DEFAULT_BODY_DEADLINE.is_zero()) };
    }

    #[test]
    fn connection_cap_admits_up_to_max_then_refuses() {
        let cap = Arc::new(ConnectionCap::new(2));
        let first = cap.try_acquire().expect("first slot");
        let second = cap.try_acquire().expect("second slot");
        assert!(cap.try_acquire().is_none(), "third connection is over cap");
        drop(first);
        assert!(
            cap.try_acquire().is_some(),
            "a dropped permit frees its slot"
        );
        drop(second);
    }

    #[test]
    fn unlimited_connection_cap_never_refuses() {
        let cap = Arc::new(ConnectionCap::new(0));
        let mut held = Vec::new();
        for _ in 0..8 {
            held.push(cap.try_acquire().expect("unlimited cap admits"));
        }
        assert_eq!(cap.max(), 0);
    }

    #[test]
    fn unlimited_cap_warning_fires_only_for_an_unlimited_cap_off_loopback() {
        // The one ADR-0333 warning policy (round 2, F-2-1), shared by the
        // CLI's serve and bolt's standalone server: the condition folds
        // here, and the rendered line is the canonical text (round 3,
        // F-3-1) — asserted as string literals, so drift from it is a
        // failure, not a tautology.
        assert_eq!(
            unlimited_cap_warning(0, false, "--host 0.0.0.0"),
            Some(
                "WARNING: --max-connections is unlimited on non-loopback --host 0.0.0.0. \
                 Each accepted connection takes a thread before authentication. \
                 Set --max-connections to bound it."
                    .to_string()
            ),
            "the CLI's rendered line"
        );
        assert_eq!(
            unlimited_cap_warning(0, false, "bind 0.0.0.0:7687"),
            Some(
                "WARNING: --max-connections is unlimited on non-loopback bind 0.0.0.0:7687. \
                 Each accepted connection takes a thread before authentication. \
                 Set --max-connections to bound it."
                    .to_string()
            ),
            "bolt's rendered line"
        );
        // A bounded cap and a loopback-only bind stay silent.
        assert_eq!(unlimited_cap_warning(4, false, "bind 0.0.0.0:7687"), None);
        assert_eq!(unlimited_cap_warning(0, true, "--host 127.0.0.1"), None);
    }

    #[test]
    fn over_cap_warning_is_the_one_rendered_line() {
        // The accept-time and WebSocket caps print the same text (ADR-0459),
        // parameterized by surface, flag and max. Asserted as literals so a
        // drift from the canonical line fails.
        assert_eq!(
            over_cap_warning("bolt", "--max-connections", 128),
            "bolt: --max-connections 128 reached; closing connection"
        );
        assert_eq!(
            over_cap_warning("bolt", "--ws-max-connections", 4),
            "bolt: --ws-max-connections 4 reached; closing connection"
        );
        assert_eq!(
            over_cap_warning("http", "--max-connections", 1),
            "http: --max-connections 1 reached; closing connection"
        );
    }

    #[test]
    fn accept_resilient_returns_an_accepted_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).unwrap();
        let accepted = accept_resilient(&listener, "test");
        // LABS-911: the accepted stream carries TCP_NODELAY — the getter
        // reads the socket option the setter wrote, so this pins the
        // option on every platform that has one.
        assert_eq!(
            accepted.nodelay().map_err(|e| e.to_string()),
            Ok(true),
            "the accepted stream must disable Nagle's algorithm"
        );
        drop(accepted);
        drop(client);
    }
}
