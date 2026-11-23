//! The env seam: the one reentrant lock every environment knob a test
//! binary's store reads once at open — `RELATE_BULK_BATCH_THRESHOLD`,
//! `RELATE_BULK_BATCH_BUDGET`, `RELATE_PATTERN_ENGINE` — is set and
//! consumed under, and the one open helper every test-store open goes
//! through. Callers name this module
//! (`common::env_seam::EnvSeamWindow`); nothing is re-exported from the
//! parent.

use std::cell::Cell;
use std::sync::{Mutex, MutexGuard};

/// The one env-seam lock this test binary takes: every store open and
/// every set_var + open window — [`super::BatchEnv`]'s forced/disabled
/// spans, [`super::open_with_engine`]'s engine pins, every plain
/// [`super::connection`]'s open — holds it, so one test's opens never
/// read a sibling test's half-set seam value (the three
/// read-once-at-open knobs must not cross between the parallel tests
/// of one binary).
///
/// Reentrant by a thread-local held flag, not by a second mutex: a
/// test may hold [`super::BatchEnv`] across an
/// [`super::open_with_engine`] window — ADR-0666's specialized-path
/// duty forces the batched seam and runs both pattern engines in one
/// test — and re-locking a std `Mutex` on the holding thread would
/// deadlock. The outermost window serializes against every other
/// test; a nested one only scopes its own restore.
static ENV_SEAM_LOCK: Mutex<()> = Mutex::new(());

thread_local! {
    static ENV_SEAM_HELD: Cell<bool> = const { Cell::new(false) };
}

/// The outermost window's guard: dropping it clears this thread's
/// held flag — the two were set together, and a panicking nested
/// window unwinds without marking the thread lock-free. The field is
/// private, so the type can be named (the scope's return type) but
/// never constructed from outside this module.
pub struct SeamGuard {
    _guard: MutexGuard<'static, ()>,
}

impl Drop for SeamGuard {
    fn drop(&mut self) {
        ENV_SEAM_HELD.with(|held| held.set(false));
    }
}

/// Enter the seam: the outermost entry on this thread holds the mutex
/// (serializing every other window and open in this binary); a nested
/// entry carries no guard, only its own restore scope.
pub fn env_seam_scope() -> Option<SeamGuard> {
    // Only the outermost entry acquires; the flag is set after, so a
    // blocked acquisition never counts as held on this thread.
    if ENV_SEAM_HELD.with(|held| held.get()) {
        None
    } else {
        let guard = ENV_SEAM_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ENV_SEAM_HELD.with(|held| held.set(true));
        Some(SeamGuard { _guard: guard })
    }
}

/// One seam window: the seam plus the env restores it pins, with the
/// restores running while the seam is still held — an ordering this
/// type's own [`Drop`] performs, never a field- or local-declaration
/// order its users must preserve.
pub struct EnvSeamWindow {
    seam: Option<SeamGuard>,
    restores: Vec<relate_test_support::EnvRestore>,
}

impl EnvSeamWindow {
    /// Pin each `(var, value)` pair for this window's span; the prior
    /// values come back on drop ([`relate_test_support::EnvRestore`],
    /// the one env guard), still under the seam.
    pub fn pin(pins: &[(&'static str, &str)]) -> Self {
        let window = Self {
            seam: env_seam_scope(),
            restores: pins
                .iter()
                .map(|(var, _)| relate_test_support::EnvRestore::new(var))
                .collect(),
        };
        for (var, value) in pins {
            std::env::set_var(var, value);
        }
        window
    }
}

impl Drop for EnvSeamWindow {
    fn drop(&mut self) {
        // Restores first, still under the seam: a sibling window must
        // never read a half-restored seam.
        self.restores.clear();
        self.seam.take();
    }
}

/// The one test-store open path: every `Database` open in these test
/// commons runs inside the seam — nested under any pin window the
/// caller already holds, or acquiring it when the caller holds none —
/// so no open reads a sibling test's half-set knob and no sibling
/// reads this one's. No open site acquires the seam by hand: callers
/// hold their pin window and open through here.
pub fn open(uri: &str, opts: relate::OpenOptions) -> relate::Database {
    let _seam = env_seam_scope();
    relate::Database::open_with(uri, opts).unwrap()
}
