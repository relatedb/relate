//! Input-keyed memoisation of pure expressions and checks (LABS-1478).
//!
//! A stage asks the same question of the same input many times: on a
//! branching graph the arrival set is a small fraction of the candidate
//! stream, so a check that is a *function of its input alone* is paid once
//! per edge for one answer. [`Memo`] keeps the answers; [`Memoized`] puts
//! one in front of any [`NodePred`] without the kernel loops knowing; the
//! same pieces carry any other pure expression.
//!
//! **The purity contract** (what may be wrapped). The answer is determined
//! by the input key and by state fixed for the stage: one snapshot (a read
//! clause never sees its own writes), the statement's parameters, and a
//! bound proven row-invariant and deterministic by the decoder
//! ([`super::program::bound_is_row_invariant`]). Every
//! [`Admission`](super::program::Admission) program meets it by
//! construction. A check that reads the row (a bound-variable comparison,
//! LABS-1433's boundary pair), a nondeterministic call, or a write clause's
//! own-write view does **not**, and must never be wrapped. An error is
//! never cached: it propagates, so the first erroring input surfaces
//! exactly as it would unmemoised.
//!
//! **Hashing the input can cost more than the expression**, so the key
//! scheme is part of the API, in four tiers, cheapest first:
//! 0. no key — a value constant for the stage, computed once ([`Once`]);
//! 1. a direct-indexed table over id keys, no hashing ([`PagedBits`] for
//!    boolean answers, [`NodeValues`] for any `Copy` value);
//! 2. pointer identity of a borrowed, stage-pinned slice (a table plugs in
//!    through [`MemoTable`]; none is needed yet);
//! 3. a wide-value hash, only for expressions whose cost dwarfs it.
//!
//! [`MemoClass`] is the decode-time gate: a cheap expression is never
//! wrapped, because the probe would cost as much as the work.
//!
//! **Cost shape.** Tables follow the keys *touched*, never the row or path
//! count, and report exact bytes ([`MemoTable::heap_bytes`]). Two guards
//! keep a memo from costing more than it saves, both set by a
//! [`MemoPolicy`] per cost class: a warm-up (the first inputs of a stage
//! run unmemoised, so a small stage never allocates a table) and a
//! ceiling (past it the stage keeps answering from the function, hits
//! still served). Three safety nets bound the whole: the table's bytes are
//! charged to the retained-container meter (ADR-0463), every memo of a
//! query shares one live-bytes budget ([`MEMO_QUERY_BUDGET`]), and a memo
//! whose hit rate is below the policy floor at its one review drops its
//! table and bypasses itself. A refusal or a bypass never fails the
//! query: memoisation is an optimisation, always on.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use relate_core::{NodeId, Result};

use crate::ctx::ExecCtx;
use crate::overlay::ExecGraph;

use super::node_map::NodeMap;
use super::program::NodePred;

/// The whole query's live memo bytes, across every stage's memos (a
/// refusal freezes the asking memo, never fails the query).
pub(crate) const MEMO_QUERY_BUDGET: u64 = 16 << 20;

/// The per-statement live-bytes counter every memo of a query shares.
pub(crate) type MemoLive = Arc<AtomicU64>;

/// A memo's tuning: when it starts, when it gives up, how large it may
/// grow. The trigger follows the break-even `N = setup / (hit_rate * cost -
/// probe)`: an expression far costlier than a probe pays after a handful
/// of inputs, a store read after about a thousand. Starting values, to be
/// tuned from measurement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MemoPolicy {
    /// Inputs answered by the function before the table is first
    /// allocated: a stage that never gets this far pays one counter
    /// increment per input and nothing else.
    pub(crate) warm_up: u32,
    /// Probes after which the hit rate is reviewed once. A memo over a
    /// large key domain starts cold: its cumulative hit rate after `n`
    /// probes is about the fraction of the domain already seen, so the
    /// review must wait until the table has had time to fill, or it
    /// disables a memo that would have paid (LABS-1478: a 4096 window
    /// disabled the label memo on a 1.6M-node graph).
    pub(crate) window: u32,
    /// Below this hit rate (percent) at the review, the memo drops its
    /// table and bypasses itself: every answer was used once, so the
    /// memo costs more than it saves.
    pub(crate) min_hit_percent: u32,
    /// The table's byte ceiling for this memo.
    pub(crate) max_bytes: usize,
}

impl MemoPolicy {
    /// A store-read check (a label or property test): about 20-100 ns.
    pub(crate) const MODERATE: Self = Self {
        warm_up: 1024,
        window: 1 << 20,
        min_hit_percent: 10,
        max_bytes: 8 << 20,
    };
    /// A microsecond-class expression (a parse, a compile, a subquery).
    #[allow(
        dead_code,
        reason = "API surface for the next consumers (LABS-1478 follow-ups); pinned by the tests below"
    )]
    pub(crate) const EXPENSIVE: Self = Self {
        warm_up: 8,
        window: 1 << 16,
        min_hit_percent: 2,
        max_bytes: 8 << 20,
    };
}

/// The decode-time verdict on whether an expression is worth a memo.
#[allow(
    dead_code,
    reason = "API surface for the next consumers (LABS-1478 follow-ups); pinned by the tests below"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MemoClass {
    /// Costs about what a probe costs (`id`, `size`, a string compare):
    /// never wrapped.
    Cheap,
    /// Pure, a store read per distinct key (a label or property check):
    /// memoised by input key once warm.
    PerKeyModerate,
    /// Pure and microsecond-class per distinct key (a parse, a compile, a
    /// subquery): memoised by input key almost at once.
    PerKeyExpensive,
    /// Row-invariant: one value per stage ([`Once`]), no key at all.
    ConstantPerStage,
}

impl MemoClass {
    /// The memo's tuning, `None` when the class is not keyed-memoised
    /// (cheap work is never wrapped, a constant uses [`Once`]).
    #[allow(
        dead_code,
        reason = "API surface for the next consumers (LABS-1478 follow-ups); pinned by the tests below"
    )]
    pub(crate) fn policy(self) -> Option<MemoPolicy> {
        match self {
            Self::PerKeyModerate => Some(MemoPolicy::MODERATE),
            Self::PerKeyExpensive => Some(MemoPolicy::EXPENSIVE),
            Self::Cheap | Self::ConstantPerStage => None,
        }
    }
}

/// A stage-local table from an input key to its answer.
pub(crate) trait MemoTable: Default {
    type Key: Copy;
    type Value: Copy;

    fn get(&self, key: Self::Key) -> Option<Self::Value>;
    fn insert(&mut self, key: Self::Key, value: Self::Value);
    /// The table's exact allocation, for the ceiling.
    fn heap_bytes(&self) -> usize;
}

/// Where a memo's bytes are charged: the engine's retained-container
/// meter (ADR-0463), implemented by [`ExecGraph`]. A refusal is not an
/// error to the query: the memo is an optimisation, so it stops growing.
pub(crate) trait MemoMeter {
    /// Add `added` bytes to the memo's running `total`, `Err` when the
    /// configured cap is crossed.
    fn charge(&self, total: &mut u64, added: u64) -> Result<()>;
}

impl MemoMeter for ExecGraph<'_> {
    fn charge(&self, total: &mut u64, added: u64) -> Result<()> {
        self.charge_retained(total, added)
    }
}

/// What a memo did, for PROFILE and tuning: counted after the warm-up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct MemoStats {
    /// Inputs looked up in the table (warm-up inputs are not probes).
    pub(crate) probes: u64,
    /// Probes answered from the table.
    pub(crate) hits: u64,
    /// Answers stored.
    pub(crate) inserts: u64,
    /// Answers not stored because the meter or the ceiling refused.
    pub(crate) refused: u64,
    /// The table's allocation, bytes.
    pub(crate) bytes: u64,
    /// The memo gave up at its hit-rate review and bypasses itself.
    pub(crate) disabled: bool,
}

/// One stage's memo of a pure function over the table's keys — see the
/// module docs for the contract the function must meet.
pub(crate) struct Memo<T: MemoTable> {
    policy: MemoPolicy,
    /// Inputs answered by the function so far, up to the policy's warm-up.
    warm: u32,
    table: T,
    /// The table's running size as charged to the retained-container meter.
    retained: u64,
    /// Set once a meter or the query budget refuses: the table stops
    /// growing, hits are still served.
    frozen: bool,
    /// Set at the hit-rate review when the memo is not paying.
    disabled: bool,
    reviewed: bool,
    /// This memo's share of the query's live memo bytes, returned on drop.
    reserved: u64,
    live: Option<MemoLive>,
    stats: MemoStats,
}

impl<T: MemoTable> Default for Memo<T> {
    fn default() -> Self {
        Self::new(MemoPolicy::MODERATE, None)
    }
}

impl<T: MemoTable> Drop for Memo<T> {
    fn drop(&mut self) {
        if let Some(live) = &self.live {
            live.fetch_sub(self.reserved, Ordering::Relaxed);
        }
    }
}

/// The node-label memo: a node id to a boolean answer.
pub(crate) type NodeMemo = Memo<PagedBits>;

impl<T: MemoTable> Memo<T> {
    /// A memo under `policy`, counting its bytes against the query's
    /// shared `live` total when one is given.
    pub(crate) fn new(policy: MemoPolicy, live: Option<MemoLive>) -> Self {
        Self {
            policy,
            warm: 0,
            table: T::default(),
            retained: 0,
            frozen: false,
            disabled: false,
            reviewed: false,
            reserved: 0,
            live,
            stats: MemoStats::default(),
        }
    }

    /// `key`'s answer: the memoised one, or `compute()`'s — remembered
    /// when the stage is warm and the table is under its ceiling. An error
    /// is returned and never remembered.
    pub(crate) fn answer(
        &mut self,
        meter: &impl MemoMeter,
        key: T::Key,
        compute: impl FnOnce() -> Result<T::Value>,
    ) -> Result<T::Value> {
        if self.disabled {
            return compute();
        }
        if self.warm < self.policy.warm_up {
            self.warm += 1;
            return compute();
        }
        self.stats.probes += 1;
        if let Some(known) = self.table.get(key) {
            self.stats.hits += 1;
            return Ok(known);
        }
        let fresh = compute()?;
        self.remember(meter, key, fresh);
        self.review();
        Ok(fresh)
    }

    /// The answers for `keys`, in order, into `out` (cleared first): the
    /// misses go to `compute` together — one call with the missing keys
    /// and a buffer it fills one value per key, in order — so a batched
    /// function pays its setup once. A key repeated inside one batch is
    /// computed once per occurrence. On an error nothing is remembered.
    #[allow(
        dead_code,
        reason = "API surface for the next consumers (LABS-1478 follow-ups); pinned by the tests below"
    )]
    pub(crate) fn answer_batch(
        &mut self,
        meter: &impl MemoMeter,
        keys: &[T::Key],
        out: &mut Vec<T::Value>,
        compute: impl FnOnce(&[T::Key], &mut Vec<T::Value>) -> Result<()>,
    ) -> Result<()> {
        out.clear();
        if self.disabled {
            compute(keys, out)?;
            return Ok(());
        }
        if self.warm < self.policy.warm_up {
            self.warm = self
                .warm
                .saturating_add(keys.len().min(u32::MAX as usize) as u32);
            compute(keys, out)?;
            debug_assert_eq!(out.len(), keys.len(), "one value per key");
            return Ok(());
        }
        let misses: Vec<T::Key> = keys
            .iter()
            .copied()
            .filter(|&key| self.table.get(key).is_none())
            .collect();
        self.stats.probes += keys.len() as u64;
        self.stats.hits += (keys.len() - misses.len()) as u64;
        let mut fresh = Vec::with_capacity(misses.len());
        if !misses.is_empty() {
            compute(&misses, &mut fresh)?;
            debug_assert_eq!(fresh.len(), misses.len(), "one value per key");
            for (&key, &value) in misses.iter().zip(&fresh) {
                self.remember(meter, key, value);
            }
        }
        self.review();
        let mut fresh = fresh.into_iter();
        for &key in keys {
            out.push(match self.table.get(key) {
                Some(known) => known,
                // Past the ceiling a miss was not remembered: its value is
                // the next computed one, in key order.
                None => fresh.next().expect("one computed value per miss"),
            });
        }
        Ok(())
    }

    /// Store an answer unless the memo is frozen or at its ceiling; charge
    /// the table's growth to the meter, and freeze on a refusal (the
    /// overshoot is at most one page).
    fn remember(&mut self, meter: &impl MemoMeter, key: T::Key, value: T::Value) {
        if self.frozen || self.table.heap_bytes() >= self.policy.max_bytes {
            self.stats.refused += 1;
            return;
        }
        let before = self.table.heap_bytes();
        self.table.insert(key, value);
        self.stats.inserts += 1;
        let grew = self.table.heap_bytes().saturating_sub(before) as u64;
        if grew > 0 && meter.charge(&mut self.retained, grew).is_err() {
            self.frozen = true;
        }
        if let (true, Some(live)) = (grew > 0, &self.live) {
            self.reserved += grew;
            if live.fetch_add(grew, Ordering::Relaxed) + grew > MEMO_QUERY_BUDGET {
                self.frozen = true;
            }
        }
    }

    /// The one hit-rate review, after the policy's window of probes: a
    /// memo whose answers are mostly used once drops its table and
    /// bypasses itself.
    fn review(&mut self) {
        if self.reviewed || self.stats.probes < u64::from(self.policy.window) {
            return;
        }
        self.reviewed = true;
        let floor = u64::from(self.policy.min_hit_percent);
        if self.stats.hits * 100 < self.stats.probes * floor {
            self.table = T::default();
            self.retained = 0;
            self.disabled = true;
            if let Some(live) = &self.live {
                live.fetch_sub(self.reserved, Ordering::Relaxed);
            }
            self.reserved = 0;
        }
    }

    /// What the memo did so far, with the table's current allocation.
    #[allow(
        dead_code,
        reason = "read by PROFILE once the metric is threaded (LABS-1478)"
    )]
    pub(crate) fn stats(&self) -> MemoStats {
        MemoStats {
            bytes: self.table.heap_bytes() as u64,
            disabled: self.disabled,
            ..self.stats
        }
    }

    /// The table's exact allocation.
    #[cfg(test)]
    fn heap_bytes(&self) -> usize {
        self.table.heap_bytes()
    }
}

/// Tier 0: a value constant for the stage, computed on first use. An
/// error is returned and the next ask computes again.
#[allow(
    dead_code,
    reason = "API surface for the next consumers (LABS-1478 follow-ups); pinned by the tests below"
)]
#[derive(Default)]
pub(crate) struct Once<V: Copy>(Option<V>);

impl<V: Copy> Once<V> {
    #[allow(
        dead_code,
        reason = "API surface for the next consumers (LABS-1478 follow-ups); pinned by the tests below"
    )]
    pub(crate) fn get_or_init(&mut self, compute: impl FnOnce() -> Result<V>) -> Result<V> {
        if let Some(known) = self.0 {
            return Ok(known);
        }
        let fresh = compute()?;
        self.0 = Some(fresh);
        Ok(fresh)
    }
}

/// Tier 1: any `Copy` value per node id, in the frontier family's sparse
/// map (open addressed, hashed by one multiply).
#[allow(
    dead_code,
    reason = "API surface for the next consumers (LABS-1478 follow-ups); pinned by the tests below"
)]
pub(crate) struct NodeValues<V>(NodeMap<V>);

impl<V> Default for NodeValues<V> {
    fn default() -> Self {
        Self(NodeMap::default())
    }
}

impl<V: Copy> MemoTable for NodeValues<V> {
    type Key = NodeId;
    type Value = V;

    fn get(&self, key: NodeId) -> Option<V> {
        self.0.get(key).copied()
    }

    fn insert(&mut self, key: NodeId, value: V) {
        self.0.insert(key, value);
    }

    fn heap_bytes(&self) -> usize {
        self.0.heap_bytes()
    }
}

/// Ids per page: 4096, so a page is two 64-word planes (1 KiB).
const PAGE_BITS: u32 = 12;
const PAGE_WORDS: usize = 1 << (PAGE_BITS - 6);
/// Directory entries allowed (ids below 2^28): a larger id is simply not
/// remembered, so a sparse far id cannot grow the directory.
const MAX_PAGES: usize = 1 << 16;

/// One page: which ids are answered, and what the answer is.
struct Page {
    known: [u64; PAGE_WORDS],
    value: [u64; PAGE_WORDS],
}

/// Tier 1: a paged bitmap of boolean answers, 64 node ids per word, two
/// planes (known, value). Pages allocate on first touch, so memory follows
/// the id ranges touched (1 KiB per 4096-id range), never the row or path
/// count; a probe is a directory load and a bit test — no hashing.
#[derive(Default)]
pub(crate) struct PagedBits {
    pages: Vec<Option<Box<Page>>>,
    allocated: usize,
}

impl PagedBits {
    fn split(node: NodeId) -> Option<(usize, usize, u64)> {
        let id = u64::try_from(node).ok()?;
        let page = usize::try_from(id >> PAGE_BITS).ok()?;
        let word = ((id >> 6) as usize) & (PAGE_WORDS - 1);
        (page < MAX_PAGES).then_some((page, word, 1u64 << (id & 63)))
    }
}

impl MemoTable for PagedBits {
    type Key = NodeId;
    type Value = bool;

    fn get(&self, node: NodeId) -> Option<bool> {
        let (page, word, bit) = Self::split(node)?;
        let page = self.pages.get(page)?.as_ref()?;
        (page.known[word] & bit != 0).then(|| page.value[word] & bit != 0)
    }

    fn insert(&mut self, node: NodeId, answer: bool) {
        let Some((page, word, bit)) = Self::split(node) else {
            return;
        };
        if page >= self.pages.len() {
            self.pages.resize_with(page + 1, || None);
        }
        let slot = &mut self.pages[page];
        if slot.is_none() {
            self.allocated += 1;
            *slot = Some(Box::new(Page {
                known: [0; PAGE_WORDS],
                value: [0; PAGE_WORDS],
            }));
        }
        let page = slot.as_mut().expect("page allocated above");
        page.known[word] |= bit;
        if answer {
            page.value[word] |= bit;
        }
    }

    fn heap_bytes(&self) -> usize {
        self.pages.capacity() * std::mem::size_of::<Option<Box<Page>>>()
            + self.allocated * std::mem::size_of::<Page>()
    }
}

/// A [`NodePred`] answered through a [`NodeMemo`]: the wrapped predicate's
/// own `admits` is the check, called at most once per distinct node once
/// the stage is warm. The wrapper is `Copy` and monomorphic like the
/// predicate it wraps — the kernel loop's per-candidate call stays static.
#[derive(Clone, Copy)]
pub(crate) struct Memoized<P>(pub(crate) P);

/// The memoised predicate's per-run state: the wrapped predicate's own
/// prepared form (lazily filled, as unmemoised) beside the memo.
pub(crate) struct MemoState<T> {
    inner: Option<T>,
    memo: NodeMemo,
}

impl<P: NodePred> NodePred for Memoized<P> {
    type Prepared = MemoState<P::Prepared>;

    fn prepare(&self, ctx: ExecCtx<'_>) -> Result<Self::Prepared> {
        // The wrapped predicate prepares itself at its first real check
        // (the lazy once-per-stage evaluation of its bound), so a stage
        // with no candidates evaluates nothing, memoised or not.
        Ok(MemoState {
            inner: None,
            memo: NodeMemo::new(MemoPolicy::MODERATE, Some(ctx.graph.memo_live())),
        })
    }

    fn admits(
        &self,
        prepared: &mut Option<Self::Prepared>,
        ctx: ExecCtx<'_>,
        node: NodeId,
    ) -> Result<bool> {
        if prepared.is_none() {
            *prepared = Some(self.prepare(ctx)?);
        }
        let state = prepared
            .as_mut()
            .expect("prepared before the memoised check");
        let MemoState { inner, memo } = state;
        memo.answer(ctx.graph, node, || self.0.admits(inner, ctx, node))
    }
}

#[cfg(test)]
#[path = "memo_tests.rs"]
mod tests;
