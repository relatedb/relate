//! The sealed arena's 4 GiB admission wall (ADR-0599): the one predicate
//! every writer of an arena's `u32` offsets asks, and the thread-local seam
//! a test lowers it through. It lives beside neither the arena nor the
//! wholesale build, so the policy has one home.

#[cfg(test)]
use crate::test_support::with_override;

/// Whether `arena_bytes` plus a write's `growth` still fits the `u32` offset
/// space — the one predicate the under-lock recheck, the admission
/// pre-check and the wholesale build's whole-side check all consult.
pub(crate) fn arena_admits(arena_bytes: u64, growth: u64) -> bool {
    arena_bytes.saturating_add(growth) <= arena_wall()
}

/// The wall's bound: the `u32` offset space, or a lower one a test pins.
fn arena_wall() -> u64 {
    #[cfg(test)]
    {
        if let Some(wall) = WALL_OVERRIDE.with(std::cell::Cell::get) {
            return wall;
        }
    }
    u32::MAX as u64
}

#[cfg(test)]
thread_local! {
    /// The wall a test pins on this thread (see [`with_arena_wall`]).
    static WALL_OVERRIDE: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// Run `f` with the arena wall lowered to `wall` on this thread, restoring
/// the previous one afterwards. A real 4 GiB build is not a unit test, so a
/// wholesale build past the wall is driven through this seam; the override
/// is per-thread, so it holds for every shard that runs inline on it.
#[cfg(test)]
pub(crate) fn with_arena_wall<R>(wall: u64, f: impl FnOnce() -> R) -> R {
    with_override(&WALL_OVERRIDE, wall, f)
}
