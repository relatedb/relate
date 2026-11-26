//! The statement deadline (ADR-0692): a wall-clock budget enforced through
//! the [`CancelToken`] (ADR-0111).
//!
//! One shared timer thread owns every armed deadline, so arming a statement
//! is a lock and a map insert rather than a thread spawn, and a statement
//! that finishes first disarms by dropping its [`Armed`] guard. The timer
//! cancels with a cause ([`CancelToken::timed_out`]), which the engine
//! turns into [`crate::Error::TimedOut`].

use std::time::Duration;

use crate::CancelToken;

/// The default statement budget of the network surfaces, in seconds
/// (ADR-0692 §2): Bolt, HTTP and MCP single-source it here. `0` disables.
pub const DEFAULT_STATEMENT_TIMEOUT_SECS: u64 = 30;

/// A limit as the error text spells it: whole seconds when exact,
/// milliseconds otherwise (`30s`, `1500ms`).
pub fn format_limit(limit_ms: u64) -> String {
    if limit_ms.is_multiple_of(1000) {
        format!("{}s", limit_ms / 1000)
    } else {
        format!("{limit_ms}ms")
    }
}

/// The resolved budget of one statement: how long, and whether the request
/// (not the server default) chose it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limit {
    /// The wall-clock budget.
    pub budget: Duration,
    /// True when the request set it.
    pub requested: bool,
}

/// The server-side policy (ADR-0692 §3): a default used when a request
/// carries none, and an optional ceiling no request or default exceeds.
/// `None` is "no bound".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TimeoutPolicy {
    /// Applied when the request names no value.
    pub default: Option<Duration>,
    /// The operator's hard bound over every value, including a request's
    /// `0`/"no bound".
    pub ceiling: Option<Duration>,
}

impl TimeoutPolicy {
    /// The policy from the two operator flags, in whole seconds, each `0`
    /// meaning none (the "0 disables" convention of the surface flags).
    pub fn from_secs(default_secs: u64, ceiling_secs: u64) -> Self {
        let secs = |n: u64| (n != 0).then(|| Duration::from_secs(n));
        Self {
            default: secs(default_secs),
            ceiling: secs(ceiling_secs),
        }
    }

    /// The served policy: [`DEFAULT_STATEMENT_TIMEOUT_SECS`], no ceiling.
    pub const SERVED: Self = Self {
        default: Some(Duration::from_secs(DEFAULT_STATEMENT_TIMEOUT_SECS)),
        ceiling: None,
    };

    /// The effective limit for a request: `request` is `Some(zero)` for a
    /// client that asks for no bound, `None` when it names nothing.
    pub fn resolve(&self, request: Option<Duration>) -> Option<Limit> {
        let (wanted, requested) = match request {
            Some(d) if d.is_zero() => (None, true),
            Some(d) => (Some(d), true),
            None => (self.default, false),
        };
        match (wanted, self.ceiling) {
            (Some(w), Some(c)) if c < w => Some(Limit {
                budget: c,
                requested: false,
            }),
            (Some(w), _) => Some(Limit {
                budget: w,
                requested,
            }),
            (None, Some(c)) => Some(Limit {
                budget: c,
                requested: false,
            }),
            (None, None) => None,
        }
    }
}

/// Disarms its deadline on drop; arm one with [`arm`]. Dropping it at once
/// disarms at once, so the guard must be held for the statement.
#[must_use = "dropping the guard disarms the deadline; hold it for the statement"]
#[derive(Debug)]
pub struct Armed {
    #[cfg(not(target_arch = "wasm32"))]
    key: Option<imp::Key>,
}

impl Drop for Armed {
    fn drop(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(key) = self.key.take() {
            imp::disarm(key);
        }
    }
}

/// Arm `limit` over `token`: after the budget the token is cancelled with
/// the timeout cause, unless the guard is dropped first. A `None` limit,
/// or a target without threads (wasm), arms nothing.
pub fn arm(token: &CancelToken, limit: Option<Duration>) -> Armed {
    #[cfg(not(target_arch = "wasm32"))]
    {
        Armed {
            key: limit.and_then(|d| imp::arm(token.clone(), d)),
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (token, limit);
        Armed {}
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use std::collections::BTreeMap;
    use std::sync::{Condvar, Mutex, OnceLock};
    use std::time::{Duration, Instant};

    use crate::CancelToken;

    pub(super) type Key = (Instant, u64);

    #[derive(Default)]
    struct State {
        next_id: u64,
        entries: BTreeMap<Key, CancelToken>,
    }

    struct Timer {
        state: Mutex<State>,
        wake: Condvar,
    }

    fn timer() -> &'static Timer {
        static TIMER: OnceLock<&'static Timer> = OnceLock::new();
        TIMER.get_or_init(|| {
            let timer: &'static Timer = Box::leak(Box::new(Timer {
                state: Mutex::new(State::default()),
                wake: Condvar::new(),
            }));
            std::thread::Builder::new()
                .name("relate-deadline".into())
                .spawn(move || run(timer))
                .expect("the statement deadline timer thread must start");
            timer
        })
    }

    fn lock(timer: &Timer) -> std::sync::MutexGuard<'_, State> {
        // The state is a plain map whose every mutation is one insert or
        // remove, so a poisoned lock holds a consistent map.
        timer
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// `None` when `limit` is past `Instant`'s range: such a budget can
    /// never fire, so nothing is armed.
    pub(super) fn arm(token: CancelToken, limit: Duration) -> Option<Key> {
        let timer = timer();
        let due = Instant::now().checked_add(limit)?;
        let mut state = lock(timer);
        state.next_id += 1;
        let key = (due, state.next_id);
        let earliest = state.entries.keys().next().is_none_or(|first| key < *first);
        state.entries.insert(key, token);
        drop(state);
        if earliest {
            timer.wake.notify_one();
        }
        Some(key)
    }

    pub(super) fn disarm(key: Key) {
        lock(timer()).entries.remove(&key);
    }

    fn run(timer: &'static Timer) {
        let mut state = lock(timer);
        loop {
            let now = Instant::now();
            match state.entries.keys().next().copied() {
                None => {
                    state = timer
                        .wake
                        .wait(state)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
                Some(key) if key.0 <= now => {
                    if let Some(token) = state.entries.remove(&key) {
                        token.cancel_timed_out();
                    }
                }
                Some(key) => {
                    state = timer
                        .wake
                        .wait_timeout(state, key.0 - now)
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .0;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECS: fn(u64) -> Duration = Duration::from_secs;

    #[test]
    fn a_request_overrides_the_default_in_both_directions() {
        let policy = TimeoutPolicy {
            default: Some(SECS(30)),
            ceiling: None,
        };
        let shorter = policy.resolve(Some(SECS(5))).unwrap();
        assert_eq!((shorter.budget, shorter.requested), (SECS(5), true));
        let longer = policy.resolve(Some(SECS(600))).unwrap();
        assert_eq!((longer.budget, longer.requested), (SECS(600), true));
        let none = policy.resolve(None).unwrap();
        assert_eq!((none.budget, none.requested), (SECS(30), false));
        assert_eq!(policy.resolve(Some(SECS(0))), None, "0 asks for no bound");
    }

    #[test]
    fn the_ceiling_bounds_every_value_including_no_bound() {
        let policy = TimeoutPolicy {
            default: Some(SECS(30)),
            ceiling: Some(SECS(60)),
        };
        assert_eq!(policy.resolve(Some(SECS(600))).unwrap().budget, SECS(60));
        assert_eq!(policy.resolve(Some(SECS(0))).unwrap().budget, SECS(60));
        assert_eq!(policy.resolve(Some(SECS(10))).unwrap().budget, SECS(10));
        let off = TimeoutPolicy::default();
        assert_eq!(off.resolve(None), None);
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn an_armed_deadline_cancels_with_the_timeout_cause() {
        let token = CancelToken::new();
        let _armed = arm(&token, Some(Duration::from_millis(20)));
        let start = std::time::Instant::now();
        while !token.is_cancelled() {
            assert!(start.elapsed() < SECS(10), "the deadline never fired");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(token.timed_out());
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn a_disarmed_deadline_never_cancels() {
        let token = CancelToken::new();
        drop(arm(&token, Some(Duration::from_millis(30))));
        std::thread::sleep(Duration::from_millis(120));
        assert!(!token.is_cancelled());
    }

    #[test]
    fn limits_format_as_seconds_when_exact() {
        assert_eq!(format_limit(30_000), "30s");
        assert_eq!(format_limit(1_500), "1500ms");
    }
}
