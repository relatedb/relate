//! The pair partition's reach cache (ADR-0186): consecutive pairs from one
//! start share one breadth-first reach set, so a pair whose end lies
//! outside it is answered without a search of its own.
//!
//! An all-pairs `MATCH` binds both endpoints before the shortest-path
//! pattern runs, so the route sees one (start, end) pair per input row and
//! runs one search per pair — most of which find nothing. The cache keeps
//! the reach set of the start the last pairs shared. It only ever answers
//! "unreachable": a reachable end still takes the pair search, so the
//! trail chosen, its tie-break and the row order are the per-pair path's.
//!
//! The cache lives in the pattern's [`super::super::GpmFilters`], built
//! once per clause, so it spans the rows of one clause evaluation and
//! never a write: the graph it read cannot change under it.

#[cfg(test)]
use std::cell::Cell;
use std::cell::RefCell;

use relate_core::tokens::WorkspaceCharge;
use relate_core::{NodeId, Result};
use relate_ir::Direction;

use crate::hash_seed::IdSet;
use crate::shortest::Search;

/// The most nodes one reach set holds. A set is `cap` ids at most (about
/// 16 MiB), charged to the workspace ledger while it lives; a start whose
/// reach is larger is searched pair by pair, as without the cache.
const REACH_CAP: usize = 1 << 20;

/// The pair count from one start at which its reach set is first tried:
/// the second consecutive pair is the evidence that a start repeats.
const FIRST_TRY: u32 = 2;

/// Reach-set entries one searched pair pays for: a set is attempted only
/// up to this many entries per pair already searched from its start.
const ENTRIES_PER_PAIR: usize = 64;

/// How much later a set that did not fit its budget is retried.
const RETRY_FACTOR: u32 = 4;

/// Bytes charged per reach-set entry: the id plus the table's control byte
/// and load slack.
const ENTRY_BYTES: u64 = 16;

#[cfg(test)]
thread_local! {
    static CAP: Cell<Option<usize>> = const { Cell::new(None) };
    static BUILDS: Cell<u64> = const { Cell::new(0) };
    static SKIPS: Cell<u64> = const { Cell::new(0) };
}

/// [`REACH_CAP`], or the test override that lets a small graph reach it.
fn cap() -> usize {
    #[cfg(test)]
    if let Some(cap) = CAP.with(Cell::get) {
        return cap;
    }
    REACH_CAP
}

/// Run `f` with the reach cap lowered to `cap` on this thread.
#[cfg(test)]
pub(super) fn with_reach_cap<T>(cap: usize, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<usize>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CAP.with(|c| c.set(self.0));
        }
    }
    let _restore = Restore(CAP.with(|c| c.replace(Some(cap))));
    f()
}

/// Take and reset this thread's counts: reach sets built, and pair
/// searches the cache answered.
#[cfg(test)]
pub(super) fn take_reach_counts() -> (u64, u64) {
    (BUILDS.with(|c| c.replace(0)), SKIPS.with(|c| c.replace(0)))
}

/// What one search reads of a start: its identity and the hop bound that
/// shapes the reach set. The relationship types are the pattern's, fixed
/// for the cache's life.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Key {
    start: NodeId,
    dir: Direction,
    max: u32,
}

impl Key {
    fn of(search: &Search<'_, '_>, start: NodeId) -> Self {
        Key {
            start,
            dir: search.dir,
            max: search.max,
        }
    }
}

/// What one lookup does next, decided under the state borrow.
enum Step {
    /// The answer is known: `true` is "certainly unreachable".
    Answer(bool),
    /// Try the reach set, `pairs` pairs having been seen from the start.
    Build(u32),
}

#[derive(Default)]
enum State {
    /// Nothing seen yet.
    #[default]
    Cold,
    /// Pairs seen from the key's start, with their count at which a reach
    /// set is next attempted.
    Seen { key: Key, pairs: u32, retry: u32 },
    /// The reach set of the key's start, charged while held.
    Ball {
        key: Key,
        reached: IdSet<NodeId>,
        _charge: WorkspaceCharge,
    },
    /// The key's start reaches more than the hard cap: pair searches only.
    Refused(Key),
}

/// The last start's reach set, shared by the rows of one clause.
#[derive(Default)]
pub(crate) struct ReachCache {
    state: RefCell<State>,
}

impl ReachCache {
    /// Whether `end` is certainly outside `search`'s reach from `start`
    /// (`true`: no trail of length 1 to `max` joins them, no search needed).
    /// `false` means unknown or reachable — the caller searches.
    ///
    /// Preconditions of the pair search, which `route_shape` guarantees:
    /// `start` ≠ `end`, `min` ≤ 1.
    pub(super) fn unreachable(
        &self,
        search: &Search<'_, '_>,
        start: NodeId,
        end: NodeId,
    ) -> Result<bool> {
        debug_assert!(start != end && search.min <= 1);
        let key = Key::of(search, start);
        let attempt = {
            let mut state = self.state.borrow_mut();
            match &mut *state {
                State::Ball {
                    key: held, reached, ..
                } if *held == key => Step::Answer(!reached.contains(&end)),
                State::Refused(held) if *held == key => Step::Answer(false),
                State::Seen {
                    key: held,
                    pairs,
                    retry,
                } if *held == key => {
                    *pairs = pairs.saturating_add(1);
                    if *pairs < *retry {
                        Step::Answer(false)
                    } else {
                        Step::Build(*pairs)
                    }
                }
                _ => {
                    *state = State::Seen {
                        key,
                        pairs: 1,
                        retry: FIRST_TRY,
                    };
                    Step::Answer(false)
                }
            }
        };
        let out = match attempt {
            Step::Answer(answer) => answer,
            Step::Build(pairs) => self.build(search, key, pairs, end)?,
        };
        #[cfg(test)]
        if out {
            SKIPS.with(|c| c.set(c.get() + 1));
        }
        Ok(out)
    }

    /// Try the reach set of `key`'s start, within the work the `pairs`
    /// searched so far pay for ([`ENTRIES_PER_PAIR`] each), and answer `end`
    /// from it. A set past that budget is retried at four times the pairs,
    /// so the work spent on sets that do not fit stays a constant multiple
    /// of the pair searches; one past the hard cap is refused for good.
    fn build(&self, search: &Search<'_, '_>, key: Key, pairs: u32, end: NodeId) -> Result<bool> {
        #[cfg(test)]
        BUILDS.with(|c| c.set(c.get() + 1));
        let hard = cap();
        let want = hard.min((pairs as usize).saturating_mul(ENTRIES_PER_PAIR));
        let Some(reached) = search.reach(key.start, want)? else {
            *self.state.borrow_mut() = if want >= hard {
                State::Refused(key)
            } else {
                State::Seen {
                    key,
                    pairs,
                    retry: pairs.saturating_mul(RETRY_FACTOR),
                }
            };
            return Ok(false);
        };
        let charge = WorkspaceCharge::retain_with(|| {
            (reached.capacity() as u64).saturating_mul(ENTRY_BYTES)
        })?;
        let out = !reached.contains(&end);
        *self.state.borrow_mut() = State::Ball {
            key,
            reached,
            _charge: charge,
        };
        Ok(out)
    }
}
