//! The executor's hasher seam: the one place each hashing policy is
//! spelled, so a table picks a policy by name instead of re-spelling it.
//!
//! * [`FxBuildHasher`] — the keyless FxHash for engine-allocated dense ids
//!   (the `CoreMap` family, the distinct set's node/rel word tables). Ids no
//!   client can choose; ADR-0355 bars it from attacker-writable keys.
//! * [`shared_random_state`] — the one process-wide seeded SipHash state
//!   (LABS-822) for every table whose keys a client can influence.

use std::collections::hash_map::RandomState;
use std::sync::OnceLock;

/// FxHash as a `BuildHasher`: the hasher of the `CoreMap` family and of
/// every other engine-id table. Keyless — never for client-controlled keys.
pub(crate) type FxBuildHasher = std::hash::BuildHasherDefault<rustc_hash::FxHasher>;

/// The seeded build-hasher of every table whose keys a client can
/// influence: the one name for the policy [`shared_random_state`]
/// constructs, spelled at type boundaries instead of `RandomState`.
pub(crate) type SeededState = RandomState;

/// The one process-wide seed behind [`shared_random_state`] (LABS-822).
static SHARED_RANDOM_STATE: OnceLock<RandomState> = OnceLock::new();

/// The process-wide seeded hasher state, cloned per table. A `RandomState`
/// clone copies two seed words and never re-reads the thread-local RNG
/// cell, whose read LABS-822 measured as the classic walker's hot cost
/// when every used-set built a fresh `RandomState`. The seed stays secret
/// for the process lifetime, so sharing it carries the same HashDoS
/// exposure as std's per-thread seed.
pub(crate) fn shared_random_state() -> SeededState {
    SHARED_RANDOM_STATE.get_or_init(RandomState::new).clone()
}

/// A map keyed through the seeded policy (LABS-1116, F-1-4): the type a
/// client-keyed map spells at its boundary, constructed with
/// [`shared_random_state`] so no fresh `RandomState` is read. The
/// label-interning memo's keys are the statement's own label strings —
/// client-controlled, so FxHash is barred (ADR-0355).
pub(crate) type SeededMap<K, V> = std::collections::HashMap<K, V, SeededState>;
