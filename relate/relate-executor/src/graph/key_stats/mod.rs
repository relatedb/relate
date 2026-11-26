//! Per-key value statistics (the per-key statistics ADR): for each `(node label, property
//! key)` the count of admitted values, their minimum, maximum and sum, an
//! approximate distinct count and a top-16 heavy-hitter summary, built
//! lazily per label and never persisted.
//!
//! **Consult.** [`Graph::key_stats`] is a single map lookup on a built
//! label: it clones an `Arc`, allocates nothing and holds a shard lock only
//! for the lookup. A label that is unbuilt, stale or being built answers
//! `None` at once and never blocks, builds nothing and never waits for a
//! build in flight; the first miss on an unbuilt label records a build
//! request (one more map operation, miss path only).
//!
//! **Build.** The request is served by [`Graph::build_pending_key_stats`],
//! the engine's maintenance entry: one chunked scan per requested label,
//! taking the node store's lock per node and none between chunks, so
//! writers are never stalled for the scan. The pump is the one place a
//! build runs, and the read executor never calls it. The executor holds
//! the graph by reference, so nothing here can spawn its own thread: the
//! `relate` crate's statement-completion seam (`connection/stats_pump.rs`)
//! calls it after every successful statement, once the statement's own
//! snapshot pin is dropped, for at most one label, and stops it at the
//! next chunk boundary when the statement's cancel token fires (the
//! label returns to the queue). A statement therefore inherits at most one
//! label's scan, and the remaining requests wait for later statements.
//!
//! **Staleness.** A node write counts against each of its labels
//! ([`Graph::key_stats_note_write`], hooked at `index_add` and
//! `suspect_postings`, the two sites every node create, change, label change
//! and delete already pass). Past a threshold of a built label's rows the
//! label turns stale: its entries go and the next consult requests a
//! rebuild. A write during a build counts the same way; a build that saw
//! more than the threshold still publishes (best-effort statistics) and its
//! write count restarts, so a label written faster than it can be scanned
//! is rebuilt once per threshold of writes, never once per read. Below the
//! threshold the statistics stay, a bounded drift on a best-effort
//! estimate. The any-label bucket is unsupported: nothing ages it, so a
//! consult for it is unmeasured and requests nothing.
//!
//! **Budget.** The published statistics hold at most [`BUDGET_BYTES`];
//! structured keys outrank text keys and evict their entries when the
//! budget is full. A key that was evicted or never admitted answers `None`.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use relate_core::Value;

use super::{CoreMap, Graph};

mod accum;
mod build;
mod gate;
mod ledger;
mod sketch;

#[cfg(test)]
mod tests;

use ledger::Ledger;

/// The published statistics' byte budget (about 2,000 keys at the typical
/// entry size). Outside the ADR-0042 logical charge, like the lazy pairs.
pub(super) const BUDGET_BYTES: usize = 5 << 20;

/// Smallest stale threshold, so a small label does not rebuild on every
/// few writes.
const MIN_STALE_WRITES: u64 = 64;

/// A built label turns stale after `rows / STALE_DIVISOR` writes.
const STALE_DIVISOR: u64 = 32;

pub(crate) use gate::is_admitted;
use relate_planner::{HeavyValue, KeyStats};

/// The upper-bound count of `value` when it is one of `stats`' heavy
/// values, by the index's equality identity (an integer and its equal float
/// are one value). The identity hash is recomputed here, from the stored
/// representatives, so the published figures carry no parallel hash state.
pub(crate) fn heavy_count(stats: &KeyStats, value: &Value) -> Option<u64> {
    let wanted = sketch::identity_hash(value)?;
    stats
        .heavy
        .iter()
        .find(|h| sketch::identity_hash(&h.value) == Some(wanted))
        .map(|h| h.count)
}

/// Heap bytes a stored value holds beyond its inline size. Admitted values
/// are scalars and strings of at most [`gate::MAX_TEXT_BYTES`]: only a
/// string owns a payload.
fn value_payload_bytes(value: &Value) -> usize {
    match value {
        Value::String(text) => text.len(),
        _ => 0,
    }
}

/// Estimated heap footprint of one published entry, charged against
/// [`BUDGET_BYTES`]: the struct, the heavy values, and the payload of every
/// stored string (minimum, maximum and heavy values).
fn approx_bytes(stats: &KeyStats) -> usize {
    let held = stats.min.iter().chain(stats.max.iter());
    let heavy = stats.heavy.iter().map(|h| &h.value);
    std::mem::size_of::<KeyStats>()
        + stats.heavy.len() * std::mem::size_of::<HeavyValue>()
        + held.chain(heavy).map(value_payload_bytes).sum::<usize>()
}

/// Where a label stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// A consult asked for it; the pump has not claimed it.
    Requested,
    /// The pump is scanning it.
    Building,
    /// Published.
    Built,
    /// Built once, then written past the threshold; the next consult
    /// requests a rebuild.
    Stale,
}

#[derive(Debug)]
struct LabelState {
    phase: Phase,
    /// Writes since the build began (or, while built, since it published).
    writes: u64,
    /// Writes a built label tolerates before it turns stale.
    stale_after: u64,
}

/// All key-statistics state, one bundle in [`super::index::Indexes`].
#[derive(Debug, Default)]
pub(super) struct KeyStatsState {
    published: CoreMap<(u32, u32), Arc<KeyStats>>,
    labels: CoreMap<u32, LabelState>,
    /// Labels with any state: zero keeps the write-path hook to one load.
    active: AtomicUsize,
    /// Labels waiting for the pump: zero keeps the pump, which every read
    /// statement calls, to one load.
    requested: AtomicUsize,
    ledger: Mutex<Ledger>,
    /// Monotonic count of changes to what a consult can answer: a label's
    /// statistics published, aged out (a label turning stale) or cleared,
    /// bumped AFTER the published map changed. A reader that stamps the
    /// epoch before it consults can therefore never hold a stamp that
    /// postdates a change it did not see (a plan cached against the
    /// statistics of epoch `e` is stale once the epoch moves past `e`).
    epoch: AtomicU64,
}

impl KeyStatsState {
    /// The statistics epoch (see the field): one acquire load.
    pub(super) fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    /// Record that what a consult answers has changed; called after the
    /// change is visible.
    fn bump_epoch(&self) {
        self.epoch.fetch_add(1, Ordering::Release);
    }

    /// The consult: one lookup on a hit; on a miss, record a request when
    /// the label has never been asked for, or went stale.
    fn get(&self, label: u32, key: u32) -> Option<Arc<KeyStats>> {
        if let Some(found) = self.published.get(&(label, key)) {
            return Some(Arc::clone(&found));
        }
        self.request(label);
        None
    }

    /// The published statistics of one key without recording a request:
    /// the read for a consumer that benefits from statistics when they
    /// exist but must not make a build happen on their account.
    fn peek(&self, label: u32, key: u32) -> Option<Arc<KeyStats>> {
        self.published
            .get(&(label, key))
            .map(|found| Arc::clone(&found))
    }

    fn request(&self, label: u32) {
        match self.labels.entry(label) {
            dashmap::mapref::entry::Entry::Vacant(slot) => {
                // The entry's shard stays locked while the counters move, so
                // `clear` removes an entry and its counts as one step.
                let _held = slot.insert(LabelState {
                    phase: Phase::Requested,
                    writes: 0,
                    stale_after: MIN_STALE_WRITES,
                });
                self.active.fetch_add(1, Ordering::Relaxed);
                self.requested.fetch_add(1, Ordering::Relaxed);
            }
            dashmap::mapref::entry::Entry::Occupied(mut slot) => {
                let state = slot.get_mut();
                if state.phase == Phase::Stale {
                    state.phase = Phase::Requested;
                    self.requested.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    /// One node write against `label`.
    fn note_write(&self, label: u32) {
        if self.active.load(Ordering::Relaxed) == 0 {
            return;
        }
        let Some(mut state) = self.labels.get_mut(&label) else {
            return;
        };
        match state.phase {
            Phase::Built => {
                state.writes += 1;
                if state.writes > state.stale_after {
                    let dropped = self.evict_label(label);
                    state.phase = Phase::Stale;
                    // Once per transition, not per write, and only when
                    // published statistics actually went away.
                    if dropped {
                        self.bump_epoch();
                    }
                }
            }
            Phase::Building => state.writes += 1,
            Phase::Requested | Phase::Stale => {}
        }
    }

    /// Drop a label's published entries, whether any were published. The
    /// caller holds the label's state (lock order: labels, then the
    /// ledger, then published).
    fn evict_label(&self, label: u32) -> bool {
        let mut ledger = self.ledger.lock().expect("ledger lock");
        let released = ledger.release_label(label);
        for key in &released {
            self.published.remove(key);
        }
        !released.is_empty()
    }

    /// Whether any label waits for the pump: the one load the pump's fast
    /// path and the connection's seam both start with.
    fn has_requests(&self) -> bool {
        self.requested.load(Ordering::Relaxed) != 0
    }

    /// Claim one requested label for the pump.
    fn claim_requested(&self) -> Option<u32> {
        if !self.has_requests() {
            return None;
        }
        let candidates: Vec<u32> = self
            .labels
            .iter()
            .filter(|e| e.phase == Phase::Requested)
            .map(|e| *e.key())
            .collect();
        candidates.into_iter().find(|label| {
            self.labels.get_mut(label).is_some_and(|mut s| {
                let claimed = s.phase == Phase::Requested;
                if claimed {
                    s.phase = Phase::Building;
                    s.writes = 0;
                    self.requested.fetch_sub(1, Ordering::Relaxed);
                }
                claimed
            })
        })
    }

    /// Give a claimed label back to the queue: its scan was abandoned, so
    /// the next pump serves it again.
    fn unclaim(&self, label: u32) {
        let Some(mut state) = self.labels.get_mut(&label) else {
            return;
        };
        if state.phase == Phase::Building {
            state.phase = Phase::Requested;
            self.requested.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Forget everything. The counters move with each entry under that
    /// entry's shard lock (`request`, `claim_requested`), so removing the
    /// entries here subtracts exactly what they added: a request racing the
    /// clear is either removed and subtracted, or inserted after and
    /// counted, never stranded with a zero counter.
    pub(super) fn clear(&self) {
        let (mut removed, mut requested) = (0usize, 0usize);
        self.labels.retain(|_, state| {
            removed += 1;
            requested += usize::from(state.phase == Phase::Requested);
            false
        });
        self.active.fetch_sub(removed, Ordering::Relaxed);
        self.requested.fetch_sub(requested, Ordering::Relaxed);
        let had_published = !self.published.is_empty();
        self.published.clear();
        *self.ledger.lock().expect("ledger lock") = Ledger::default();
        if had_published {
            self.bump_epoch();
        }
    }

    /// Whether the counters agree with the label table: only meaningful
    /// while no request, claim or clear is in flight.
    #[cfg(test)]
    fn counters_agree(&self) -> bool {
        let requested = self
            .labels
            .iter()
            .filter(|e| e.phase == Phase::Requested)
            .count();
        self.active.load(Ordering::Relaxed) == self.labels.len()
            && self.requested.load(Ordering::Relaxed) == requested
    }
}

/// One published key's statistics beside the decoded names it belongs to.
pub(crate) struct PublishedKeyStats {
    pub(crate) label: Arc<str>,
    pub(crate) property: Arc<str>,
    pub(crate) stats: Arc<KeyStats>,
}

impl Graph {
    /// The statistics epoch: a monotonic count of changes to what
    /// [`Graph::key_stats`] can answer (a label's statistics published,
    /// aged out by writes, or cleared), one atomic load. A consumer that
    /// reads it BEFORE it consults and stores it beside the result can tell
    /// later that the result is out of date (the plan cache replans a plan
    /// that consulted the statistics once the epoch moves).
    pub fn key_stats_epoch(&self) -> u64 {
        self.indexes.key_stats.epoch()
    }

    /// The statistics of `property` on `label`'s nodes by interned id, or
    /// `None` while the label is unbuilt, stale, being built, or the key was
    /// declined or evicted. A hit is one map lookup; a miss never blocks
    /// and never waits for a build in flight (a first miss records a build
    /// request for [`Graph::build_pending_key_stats`]).
    pub(crate) fn key_stats(&self, label: u32, key: u32) -> Option<Arc<KeyStats>> {
        // The any-label bucket has no per-label writes to age it, so it is
        // unsupported: unmeasured, and no build is requested.
        if label == self.indexes.star_label_id {
            return None;
        }
        self.indexes.key_stats.get(label, key)
    }

    /// The distinct count the equality pair's build sizes its table from:
    /// the key's PUBLISHED statistics, read without a request. A pair build
    /// happens once per process (the pair is lazy and kept), so a request
    /// made on its behalf would scan the label to benefit a build that has
    /// already happened; the build uses statistics only when an earlier
    /// consult had them built, and never waits for or asks for a build.
    /// `None` while the label is unbuilt, stale, or the key was declined,
    /// and for the unsupported any-label bucket.
    pub(super) fn published_distinct(&self, label: u32, key: u32) -> Option<u64> {
        if label == self.indexes.star_label_id {
            return None;
        }
        self.indexes.key_stats.peek(label, key).map(|s| s.distinct)
    }

    /// [`Graph::key_stats`] by name; `None` when either name was never
    /// interned. The tests' spelling; production resolves tokens in
    /// `GraphStats`.
    #[cfg(test)]
    pub(crate) fn key_stats_named(&self, label: &str, property: &str) -> Option<Arc<KeyStats>> {
        let label = self.interner.get_id(label)?;
        let key = self.interner.get_id(property)?;
        self.key_stats(label, key)
    }

    /// Whether any label's build is requested: one atomic load, so the
    /// statement-completion seam costs a statement nothing when no consult
    /// asked for statistics.
    pub fn has_pending_key_stats(&self) -> bool {
        self.indexes.key_stats.has_requests()
    }

    /// The maintenance entry of the statistics: serve at most `max_labels`
    /// requested labels' builds, the number built. A label is claimed by one
    /// caller (a concurrent caller finds nothing to claim), and the scan
    /// stops at the next chunk boundary when `keep_going` answers `false`,
    /// returning its label to the queue. The engine's statement-completion
    /// seam calls this with one label, so a statement inherits at most one
    /// label's scan.
    pub fn build_pending_key_stats(
        &self,
        max_labels: usize,
        keep_going: impl Fn() -> bool,
    ) -> usize {
        let mut built = 0;
        while built < max_labels && keep_going() {
            let Some(label) = self.indexes.key_stats.claim_requested() else {
                break;
            };
            if self.build_key_stats_for(label, &keep_going) {
                built += 1;
            }
        }
        built
    }

    /// One node write against each of `labels`: the staleness count.
    pub(super) fn key_stats_note_write(&self, labels: &[u32]) {
        for &label in labels {
            self.indexes.key_stats.note_write(label);
        }
    }

    /// Every published key's statistics beside its decoded label and
    /// property names, ordered by `(label, property)`: the read-only
    /// snapshot a surface lists. It reads the published map alone, so it
    /// never records a build request and never builds, and only labels
    /// whose statistics are currently built (not stale, not evicted) appear.
    pub(crate) fn published_key_statistics(&self) -> Vec<PublishedKeyStats> {
        let mut all: Vec<PublishedKeyStats> = self
            .indexes
            .key_stats
            .published
            .iter()
            .map(|entry| {
                let (label, key) = *entry.key();
                PublishedKeyStats {
                    label: self.interner.get(label),
                    property: self.interner.get(key),
                    stats: Arc::clone(entry.value()),
                }
            })
            .collect();
        all.sort_by(|a, b| (&*a.label, &*a.property).cmp(&(&*b.label, &*b.property)));
        all
    }

    /// Published statistics of every key of `label`, for tests and the
    /// future surface; `None` entries are keys without statistics.
    #[cfg(test)]
    pub(super) fn key_stats_published(
        &self,
        label: u32,
    ) -> std::collections::BTreeMap<u32, Arc<KeyStats>> {
        self.indexes
            .key_stats
            .published
            .iter()
            .filter(|e| e.key().0 == label)
            .map(|e| (e.key().1, Arc::clone(e.value())))
            .collect()
    }
}
