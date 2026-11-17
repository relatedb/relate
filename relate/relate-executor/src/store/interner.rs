//! The append-only string interner behind label, relationship-type and
//! property-key ids (ADR-0033), and its plan-time [`TokenResolver`] seam
//! (ADR-0541).

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;

#[derive(Debug, Default)]
pub(crate) struct Interner {
    to_id: DashMap<Arc<str>, u32>,
    /// Dense, append-only id→string table. Entries are never removed or
    /// reassigned, so a read for a committed id is a plain shard lookup; only
    /// `intern` mutates (on first sight of a string).
    to_str: DashMap<u32, Arc<str>>,
    /// Monotonic id source. Separate from `to_str.len()` so two concurrent
    /// first-time interns of distinct strings cannot observe the same len.
    next_id: AtomicU32,
    /// The interner's whole retained bytes: distinct interned strings
    /// (`Arc` clones are not added again) plus the label-vec cache's own
    /// copies. One counter, so the store's enforced logical-size account
    /// sees the cache too.
    bytes: AtomicU64,
    /// One label vector per distinct label-id list. Walk stubs clone the
    /// `Arc` instead of rebuilding the strings.
    label_vecs: DashMap<Vec<u32>, Arc<Vec<String>>>,
}

#[cfg(test)]
thread_local! {
    /// `Interner::get_id` calls on this thread — test-only, read through
    /// [`take_get_id_calls`].
    static GET_ID_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Returns and resets this thread's `Interner::get_id` call count.
#[cfg(test)]
pub(crate) fn take_get_id_calls() -> u64 {
    GET_ID_CALLS.with(|c| c.replace(0))
}

impl relate_names::TokenResolver for Interner {
    fn resolve(&self, name: &str) -> relate_names::Token {
        match self.get_id(name) {
            Some(id) => relate_names::Token::Id(relate_names::TokenId(id)),
            None => relate_names::Token::Absent,
        }
    }
}

impl Interner {
    pub(crate) fn intern(&self, s: &str) -> u32 {
        if let Some(id) = self.to_id.get(s) {
            return *id;
        }
        // `entry` holds the shard write lock, so a concurrent `intern` of the
        // same string takes the occupied arm and both callers get one id.
        match self.to_id.entry(Arc::from(s)) {
            dashmap::mapref::entry::Entry::Occupied(e) => *e.get(),
            dashmap::mapref::entry::Entry::Vacant(e) => {
                let arc = e.key().clone();
                let id = self.next_id.fetch_add(1, Ordering::Relaxed);
                // Publish id→str before the id is reachable via `to_id`, so a
                // `get(id)` that observes the id always finds the string.
                self.to_str.insert(id, arc);
                e.insert(id);
                self.bytes.fetch_add(
                    crate::memory::interned_string_bytes(s.len()),
                    Ordering::Relaxed,
                );
                id
            }
        }
    }

    pub(crate) fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }

    /// Distinct interned strings — the census harness's key count.
    #[cfg(feature = "research")]
    pub(crate) fn key_count(&self) -> usize {
        self.to_id.len()
    }

    pub(crate) fn get_id(&self, s: &str) -> Option<u32> {
        #[cfg(test)]
        GET_ID_CALLS.with(|c| c.set(c.get() + 1));
        self.to_id.get(s).map(|v| *v)
    }

    /// Reverse lookup of an interned id. Every reachable id was published
    /// by [`Interner::intern`] before use, so a miss is a bug, not data:
    /// panic instead of silently returning the empty string.
    pub(crate) fn get(&self, id: u32) -> Arc<str> {
        self.to_str
            .get(&id)
            .map(|v| Arc::clone(&*v))
            .expect("interned id is published before use")
    }

    /// The label strings for `ids`, in id order — the interner's one
    /// id→text conversion, which the stub cache and full materialization
    /// both read.
    pub(crate) fn label_strings(&self, ids: &[u32]) -> Vec<String> {
        ids.iter().map(|id| String::from(&*self.get(*id))).collect()
    }

    /// The label strings for `ids`, shared with every other stub of the
    /// same label list.
    pub(crate) fn cached_labels(&self, ids: &[u32]) -> Arc<Vec<String>> {
        if let Some(hit) = self.label_vecs.get(ids) {
            return Arc::clone(hit.value());
        }
        match self.label_vecs.entry(ids.to_vec()) {
            // A racer that lost the entry decision never builds the vector:
            // the losing path only clones the winner's `Arc`.
            dashmap::mapref::entry::Entry::Occupied(entry) => Arc::clone(entry.get()),
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                let labels = Arc::new(self.label_strings(ids));
                let string_bytes = labels.iter().map(|label| label.len() as u64).sum();
                self.bytes.fetch_add(
                    crate::memory::label_vec_cache_bytes(ids.len(), string_bytes),
                    Ordering::Relaxed,
                );
                entry.insert(Arc::clone(&labels));
                labels
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_roundtrip() {
        let i = Interner::default();
        let a = i.intern("TRANSFER");
        let b = i.intern("TRANSFER");
        assert_eq!(a, b);
        assert_eq!(&*i.get(a), "TRANSFER");
        assert_eq!(i.get_id("OTHER"), None);
    }

    #[test]
    fn resolver_matches_interner_ids() {
        use relate_names::{Token, TokenId, TokenResolver};
        let i = Interner::default();
        // label, relationship type, property key share the interner's id space
        for name in ["Person", "KNOWS", "since"] {
            let id = i.intern(name);
            assert_eq!(i.resolve(name), Token::Id(TokenId(id)));
        }
        for name in ["Absent", "MISSING", "nokey"] {
            assert_eq!(i.resolve(name), Token::Absent);
        }
    }

    #[test]
    #[should_panic(expected = "interned id is published before use")]
    fn get_unknown_id_panics() {
        let i = Interner::default();
        let _ = i.get(0);
    }

    /// The id table is append-only and concurrent `intern` calls race to mint
    /// ids. Every distinct string must get exactly one id, repeated interns of
    /// the same string must agree, and no id may ever resolve to the wrong
    /// string (which the old `len()`-based counter could cause).
    #[test]
    fn intern_concurrent_ids_are_stable_and_unique() {
        use std::collections::{HashMap, HashSet};
        use std::sync::Arc as StdArc;

        let interner = StdArc::new(Interner::default());
        let words: Vec<String> = (0..64).map(|n| format!("label-{n}")).collect();

        let handles: Vec<_> = (0..8)
            .map(|t| {
                let interner = StdArc::clone(&interner);
                let words = words.clone();
                std::thread::spawn(move || {
                    let mut seen = HashMap::new();
                    for round in 0..32 {
                        for n in 0..words.len() {
                            // Rotate start so threads interleave differently.
                            let w = &words[(n + t + round) % words.len()];
                            let id = interner.intern(w);
                            // Idempotent: the same string always yields the
                            // same id, even under concurrent first-sight.
                            if let Some(prev) = seen.insert(w.clone(), id) {
                                assert_eq!(prev, id, "id changed for {w}");
                            }
                            // The reverse direction must agree immediately.
                            assert_eq!(&*interner.get(id), w.as_str());
                        }
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        // Exactly one id per distinct string, and the id space is dense from 0.
        let ids: HashSet<u32> = words.iter().map(|w| interner.intern(w)).collect();
        assert_eq!(ids.len(), words.len(), "duplicate ids for distinct strings");
        assert_eq!(*ids.iter().min().unwrap(), 0);
        assert_eq!(*ids.iter().max().unwrap(), (words.len() - 1) as u32);
    }
}
