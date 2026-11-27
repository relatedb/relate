//! The one take-and-reset counter home the graph's test seams share: a
//! per-thread event count for seams whose bump and read share a thread,
//! and a process-wide name-keyed count for seams whose events fire on a
//! thread the reading test does not run on — the facade answers queries
//! on a spawned `QueryWorker` thread, so a `thread_local!` count cannot
//! cross that boundary. The one decision rule: same thread →
//! [`BuildCounter`] in a `thread_local!`; events on another thread →
//! [`SharedCounter`] in a plain `static`. Each seam keeps only its own
//! static and its cfg gate; the counts' semantics live here.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::Mutex;

/// A per-thread event counter, read-and-reset by [`BuildCounter::take`].
pub(crate) struct BuildCounter(Cell<u64>);

impl BuildCounter {
    /// A zeroed counter, usable as a `thread_local!` const initializer.
    pub(crate) const fn new() -> Self {
        Self(Cell::new(0))
    }

    /// Count one event.
    pub(crate) fn bump(&self) {
        self.0.set(self.0.get() + 1);
    }

    /// The count so far, resetting it to zero.
    pub(crate) fn take(&self) -> u64 {
        self.0.replace(0)
    }
}

/// A process-wide, name-keyed event counter for seams whose events fire
/// on a thread the reading test does not run on — the facade runs queries
/// on a spawned `QueryWorker` thread, so a `thread_local!` count cannot
/// cross that boundary (ADR-0705's vector-matrix serve count). One static
/// per seam; the name keeps concurrently running tests' counts separate.
/// Read-and-reset by [`SharedCounter::take`], like [`BuildCounter`].
pub(crate) struct SharedCounter(Mutex<BTreeMap<String, u64>>);

impl SharedCounter {
    /// An empty counter, usable as a plain `static` initializer.
    pub(crate) const fn new() -> Self {
        Self(Mutex::new(BTreeMap::new()))
    }

    /// Count one event under `name`.
    pub(crate) fn bump(&self, name: &str) {
        *self
            .0
            .lock()
            .expect("shared test counter lock poisoned")
            .entry(name.to_string())
            .or_insert(0) += 1;
    }

    /// The count under `name` so far, resetting it to zero.
    pub(crate) fn take(&self, name: &str) -> u64 {
        self.0
            .lock()
            .expect("shared test counter lock poisoned")
            .remove(name)
            .unwrap_or(0)
    }
}
