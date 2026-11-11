//! The unbound-surface flag policy: one declarative row per flag that
//! configures a specific surface, and one loop reporting the first row whose
//! flag this run set without binding that surface — never a silently ignored
//! value. Each row carries its flag, the run fact that sets it, and the
//! subject it configures ([`Subject`]); binding and the refusal message both
//! derive from that subject, and the listener vocabulary projects from
//! [`SURFACE_FLAGS`]. Adding the next surface-bound flag is one row.
//!
//! This is the posture ADR-0433/ADR-0435 state for a value the user clearly
//! passed: fail startup rather than discard it, because the flag expressed an
//! intent the run cannot honor. The environment values are not covered here:
//! `setup` still resolves them for every run, so a malformed variable stays a
//! loud startup error in the builds that compile its surface.
//!
//! `--host` and `--max-connections` are deliberately tolerated: they set the
//! posture of the serve command itself — the bind address and the admission
//! cap every listener shares, not one surface's story — and a stdio-only run
//! is on the safe side of both (no socket binds, no connection is ever
//! admitted), so they stay accepted uniformly.

use crate::parse_error::ParseError;

#[cfg(any(feature = "http", feature = "bolt"))]
use super::SurfaceFlag;
use super::{ServeCmd, SURFACE_FLAGS};

/// The listener spellings of the compiled surface flags — [`SURFACE_FLAGS`]
/// projected onto its listener half: each row renders its own
/// [`SurfaceFlag::listener_spelling`] — `--mcp` as its listener spelling
/// `--mcp PORT` (disambiguated from the `stdio` transport, which is not a
/// listener), every other surface flag as itself. The `--auth` rejection
/// message enumerates the listeners from this one projection. Compiled with
/// the mcp feature only: the only run with no listener is `--mcp stdio`,
/// which cannot exist without mcp.
#[cfg(feature = "mcp")]
pub(crate) fn listener_flag_spellings() -> Vec<&'static str> {
    SURFACE_FLAGS
        .iter()
        .map(|surface| surface.listener_spelling)
        .collect()
}

/// The HTTP-phase listener flags compiled into this build — the surfaces the
/// admission deadline flags configure (ADR-0451), for their unbound-value
/// messages. A projection of the one table, filtered by its
/// [`SurfaceFlag::http_header_phase`] column: a bolt listener has no HTTP
/// request phase (its handshake deadline is ADR-0141's), so it cannot bind
/// either deadline flag's subject.
#[cfg(any(feature = "http", feature = "mcp"))]
fn http_listener_spellings() -> Vec<&'static str> {
    SURFACE_FLAGS
        .iter()
        .filter(|surface| surface.http_header_phase)
        .map(|surface| surface.listener_spelling)
        .collect()
}

/// What an unbound flag configures: the surface/phase whose listener this
/// run must bind. Binding and the refusal message both derive from this one
/// value, so a row states its subject as data instead of carrying two fns.
enum Subject {
    /// One compiled surface's listener.
    #[cfg(any(feature = "http", feature = "bolt"))]
    Surface(&'static SurfaceFlag),
    /// The HTTP request-header or request-body phase of any HTTP-based
    /// listener; the string names the phase.
    #[cfg(any(feature = "http", feature = "mcp"))]
    HttpPhase(&'static str),
    /// Any listener at all (the `--mcp stdio`-only `--auth` case).
    #[cfg(feature = "mcp")]
    AnyListener,
}

impl Subject {
    /// Whether this run binds the subject.
    fn bound(&self, cmd: &ServeCmd) -> bool {
        match self {
            #[cfg(any(feature = "http", feature = "bolt"))]
            Self::Surface(surface) => (surface.binds)(cmd),
            #[cfg(any(feature = "http", feature = "mcp"))]
            Self::HttpPhase(_) => cmd.binds_http_based_listener(),
            #[cfg(feature = "mcp")]
            Self::AnyListener => cmd.has_listener(),
        }
    }

    /// The refusal message for `flag` (built only when `bound` is false).
    fn refusal(&self, flag: &str) -> String {
        match self {
            #[cfg(any(feature = "http", feature = "bolt"))]
            Self::Surface(surface) => unbound(flag, surface),
            #[cfg(any(feature = "http", feature = "mcp"))]
            Self::HttpPhase(phase) => unbound_http_phase(flag, phase),
            #[cfg(feature = "mcp")]
            Self::AnyListener => auth_unbound_message(flag),
        }
    }
}

/// One unbound-surface check: the flag, the run fact that sets it, and the
/// subject whose listener this run must bind for the flag to be valid.
struct UnboundFlag {
    /// The flag's command-line spelling.
    flag: &'static str,
    /// Whether this run set the flag.
    set: fn(&ServeCmd) -> bool,
    /// What the flag configures.
    subject: Subject,
}

/// The unbound-surface checks compiled into this build, in report order.
fn unbound_flags() -> impl Iterator<Item = UnboundFlag> {
    [
        // `--auth` applies to every listener surface; a run with no listener
        // (the `--mcp stdio` case) has no subject for it. Gated with the mcp
        // feature: no other build can produce a listener-less serve.
        #[cfg(feature = "mcp")]
        UnboundFlag {
            flag: "--auth",
            set: |cmd| cmd.auth.is_some(),
            subject: Subject::AnyListener,
        },
        #[cfg(feature = "http")]
        UnboundFlag {
            flag: "--origin",
            set: |cmd| !cmd.origins.is_empty(),
            subject: Subject::Surface(surface_row("--http")),
        },
        #[cfg(feature = "http")]
        UnboundFlag {
            flag: "--tx-idle-timeout-secs",
            set: |cmd| cmd.tx_idle_timeout_secs.is_some(),
            subject: Subject::Surface(surface_row("--http")),
        },
        #[cfg(feature = "http")]
        UnboundFlag {
            flag: "--max-open-transactions",
            set: |cmd| cmd.max_open_transactions.is_some(),
            subject: Subject::Surface(surface_row("--http")),
        },
        #[cfg(any(feature = "http", feature = "mcp"))]
        UnboundFlag {
            flag: "--header-deadline-secs",
            set: |cmd| cmd.header_deadline_secs.is_some(),
            subject: Subject::HttpPhase("HTTP request-header phase"),
        },
        #[cfg(any(feature = "http", feature = "mcp"))]
        UnboundFlag {
            flag: "--body-deadline-secs",
            set: |cmd| cmd.body_deadline_secs.is_some(),
            subject: Subject::HttpPhase("HTTP request-body phase"),
        },
        #[cfg(feature = "bolt")]
        UnboundFlag {
            flag: "--bolt-identity",
            set: |cmd| cmd.bolt_identity.is_some(),
            subject: Subject::Surface(surface_row("--bolt")),
        },
        #[cfg(feature = "bolt")]
        UnboundFlag {
            flag: "--ws-max-connections",
            set: |cmd| cmd.ws_max_connections.is_some(),
            subject: Subject::Surface(surface_row("--bolt")),
        },
        #[cfg(feature = "bolt")]
        UnboundFlag {
            flag: "--ws-allow-origin",
            set: |cmd| !cmd.ws_allow_origins.is_empty(),
            subject: Subject::Surface(surface_row("--bolt")),
        },
    ]
    .into_iter()
}

/// Reject the first flag whose configured subject this run does not bind.
pub(super) fn reject(cmd: &ServeCmd) -> Result<(), ParseError> {
    for row in unbound_flags() {
        if (row.set)(cmd) && !row.subject.bound(cmd) {
            return Err(row.subject.refusal(row.flag).into());
        }
    }
    Ok(())
}

// The uniform surface message: the flag and the table row that would bind
// its subject. The "pass <flag> <hint>" half projects from the row, so the
// listener vocabulary is not restated here.
#[cfg(any(feature = "http", feature = "bolt"))]
fn unbound(flag: &str, surface: &SurfaceFlag) -> String {
    format!(
        "serve {flag} configures the {} surface, which this run does \
         not serve: pass {} {} or drop {flag}",
        surface.flag, surface.flag, surface.value_hint
    )
}

// The HTTP-phase deadlines share a subject (both listeners), so their
// message is built once; the spelling list comes from the surface table.
#[cfg(any(feature = "http", feature = "mcp"))]
fn unbound_http_phase(flag: &str, phase: &str) -> String {
    format!(
        "serve {flag} configures the {phase} of the {} listeners, which this run \
         does not serve: pass one of them or drop {flag}",
        http_listener_spellings().join("/")
    )
}

#[cfg(feature = "mcp")]
fn auth_unbound_message(flag: &str) -> String {
    format!(
        "serve --mcp stdio skips auth (the host spawned the process); \
         {flag} applies to the {} listeners only",
        listener_flag_spellings().join("/")
    )
}

/// One compiled surface row, keyed by its command-line flag — the lookup the
/// unbound-flag messages use so their hint text projects from the table.
#[cfg(any(feature = "http", feature = "bolt"))]
fn surface_row(flag: &str) -> &'static SurfaceFlag {
    SURFACE_FLAGS
        .iter()
        .find(|surface| surface.flag == flag)
        .expect("the surface flag is in the compiled table")
}
