//! The one shutdown-signal policy every Relate network surface (and the
//! CLI's `serve`) blocks on: signal-hook's blocking iterator on unix
//! (SIGINT and SIGTERM, ADR-0233); a handler-signalled channel receive
//! elsewhere, because signal-hook's `iterator` module is `not(windows)`
//! -gated and Windows consoles deliver only Ctrl-C (SIGINT). The
//! caller's main thread owns the shutdown, so teardown (a final
//! checkpoint, child reaping) runs off the signal context.
//!
//! Install and wait are separate steps (REL8-1884): a server that must
//! be watchable from its first bound port arms the watcher with
//! [`install`] before it binds anything, then blocks in
//! [`ShutdownSignals::wait`] once its surfaces are dispatched — a
//! signal landing in between is captured and delivered at the wait,
//! never handled by the default disposition mid-startup.
//! [`wait_or_exit`] is the one-step form for callers that arm at the
//! moment they block.

/// The armed shutdown watcher: the signal source is installed, and a
/// signal that lands is captured until [`Self::wait`] consumes it.
#[cfg(unix)]
pub struct ShutdownSignals {
    signals: signal_hook::iterator::Signals,
}

/// Arm the shutdown watcher — SIGINT and SIGTERM are captured from here
/// on. The error is the handler-installation failure.
#[cfg(unix)]
pub fn install() -> std::io::Result<ShutdownSignals> {
    use signal_hook::consts::{SIGINT, SIGTERM};
    Ok(ShutdownSignals {
        signals: signal_hook::iterator::Signals::new([SIGINT, SIGTERM])?,
    })
}

/// The armed shutdown watcher, the Windows-console form. signal-hook's
/// blocking `iterator` module is `not(windows)`-gated, so the watcher is
/// a flag the registered action sets and [`Self::wait`] blocks on the
/// condvar it notifies — no polling, no lost wakeup.
#[cfg(not(unix))]
pub struct ShutdownSignals {
    hit: std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
}

/// Arm the shutdown watcher — the Windows console's Ctrl-C (SIGINT). The
/// error is the handler-installation failure.
#[cfg(not(unix))]
pub fn install() -> std::io::Result<ShutdownSignals> {
    use std::sync::{Arc, Condvar, Mutex};

    let hit = Arc::new((Mutex::new(false), Condvar::new()));
    let notified = Arc::clone(&hit);
    // SAFETY: Windows runs the console-control handler on its own thread
    // (the CRT's `signal` machinery), so locking a mutex and notifying a
    // condvar inside the action is safe there; this variant never
    // compiles on unix, where the action would run inside the async
    // signal handler. The action never panics (a poisoned lock is simply
    // not taken).
    unsafe {
        signal_hook::low_level::register(signal_hook::consts::SIGINT, move || {
            let (lock, cvar) = &*notified;
            if let Ok(mut hit) = lock.lock() {
                *hit = true;
                cvar.notify_all();
            }
        })?;
    }
    Ok(ShutdownSignals { hit })
}

impl ShutdownSignals {
    /// Block until a shutdown signal arrives and return the raw signal
    /// number — for the caller that maps it to its exit code (ADR-0264,
    /// the fleet entrypoint's 130/143). A caller with its own exit
    /// contract consumes the wait as a statement and ignores the number
    /// (the CLI's `serve` exits 0 after its final checkpoint).
    #[cfg(unix)]
    pub fn wait(&mut self) -> i32 {
        self.signals.forever().next().expect("signal stream ended")
    }

    /// See the unix variant: block until the registered action flags the
    /// signal, then report it as SIGINT (the only Windows-console
    /// shutdown signal).
    #[cfg(not(unix))]
    pub fn wait(&mut self) -> i32 {
        let (lock, cvar) = &*self.hit;
        let mut hit = lock.lock().expect("shutdown flag mutex poisoned");
        while !*hit {
            hit = cvar.wait(hit).expect("shutdown flag mutex poisoned");
        }
        signal_hook::consts::SIGINT
    }
}

/// The one-step wait-or-exit policy for callers that arm at the moment
/// they block: install, then wait; a handler-installation failure is
/// reported with `prefix` and exits 1 — a server that cannot watch for
/// its shutdown signal must not run unwatched. One home, so the fleet
/// entrypoint cannot drift in wording or policy. A caller that must be
/// watchable from before its first bound port arms earlier with
/// [`install`] instead (the CLI's `serve`, REL8-1884).
pub fn wait_or_exit(prefix: &str) -> i32 {
    match install() {
        Ok(mut signals) => signals.wait(),
        Err(e) => {
            eprintln!("{prefix}: could not install the signal handlers: {e}");
            std::process::exit(1);
        }
    }
}
