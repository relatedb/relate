//! The one take-and-reset counter the graph's test seams share: a
//! per-thread event count that a test (or the facade's `test-hooks`
//! reopen tests) reads and zeroes in one step. Each seam keeps only its own
//! `thread_local!` static and its cfg gate; the count's semantics live here.

use std::cell::Cell;

/// A per-thread event counter, read-and-reset by [`BuildCounter::take`].
pub(super) struct BuildCounter(Cell<u64>);

impl BuildCounter {
    /// A zeroed counter, usable as a `thread_local!` const initializer.
    pub(super) const fn new() -> Self {
        Self(Cell::new(0))
    }

    /// Count one event.
    pub(super) fn bump(&self) {
        self.0.set(self.0.get() + 1);
    }

    /// The count so far, resetting it to zero.
    pub(super) fn take(&self) -> u64 {
        self.0.replace(0)
    }
}
