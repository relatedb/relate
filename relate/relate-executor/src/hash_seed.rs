//! The executor's hasher seam: the one place each hashing policy is
//! spelled, so a table picks a policy by name instead of re-spelling it.
//!
//! Two policies. Pick by who can choose the key, never by habit; a table
//! on neither is a default-hasher table (SipHash with a per-table random
//! key) and must have a stated reason.
//!
//! * **Id policy** — [`IdMap`] / [`IdSet`] on [`FxBuildHasher`], the keyless
//!   FxHash, for keys the engine generates or a client cannot choose:
//!   allocated node/rel ids and small tuples of them, schema and `Arc`
//!   allocation addresses (the allocator picks them, ASLR randomises the
//!   base). The `CoreMap` family and the distinct set's node/rel word
//!   tables are the same policy. ADR-0355 bars it from attacker-writable
//!   keys: it is not a keyed function, so keys a client can steer
//!   (property values, strings, map keys, anything decoded from the wire
//!   or the statement text) can be pre-collided offline.
//! * **Seeded policy** — [`SeededSet`] / [`SeededMap`] on [`SeededState`], the one
//!   process-wide keyed SipHash state ([`shared_random_state`], LABS-822),
//!   for every table whose keys a client can influence ([`seeded_set`],
//!   [`seeded_map`]).
//!
//! Iteration order of either policy is unspecified and differs between
//! them: a result's order must never depend on walking one of these
//! tables (sort, or use an ordered container, where order matters).

use std::collections::hash_map::{DefaultHasher, RandomState};
use std::collections::{HashMap, HashSet};
use std::hash::BuildHasher;
use std::sync::OnceLock;

/// The keyless FxHash build-hasher of the id policy — the hasher of the
/// `CoreMap` family and of every other engine-id table. Never for
/// client-controlled keys. This is `rustc_hash`'s own build-hasher, so
/// `rustc_hash::FxHashMap` and [`IdMap`] are one type.
pub(crate) type FxBuildHasher = rustc_hash::FxBuildHasher;

/// An engine-id table (id policy): the `std` map type the shortest-path
/// searches, the frontier's level tables and the row-ownership walks
/// name instead of the dependency. [`crate::graph::CoreMap`] is the
/// concurrent spelling.
pub(crate) type IdMap<K, V> = HashMap<K, V, FxBuildHasher>;

/// An engine-id set (id policy), the set spelling of [`IdMap`].
pub(crate) type IdSet<K> = HashSet<K, FxBuildHasher>;

/// The seeded build-hasher of every table whose keys a client can
/// influence: the process-wide keyed state [`shared_random_state`]
/// clones. A newtype over `RandomState` with **no `Default`**, so a
/// seeded table can only be built from the shared state (a derived
/// `default()` would silently draw a fresh per-table seed and pay the
/// thread-local RNG read the shared state exists to avoid): the seeded
/// policy is a type, not a convention.
#[derive(Clone, Debug)]
pub(crate) struct SeededState(RandomState);

impl BuildHasher for SeededState {
    type Hasher = DefaultHasher;

    fn build_hasher(&self) -> DefaultHasher {
        self.0.build_hasher()
    }
}

/// A set over client-influenced keys (seeded policy). Build it with
/// [`seeded_set`] or [`seeded_set_with_capacity`]; `default()` does not
/// exist for [`SeededState`].
pub(crate) type SeededSet<K> = HashSet<K, SeededState>;

/// A map over client-influenced keys (seeded policy). Build it with
/// [`seeded_map`].
pub(crate) type SeededMap<K, V> = HashMap<K, V, SeededState>;

/// The one process-wide seed behind [`shared_random_state`] (LABS-822).
static SHARED_RANDOM_STATE: OnceLock<RandomState> = OnceLock::new();

/// The process-wide seeded hasher state, cloned per table. A `RandomState`
/// clone copies two seed words and never re-reads the thread-local RNG
/// cell, whose read LABS-822 measured as the classic walker's hot cost
/// when every used-set built a fresh `RandomState`. The seed stays secret
/// for the process lifetime, so sharing it carries the same HashDoS
/// exposure as std's per-thread seed.
pub(crate) fn shared_random_state() -> SeededState {
    SeededState(SHARED_RANDOM_STATE.get_or_init(RandomState::new).clone())
}

/// An empty seeded-policy map on the shared state (allocates nothing).
pub(crate) fn seeded_map<K, V>() -> SeededMap<K, V> {
    SeededMap::with_hasher(shared_random_state())
}

/// An id-policy set presized for `capacity` keys.
pub(crate) fn id_set_with_capacity<K>(capacity: usize) -> IdSet<K> {
    IdSet::with_capacity_and_hasher(capacity, FxBuildHasher::default())
}

/// An empty seeded-policy set on the shared state (allocates nothing).
pub(crate) fn seeded_set<K>() -> SeededSet<K> {
    SeededSet::with_hasher(shared_random_state())
}

/// A seeded-policy set presized for `capacity` keys.
pub(crate) fn seeded_set_with_capacity<K>(capacity: usize) -> SeededSet<K> {
    SeededSet::with_capacity_and_hasher(capacity, shared_random_state())
}

#[cfg(test)]
mod tests {
    use std::hash::BuildHasher;

    use super::*;

    /// Probe keys both policy tests hash: zero, small, a mid pattern, max.
    const PROBE_KEYS: [u64; 4] = [0, 1, 0xdead_beef, u64::MAX];

    #[test]
    fn id_policy_is_keyless_and_two_tables_hash_alike() {
        let a = IdMap::<u64, u8>::default();
        let b = IdMap::<u64, u8>::default();
        for key in PROBE_KEYS {
            assert_eq!(a.hasher().hash_one(key), b.hasher().hash_one(key));
        }
        // The same function as `rustc_hash`'s own spelling: one type, so
        // a map built either way is interchangeable.
        let fx: rustc_hash::FxHashMap<u64, u8> = rustc_hash::FxHashMap::from_iter([(7, 1)]);
        let id: IdMap<u64, u8> = fx;
        assert_eq!(id.get(&7), Some(&1));
        let sized: IdSet<u64> = id_set_with_capacity(64);
        assert!(sized.capacity() >= 64);
        assert_eq!(sized.hasher().hash_one(7u64), a.hasher().hash_one(7u64));
    }

    #[test]
    fn seeded_policy_shares_one_secret_seed_and_is_not_the_id_hash() {
        let a = shared_random_state();
        let b = shared_random_state();
        let id = FxBuildHasher::default();
        for key in PROBE_KEYS {
            // One process-wide seed: every table agrees with every other.
            assert_eq!(a.hash_one(key), b.hash_one(key));
            // And the keyed function is not the public Fx function (a
            // 2^-64 chance of a false failure per key).
            assert_ne!(a.hash_one(key), id.hash_one(key));
        }
        let mut set = seeded_set();
        assert!(set.insert("k".to_string()));
        assert!(!set.insert("k".to_string()));
        assert!(set.contains("k"));
        // Both constructors build on the one shared seed.
        let sized: SeededSet<u64> = seeded_set_with_capacity(64);
        assert!(sized.capacity() >= 64);
        assert_eq!(sized.hasher().hash_one(7u64), a.hash_one(7u64));
    }
}
