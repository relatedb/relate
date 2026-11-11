//! The HTTP Query API's managed-transaction limits (ADR-0435 §4): the idle
//! deadline every registry operation sweeps against and the cap on open
//! registry entries — plus the one renderable-deadline policy that keeps
//! every accepted limit expressible on the wire.
//!
//! The model is typed: an idle timeout of `None` disables expiry and a cap
//! of `None` is unlimited. The wire/flag spelling keeps ADR-0435's `0` for
//! exactly those two states, and [`TxLimits::new`] converts it at that one
//! boundary — every consumer past the constructor pattern-matches the
//! model, never a re-derived zero sentinel.
//!
//! The limits type lives here, not in `relate-serve`, because the
//! invariant it carries is the renderer's: every `transaction` block
//! renders `expires` as a wall-clock deadline derived from the entry's
//! remaining idle allowance (the tx module's `expires_at`), so a limit
//! `chrono` cannot render must be refused where that rendering lives.
//! [`TxLimits::new`] runs `validate_tx_limits`, which applies the render's
//! own arithmetic on the render's own clock — `Utc::now()` plus the idle
//! timeout plus the one-day [`RENDER_MARGIN`] — so a value accepted at
//! startup stays renderable for at least a day of uptime. The fields are
//! private, so no construction path — the flag/env resolver, an embedder's
//! [`crate::TxRegistry::with_limits`], or a test — can build limits past
//! the bound. The rendering then `expect`s the invariant instead of
//! omitting the field.
//!
//! Resolution follows the ADR-0333 ladder stated once in
//! [`relate_serve::resolve_flag_or_env`]: the flag wins, a blank
//! environment value counts as omitted and yields the default — never
//! disabled/unlimited — and a malformed value is a loud error naming the
//! variable, never a silent fallback.

use std::time::Duration;

use relate_serve::resolve_flag_or_env;

/// The default managed-transaction idle timeout: 60 seconds (ADR-0435 §1)
/// — the value the Query API v2 wire already advertised, now the enforced
/// default. The wire spelling `0` disables expiry (the model's `None`).
pub const DEFAULT_TX_IDLE_TIMEOUT_SECS: u64 = 60;

/// The default cap on open managed transactions: 128 (ADR-0435 §2) — half
/// the engine's 256 process-wide snapshot pin slots, leaving the rest for
/// auto-commit statements and the other pin consumers. The wire spelling
/// `0` is unlimited (the model's `None`).
pub const DEFAULT_MAX_OPEN_TRANSACTIONS: usize = 128;

/// The margin the renderability bound allows on top of the idle timeout:
/// one day of uptime safety — a value accepted at startup stays renderable
/// for the server's lifetime given that margin, because the render clock
/// can advance by up to a day before `now + idle_timeout` reaches
/// `chrono::DateTime::<chrono::Utc>::MAX_UTC`. A serve that outlives the
/// margin would only matter for an idle timeout within a day of the
/// representable ceiling, i.e. one no caller configures.
const RENDER_MARGIN: Duration = Duration::from_secs(24 * 60 * 60);

/// The managed-transaction limits (ADR-0435 §4): the idle deadline every
/// registry operation sweeps against and the cap on open registry entries.
/// The typed model's `None` arms are the two off states — expiry disabled
/// and unlimited — which the constructor converts from the wire's `0`
/// spelling.
///
/// The fields are private and [`TxLimits::new`] validates, so an
/// unrenderable idle timeout is unrepresentable: the reply's `expires`
/// rendering can rely on the invariant (it is the constructor's, not the
/// request's).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TxLimits {
    /// How long an idle open transaction may live before the next registry
    /// operation expires it. `None` disables expiry.
    idle_timeout: Option<Duration>,
    /// The cap on open managed transactions. `None` is unlimited.
    max_open: Option<usize>,
}

impl TxLimits {
    /// The limits `idle_timeout` and `max_open`, refused when `idle_timeout`
    /// cannot be rendered as a reply deadline (see `validate_tx_limits`).
    /// The arguments carry the ADR-0435 wire/flag spelling — `Duration::ZERO`
    /// disables expiry and `0` is unlimited — converted here, at the one
    /// boundary, into the typed model. The one public constructor: the
    /// resolver, the embedder, and the tests all build limits through it.
    pub fn new(idle_timeout: Duration, max_open: usize) -> Result<Self, String> {
        let limits = Self {
            idle_timeout: (!idle_timeout.is_zero()).then_some(idle_timeout),
            max_open: (max_open != 0).then_some(max_open),
        };
        validate_tx_limits(&limits)?;
        Ok(limits)
    }

    /// How long an idle open transaction may live before the next registry
    /// operation expires it; `None` when expiry is disabled.
    pub fn idle_timeout(&self) -> Option<Duration> {
        self.idle_timeout
    }

    /// The cap on open managed transactions; `None` when unlimited.
    pub fn max_open(&self) -> Option<usize> {
        self.max_open
    }
}

impl Default for TxLimits {
    /// The ADR-0435 defaults (60 s idle, 128 open), through the validating
    /// constructor like every other path — they are known-renderable, so a
    /// refusal here is a broken default, not a caller error.
    fn default() -> Self {
        Self::new(
            Duration::from_secs(DEFAULT_TX_IDLE_TIMEOUT_SECS),
            DEFAULT_MAX_OPEN_TRANSACTIONS,
        )
        .expect("the ADR-0435 default limits are inside the renderable deadline range")
    }
}

/// The renderability deadline bound (ADR-0435 §1), stated once beside the
/// renderer it protects: `expires` is derived from the entry's remaining
/// idle allowance, so both the `chrono::Duration` conversion and the
/// datetime addition must succeed for the configured idle timeout plus the
/// one-day render margin. The check runs the render's OWN arithmetic
/// ([`render_deadline`]) against the same clock, so validation and
/// rendering cannot disagree: an idle timeout is accepted exactly when
/// `Utc::now() + idle_timeout + RENDER_MARGIN` is representable. An idle
/// timeout `chrono` cannot hold (a `u64` seconds flag reaches far past it)
/// is refused here rather than panicking or omitting the field on the
/// request path.
///
/// Private: [`TxLimits::new`] is the one construction path, so this is the
/// invariant behind it, not a second public entry.
fn validate_tx_limits(limits: &TxLimits) -> Result<(), String> {
    let Some(idle_timeout) = limits.idle_timeout() else {
        return Ok(());
    };
    if idle_timeout
        .checked_add(RENDER_MARGIN)
        .is_some_and(|horizon| render_deadline(horizon).is_some())
    {
        return Ok(());
    }
    Err(format!(
        "idle timeout of {}s is beyond the expiry deadline range the HTTP surface \
         can render (via --tx-idle-timeout-secs / RELATE_TX_IDLE_TIMEOUT_SECS)",
        idle_timeout.as_secs()
    ))
}

/// The one deadline rendering: `now + remaining`, or `None` when `chrono`
/// cannot represent the duration or the resulting instant. The reply
/// rendering uses it, and the constructor's validation runs this same
/// function on `idle_timeout + RENDER_MARGIN`, so the two cannot disagree
/// on what is renderable.
pub(crate) fn render_deadline(remaining: Duration) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::Duration::from_std(remaining)
        .ok()
        .and_then(|delta| chrono::Utc::now().checked_add_signed(delta))
}

/// `--tx-idle-timeout-secs` / `--max-open-transactions`, else
/// `RELATE_TX_IDLE_TIMEOUT_SECS` / `RELATE_MAX_OPEN_TRANSACTIONS`,
/// else the ADR-0435 defaults (60 s and 128). Resolution is
/// [`relate_serve::resolve_flag_or_env`]'s shared ladder exactly: each
/// flag wins over its variable, a blank variable value is omitted (the 60 s
/// / 128 default, never the off spelling), and a malformed value is a loud
/// startup error naming its source. `0` turns the corresponding protection
/// off; [`TxLimits::new`] converts it into the model's `None`.
///
/// The resolved limits are built through [`TxLimits::new`], so an idle
/// timeout the reply cannot render as a deadline fails this startup step
/// loudly (naming both the flag and the variable) instead of failing a
/// request later.
pub fn resolve_tx_limits(
    idle_timeout_flag: Option<u64>,
    max_open_flag: Option<u64>,
) -> Result<TxLimits, String> {
    let idle_secs = resolve_flag_or_env(
        idle_timeout_flag,
        "RELATE_TX_IDLE_TIMEOUT_SECS",
        DEFAULT_TX_IDLE_TIMEOUT_SECS,
    )?;
    let max_open = resolve_flag_or_env(
        max_open_flag,
        "RELATE_MAX_OPEN_TRANSACTIONS",
        DEFAULT_MAX_OPEN_TRANSACTIONS as u64,
    )?;
    let max_open = usize::try_from(max_open)
        .map_err(|_| "max-open-transactions does not fit this platform".to_string())?;
    TxLimits::new(Duration::from_secs(idle_secs), max_open)
}

/// The ADR-0435 §4 startup warning, the managed-transaction sibling of
/// [`relate_serve::unlimited_cap_warning`]: the registry is wholly
/// unguarded only when BOTH protections are off — idle expiry disabled
/// (`--tx-idle-timeout-secs 0`) AND the cap unlimited
/// (`--max-open-transactions 0`) — so an abandoned transaction neither
/// expires nor is refused and holds a snapshot pin until restart. On a
/// non-loopback bind that warns loudly, naming both flags; either
/// protection alone, or a loopback bind, stays silent. `None` is the
/// silent case; the typed model's two `None` arms are the "off" fact read
/// here, never a re-derived zero.
///
/// `bind_description` is the same caller-owned rendering
/// [`relate_serve::unlimited_cap_warning`] takes (the CLI's
/// `--host HOST`), so both warnings compose into one banner from the same
/// bind verdict.
pub fn unguarded_tx_registry_warning(
    limits: &TxLimits,
    bind_loopback: bool,
    bind_description: &str,
) -> Option<String> {
    if limits.idle_timeout().is_some() || limits.max_open().is_some() || bind_loopback {
        return None;
    }
    Some(format!(
        "WARNING: --tx-idle-timeout-secs and --max-open-transactions are both 0 on \
         non-loopback {bind_description}. An abandoned transaction never expires and \
         the registry is unbounded, so it holds a snapshot pin until restart. \
         Set one of the two to bound it."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    use relate_test_support::EnvRestore;

    #[test]
    fn the_constructor_rejects_the_u64_ceiling_embedder_path() {
        // The embedder path: `TxRegistry::with_limits` takes a `TxLimits`,
        // so the only way in is this constructor. The ceiling is refused —
        // the reply's `expires` rendering never has to omit the field.
        let err = TxLimits::new(Duration::from_secs(u64::MAX), 1).unwrap_err();
        assert!(err.contains("--tx-idle-timeout-secs"), "{err}");
        assert!(err.contains("RELATE_TX_IDLE_TIMEOUT_SECS"), "{err}");
        assert!(
            err.contains(&u64::MAX.to_string()),
            "the error names the value: {err}"
        );
        // Zero is not a ceiling: it is the valid "expiry disabled" wire
        // spelling, converted into the model's `None`.
        assert_eq!(
            TxLimits::new(Duration::ZERO, 0).unwrap(),
            TxLimits {
                idle_timeout: None,
                max_open: None,
            }
        );
        // A representable timeout passes the same step.
        assert_eq!(
            TxLimits::new(Duration::from_secs(7), 9).unwrap(),
            TxLimits {
                idle_timeout: Some(Duration::from_secs(7)),
                max_open: Some(9),
            }
        );
    }

    #[test]
    fn the_defaults_are_the_adr_0435_values_through_the_validator() {
        assert_eq!(
            TxLimits::default(),
            TxLimits {
                idle_timeout: Some(Duration::from_secs(60)),
                max_open: Some(128),
            }
        );
        assert_eq!(
            TxLimits::default(),
            TxLimits::new(Duration::from_secs(60), 128).unwrap()
        );
    }

    #[test]
    fn the_typed_getters_carry_the_off_states() {
        // The model's two off states come from the wire's `0` at the
        // constructor boundary; the getters expose them as `None`, so no
        // consumer re-derives a zero sentinel.
        let off = TxLimits::new(Duration::ZERO, 0).unwrap();
        assert_eq!(off.idle_timeout(), None);
        assert_eq!(off.max_open(), None);
        let on = TxLimits::new(Duration::from_secs(5), 3).unwrap();
        assert_eq!(on.idle_timeout(), Some(Duration::from_secs(5)));
        assert_eq!(on.max_open(), Some(3));
    }

    #[test]
    fn the_render_bound_is_anchored_to_the_render_clock_not_the_epoch() {
        // ADR-0435 §1: the bound is the render's own arithmetic —
        // `Utc::now() + idle_timeout + RENDER_MARGIN` must be representable
        // — so it shrinks with the process's uptime instead of staying fixed
        // at the epoch-to-`MAX_UTC` span. Pin the now-anchored ceiling: a
        // value one second inside it is accepted AND renders, one second
        // past it is refused.
        let now = chrono::Utc::now();
        let span = chrono::DateTime::<chrono::Utc>::MAX_UTC
            .signed_duration_since(now)
            .to_std()
            .expect("the now-to-max span is positive");
        let ceiling = span - RENDER_MARGIN;
        let accepted = ceiling - Duration::from_secs(1);
        assert!(
            TxLimits::new(accepted, 1).is_ok(),
            "one second inside the now-anchored ceiling is accepted"
        );
        assert!(
            render_deadline(accepted + RENDER_MARGIN).is_some(),
            "the accepted constructor value renders within its margin"
        );
        assert!(
            TxLimits::new(ceiling + Duration::from_secs(1), 1).is_err(),
            "one second past the now-anchored ceiling is refused"
        );
        // The old epoch anchor would have accepted this: it is the whole
        // epoch-to-MAX_UTC span, which no `Utc::now()`-relative deadline can
        // reach — so the refusal above is the render clock's, not a constant.
        let epoch_ceiling = chrono::DateTime::<chrono::Utc>::MAX_UTC
            .signed_duration_since(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH)
            .to_std()
            .expect("the epoch-to-max span is positive")
            - RENDER_MARGIN;
        assert!(
            TxLimits::new(epoch_ceiling, 1).is_err(),
            "the epoch-anchored ceiling is unreachable from the render clock"
        );
    }

    #[test]
    fn resolve_tx_limits_flag_then_env_then_defaults() {
        // The flag path is pure: each flag wins over everything, and `0`
        // is the explicit off spelling (ADR-0435 §4). The env vars are
        // set to different values first, so the flag assertions pin the
        // precedence explicitly rather than passing on ambient defaults.
        let _idle_restore = EnvRestore::new("RELATE_TX_IDLE_TIMEOUT_SECS");
        let _max_restore = EnvRestore::new("RELATE_MAX_OPEN_TRANSACTIONS");
        std::env::set_var("RELATE_TX_IDLE_TIMEOUT_SECS", "61");
        std::env::set_var("RELATE_MAX_OPEN_TRANSACTIONS", "129");
        assert_eq!(
            resolve_tx_limits(Some(7), Some(9)).unwrap(),
            TxLimits::new(Duration::from_secs(7), 9).unwrap()
        );
        assert_eq!(
            resolve_tx_limits(Some(0), Some(0)).unwrap(),
            TxLimits::new(Duration::ZERO, 0).unwrap()
        );
        // The env path reads the process environment, shared by parallel
        // tests — the EnvRestore guards above restore both variables on
        // drop, panic path included. One test owns both variables so no
        // two tests race on them.
        std::env::remove_var("RELATE_TX_IDLE_TIMEOUT_SECS");
        std::env::remove_var("RELATE_MAX_OPEN_TRANSACTIONS");
        // Both unset: the ADR-0435 defaults, not the connection cap's
        // unlimited zero.
        assert_eq!(resolve_tx_limits(None, None).unwrap(), TxLimits::default());
        // Env only.
        std::env::set_var("RELATE_TX_IDLE_TIMEOUT_SECS", " 15 ");
        std::env::set_var("RELATE_MAX_OPEN_TRANSACTIONS", "7");
        assert_eq!(
            resolve_tx_limits(None, None).unwrap(),
            TxLimits::new(Duration::from_secs(15), 7).unwrap()
        );
        // A blank environment value is omitted: the default, never zero.
        std::env::set_var("RELATE_TX_IDLE_TIMEOUT_SECS", "  ");
        std::env::set_var("RELATE_MAX_OPEN_TRANSACTIONS", "");
        assert_eq!(resolve_tx_limits(None, None).unwrap(), TxLimits::default());
        // A malformed value is a loud error naming the variable, never a
        // silent default.
        std::env::set_var("RELATE_TX_IDLE_TIMEOUT_SECS", "wat");
        let err = resolve_tx_limits(None, None).unwrap_err();
        assert!(err.contains("RELATE_TX_IDLE_TIMEOUT_SECS"), "{err}");
        std::env::remove_var("RELATE_TX_IDLE_TIMEOUT_SECS");
        std::env::set_var("RELATE_MAX_OPEN_TRANSACTIONS", "wat");
        let err = resolve_tx_limits(None, None).unwrap_err();
        assert!(err.contains("RELATE_MAX_OPEN_TRANSACTIONS"), "{err}");

        // An idle timeout chrono cannot render as a deadline is a loud
        // startup error naming both spellings, never a panic or an omitted
        // `expires` on the request path — through the flag path AND the
        // environment path, since both reach the same validating
        // constructor.
        let err = resolve_tx_limits(Some(u64::MAX), Some(1)).unwrap_err();
        assert!(err.contains("--tx-idle-timeout-secs"), "{err}");
        assert!(err.contains("RELATE_TX_IDLE_TIMEOUT_SECS"), "{err}");
        std::env::set_var("RELATE_TX_IDLE_TIMEOUT_SECS", u64::MAX.to_string());
        let err = resolve_tx_limits(None, Some(1)).unwrap_err();
        assert!(err.contains("RELATE_TX_IDLE_TIMEOUT_SECS"), "{err}");
        std::env::remove_var("RELATE_TX_IDLE_TIMEOUT_SECS");
        // A representable timeout still resolves through the same step.
        assert_eq!(
            resolve_tx_limits(Some(7), Some(1)).unwrap(),
            TxLimits::new(Duration::from_secs(7), 1).unwrap()
        );
    }

    #[test]
    fn unguarded_tx_registry_warning_needs_both_protections_off_on_non_loopback() {
        // The ADR-0435 §4 posture: only a registry with BOTH protections
        // off on a non-loopback bind warns, and the line names both flags.
        let unguarded = TxLimits::new(Duration::ZERO, 0).unwrap();
        assert_eq!(
            unguarded_tx_registry_warning(&unguarded, false, "--host 0.0.0.0"),
            Some(
                "WARNING: --tx-idle-timeout-secs and --max-open-transactions are both 0 on \
                 non-loopback --host 0.0.0.0. An abandoned transaction never expires and \
                 the registry is unbounded, so it holds a snapshot pin until restart. \
                 Set one of the two to bound it."
                    .to_string()
            )
        );
        // Either protection alone stays silent.
        let cap_only = TxLimits::new(Duration::ZERO, DEFAULT_MAX_OPEN_TRANSACTIONS).unwrap();
        assert_eq!(
            unguarded_tx_registry_warning(&cap_only, false, "--host 0.0.0.0"),
            None
        );
        let idle_only = TxLimits::new(Duration::from_secs(60), 0).unwrap();
        assert_eq!(
            unguarded_tx_registry_warning(&idle_only, false, "--host 0.0.0.0"),
            None
        );
        // A loopback-only bind stays silent even when unguarded.
        assert_eq!(
            unguarded_tx_registry_warning(&unguarded, true, "--host 127.0.0.1"),
            None
        );
    }
}
