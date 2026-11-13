//! Lazy per-label snapshot probe behind `+labels` / `-labels` accounting
//! (ADR-0171). It replaces ADR-0075's whole-graph label census.
//!
//! The module also hosts the committed-graph snapshot reads the probe and
//! constraint validation (ADR-0172 §4-5) share, so the MVCC version-chain
//! walk behind them exists exactly once ([`Graph::with_visible_node`]).

#[cfg(test)]
use std::cell::Cell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use relate_core::{NodeId, RelId};
use relate_ir::RelPattern;
use relate_names::Token;

use crate::store::{LiveNode, RelTypes};

use super::{visible, visible_version, Graph};

#[cfg(test)]
thread_local! {
    /// Snapshot node ids whose visible version label accounting checked
    /// (ADR-0171), visible or not. Test-only instrumentation (LABS-805):
    /// the probe's copy-schedule tests are the counter's only readers,
    /// so a production build pays no thread-local access per checked id.
    static LABEL_PROBE_CHECKS: Cell<u64> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn take_label_probe_checks() -> u64 {
    LABEL_PROBE_CHECKS.with(|c| c.replace(0))
}

/// Count one checked id — test-only (see [`LABEL_PROBE_CHECKS`]): the
/// one counting point, a no-op in production builds.
#[cfg(test)]
fn count_label_probe_check() {
    LABEL_PROBE_CHECKS.with(|c| c.set(c.get() + 1));
}

/// The production spelling of [`count_label_probe_check`]: nothing to
/// count, nothing to pay.
#[cfg(not(test))]
fn count_label_probe_check() {}

/// Ids in a label's first `label_index` copy within a scratch. Each later
/// copy of that label doubles it (ADR-0171's lock-order rounds).
const FIRST_COPY: usize = 64;

/// What one scratch has learned about one label at its snapshot (ADR-0171's
/// memo). The memo's `complete` is `whole && pending.is_empty()`: every id
/// of a whole-set copy has been checked, so `visible` is `S`.
struct LabelMemo {
    /// Ids already inspected.
    checked: HashSet<NodeId>,
    /// Ids copied but not yet checked, in copy order.
    pending: VecDeque<NodeId>,
    /// Checked ids whose version visible at the snapshot carries the label.
    /// Signed like the `k = need - D` it is compared with.
    visible: i64,
    /// The latest copy took the whole set.
    whole: bool,
    /// Size of the next copy.
    next_copy: usize,
}

impl LabelMemo {
    fn new() -> Self {
        Self {
            checked: HashSet::new(),
            pending: VecDeque::new(),
            visible: 0,
            whole: false,
            next_copy: FIRST_COPY,
        }
    }
}

/// One write scratch's per-label probe of its pinned snapshot (ADR-0171).
///
/// It is bound to that `snapshot_ts` and records facts about it, never
/// about the scratch's own writes, so it is never invalidated. Clones share
/// one memo: `scratch_snapshot` / `restore_scratch` (CALL … IN
/// TRANSACTIONS) keep what was learned instead of deep-copying it. Memos are
/// keyed by interned label id, which never changes for a label.
#[derive(Clone)]
pub(crate) struct LabelProbe {
    snapshot_ts: u64,
    memos: Arc<Mutex<HashMap<u32, LabelMemo>>>,
}

impl LabelProbe {
    pub(crate) fn new(snapshot_ts: u64) -> Self {
        Self {
            snapshot_ts,
            memos: Arc::default(),
        }
    }

    fn memos(&self) -> MutexGuard<'_, HashMap<u32, LabelMemo>> {
        self.memos.lock().unwrap_or_else(|poisoned| {
            // A panic mid-probe can leave a memo half-updated. The memos only
            // cache snapshot facts, so dropping them is always correct.
            let mut memos = poisoned.into_inner();
            memos.clear();
            self.memos.clear_poison();
            memos
        })
    }

    /// `Some(S)` when fewer than `k` nodes visible at the snapshot carry
    /// `label`, `None` once `k` of them do, which is at once for `k <= 0`.
    /// `S` is exact whenever it is returned (ADR-0171).
    ///
    /// - Callers probe only a label the current operation gains or loses,
    ///   once per label, with `k = need - D` from
    ///   `live_label_delta` taken before the operation (`need` 1 for a gain,
    ///   2 for a loss; ADR-0171's gain and loss checks). A `None` means the
    ///   label stays live; an
    ///   exact `S` also tells whether the snapshot carried it (the
    ///   `label_on_snapshot` probe).
    /// - An id of `Graph::label_index` counts only if its newest version
    ///   visible at the snapshot exists and carries `label`'s interned id.
    ///   Ids that are not visible, or whose visible version lacks the label
    ///   (created or relabeled after the snapshot, awaiting GC), are checked
    ///   and skipped. The scratch overlay is ignored: its effects are all in
    ///   `D`. An uninterned label has `S = 0` and needs no probe.
    /// - Lock order: this holds at most one of the `label_index`
    ///   and `nodes` guards at a time. It copies at most `c` ids (64 for the
    ///   label's first copy in the scratch, doubled on each later copy) under
    ///   one `label_index` read guard, drops it, and then checks each id
    ///   under its own short `nodes` guard. It never holds a `label_index`
    ///   guard while acquiring a `nodes` guard, so it cannot reverse
    ///   `Graph::index_add`'s `nodes` -> `label_index` order.
    /// - Memo: each id is checked at most once per scratch, and only
    ///   a fully checked whole-set copy may conclude "fewer than `k`".
    ///
    /// Every node visible with `label` at a pinned snapshot stays in the
    /// label's `label_index` set while the snapshot is pinned: ids are added
    /// at apply, before publication, and leave only at a horizon-respecting
    /// reclamation — the checkpoint GC or the apply drain (ADR-0595), both
    /// of which keep versions visible at a pinned snapshot. So a `None`
    /// equals the old
    /// census's `S + D >= need`, for autocommit statements and explicit
    /// transactions alike (ADR-0171).
    pub(crate) fn carriers_below(&self, graph: &Graph, label: &str, k: i64) -> Option<i64> {
        let Some(lid) = graph.label_id(label) else {
            return (k > 0).then_some(0);
        };
        let mut memos = self.memos();
        let memo = memos.entry(lid).or_insert_with(LabelMemo::new);
        loop {
            if memo.visible >= k {
                return None;
            }
            if let Some(id) = memo.pending.pop_front() {
                memo.checked.insert(id);
                count_label_probe_check();
                if graph.node_carries_label_at(id, lid, self.snapshot_ts) {
                    memo.visible += 1;
                }
            } else if memo.whole {
                return Some(memo.visible);
            } else {
                let (ids, whole) = graph.label_index_copy(lid, memo.next_copy);
                memo.next_copy = memo.next_copy.saturating_mul(2);
                memo.whole = whole;
                memo.pending = ids
                    .into_iter()
                    .filter(|id| !memo.checked.contains(id))
                    .collect();
            }
        }
    }
}

/// The probe's reads of the graph, plus the snapshot-read accessors
/// constraint validation runs against the committed graph (ADR-0172 §4-5).
/// Each takes one guard, either a `label_index` guard or a `nodes` guard,
/// and drops it before it returns, so the probe never holds both and never
/// reverses `index_add`'s `nodes` -> `label_index` order.
impl Graph {
    /// Interned id of `label`. `None` means no node has ever carried it: the
    /// interner is append-only and a label is interned before any version
    /// carrying it is installed.
    pub(crate) fn label_id(&self, label: &str) -> Option<u32> {
        self.interner.get_id(label)
    }

    /// Interned id of property key `key`; `None` when no payload ever
    /// carried it. The one spelling of the key internment the scan
    /// predicates resolve once per walk instead of once per row.
    pub(crate) fn prop_key_id(&self, key: &str) -> Option<u32> {
        self.interner.get_id(key)
    }

    /// The interned id of relationship type `rel_type`; `None` when no rel
    /// ever carried it. The interner is append-only and a type is interned
    /// before any version carrying it is installed.
    pub(crate) fn rel_type_id(&self, rel_type: &str) -> Option<u32> {
        self.interner.get_id(rel_type)
    }

    /// `names` — a pattern's written type list — resolved to the interned
    /// domain once (LABS-801). The boundary a walk crosses before its
    /// per-edge loop: everything below compares [`RelTypes`]' ids, never
    /// the names.
    pub(crate) fn rel_types<T: AsRef<str>>(&self, names: &[T]) -> RelTypes {
        RelTypes::resolve(&self.interner, names)
    }

    /// [`Self::rel_types`] for a planned pattern, with the caller's
    /// token→id resolution: the planner's resolved ids (ADR-0541) are
    /// the caller's to interpret — the overlay view's arm resolves a
    /// stale `Absent` token by name (the `prop_key_id_resolved` twin
    /// for types), keeping every other token's `id_or_else` policy.
    /// The interner stays inside this module; the caller supplies only
    /// the token policy.
    pub(crate) fn rel_types_of(
        &self,
        rel: &RelPattern,
        resolve: impl Fn(&str, Token) -> Option<u32>,
    ) -> RelTypes {
        RelTypes::of_pattern_resolved(&self.interner, rel, resolve)
    }

    /// The name of interned rel-type id `tid` — the reverse of
    /// [`Self::rel_type_id`], for the consumers that hold the compiled
    /// form (the frontier NFA's transitions) and need the written name
    /// the name-keyed reads filter on. The interner's own `Arc<str>`
    /// unit, so a holder's clone is a refcount, never a copy. The id
    /// was interned before any version carried it, so a miss is a bug,
    /// not data: the interner panics by its own documented rule rather
    /// than answering a fabricated name.
    pub(crate) fn rel_type_name(&self, tid: u32) -> Arc<str> {
        self.interner.get(tid)
    }

    /// The type name shared for a VISIBLE relationship at `ts` — the
    /// version's own `Arc<str>` (one refcount bump, no String build).
    /// `None` when the rel is not visible: the overlay's shared type
    /// read resolves the batch layer first, so this is the base half.
    pub(crate) fn rel_type_name_shared(&self, id: RelId, ts: u64) -> Option<Arc<str>> {
        self.rels
            .get(&id)
            .and_then(|vers| visible_version(&vers, ts).map(|ver| ver.live.type_name_shared()))
    }

    /// At most `cap` ids of label `lid`'s `label_index` set, copied under one
    /// short read guard, and whether that copy took the whole set
    /// (`len <= cap`). A label with no set copies nothing, and that copy is
    /// whole.
    pub(super) fn label_index_copy(&self, lid: u32, cap: usize) -> (Vec<NodeId>, bool) {
        match self.indexes.label_index.get(&lid) {
            Some(set) => (set.iter().take(cap).copied().collect(), set.len() <= cap),
            None => (Vec::new(), true),
        }
    }

    /// Run `f` on the live record of `id`'s newest version visible at
    /// `ts` under one short `nodes` read guard; `None` when no version is
    /// visible. The one MVCC version-chain walk behind every label probe,
    /// snapshot label read, and scan-predicate check.
    pub(crate) fn with_visible_node<R>(
        &self,
        id: NodeId,
        ts: u64,
        f: impl FnOnce(&LiveNode) -> R,
    ) -> Option<R> {
        self.nodes.get(&id).and_then(|vers| {
            vers.iter()
                .rev()
                .find(|v| visible(v.xmin, v.xmax, ts))
                .map(|v| f(v.live))
        })
    }

    /// Whether `id`'s newest version visible at `ts` exists and carries
    /// label id `lid`, read under one short `nodes` read guard.
    pub(super) fn node_carries_label_at(&self, id: NodeId, lid: u32, ts: u64) -> bool {
        self.with_visible_node(id, ts, |live| live.has_label(lid))
            .unwrap_or(false)
    }

    /// The label's interned id, when anything in the store ever carried
    /// it — the one interning lookup a caller that checks the label per
    /// candidate makes (ADR-0473's prepared programs), instead of one per
    /// candidate.
    pub(crate) fn interned_label_id(&self, label: &str) -> Option<u32> {
        self.label_id(label)
    }

    /// Whether `id`'s version visible at `ts` carries the interned label
    /// `lid`. `None` when no version is visible.
    pub(crate) fn node_has_label_id_at(&self, id: NodeId, ts: u64, lid: u32) -> Option<bool> {
        self.with_visible_node(id, ts, |live| live.has_label(lid))
    }

    /// Ids of the nodes whose version visible at `ts` carries `label`,
    /// ascending. The label's `label_index` set is copied under one short
    /// guard, dropped, and each id is then checked under its own `nodes`
    /// guard against the already-interned label id, so no two guards are
    /// held at once and no candidate re-interns the label string.
    pub(crate) fn node_ids_with_label_at(&self, label: &str, ts: u64) -> Vec<NodeId> {
        let Some(lid) = self.label_id(label) else {
            return Vec::new();
        };
        let candidates: Vec<NodeId> = match self.indexes.label_index.get(&lid) {
            Some(set) => set.iter().copied().collect(),
            None => return Vec::new(),
        };
        let mut ids: Vec<_> = candidates
            .into_iter()
            .filter(|&id| self.node_carries_label_at(id, lid, ts))
            .collect();
        ids.sort_unstable();
        ids
    }

    /// Ids of the relationships of type `rel_type` visible at `ts`,
    /// ascending: one pass that reads each relationship's versions once and
    /// compares interned type ids, building no type string.
    pub(crate) fn rel_ids_of_type_at(&self, rel_type: &str, ts: u64) -> Vec<RelId> {
        let Some(type_id) = self.rel_type_id(rel_type) else {
            return Vec::new();
        };
        let mut ids = Vec::new();
        self.rels.for_each(|id, vers| {
            let matches = vers
                .iter()
                .rev()
                .find(|v| visible(v.xmin, v.xmax, ts))
                .is_some_and(|v| v.live.type_id == type_id);
            if matches {
                ids.push(id);
            }
        });
        ids.sort_unstable();
        ids
    }
}

#[cfg(test)]
mod tests;
