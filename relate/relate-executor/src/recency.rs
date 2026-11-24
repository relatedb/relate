//! The repo's one bounded-recency table (the `PatternCache` spelling,
//! extracted so every LRU-bounded memo reads the same policy): the back
//! holds the recently used slot, the front is the eviction victim. A hit
//! refreshes its slot to the back; an insert into a full table first
//! evicts the front — recency and eviction live here, never at a call
//! site. The slot type is the caller's (`RecencySlots<T>` is generic
//! over it): the match predicate and any eviction side effect (a memo's
//! charged-bytes refund) stay at the two call sites that own them.

/// A bounded-recency table of caller-owned slots. The capacity is
/// [`NonZeroUsize`] — both production constructors pass compile-time
/// non-zero caps, so "an insert always has room after an eviction" is
/// the type's own invariant, never a guarded branch.
pub(crate) struct RecencySlots<T> {
    slots: Vec<T>,
    capacity: std::num::NonZeroUsize,
}

impl<T> RecencySlots<T> {
    /// An empty table with room for `capacity` slots. `const` so a
    /// thread-local's initializer block can build one.
    pub(crate) const fn new(capacity: std::num::NonZeroUsize) -> Self {
        Self {
            slots: Vec::new(),
            capacity,
        }
    }

    /// The first slot matching `matches`, refreshed to the back — the
    /// one recency update; `None` when the table holds no such slot.
    /// The slot rides back shared: both callers read it (a corpus
    /// clone, an entry reference), never mutate through it.
    pub(crate) fn hit(&mut self, matches: impl FnMut(&T) -> bool) -> Option<&T> {
        let pos = self.slots.iter().position(matches)?;
        let slot = self.slots.remove(pos);
        self.slots.push(slot);
        self.slots.last()
    }

    /// Append `slot`, first evicting the front when the table is full —
    /// the evicted slot rides back to the caller, whose refund or
    /// teardown it is; `None` when the table had room.
    pub(crate) fn insert(&mut self, slot: T) -> Option<T> {
        let evicted = (self.slots.len() == self.capacity.get()).then(|| self.slots.remove(0));
        self.slots.push(slot);
        evicted
    }

    /// The live slot count — the capacity pins the caches' own tests
    /// read; never a production path.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.slots.len()
    }

    /// The live slots, front (eviction victim) first — the introspection
    /// the caches' own tests read; never a production path.
    #[cfg(test)]
    pub(crate) fn iter(&self) -> std::slice::Iter<'_, T> {
        self.slots.iter()
    }
}
