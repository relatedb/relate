//! The property-store seam at the executor's read boundary (ADR-0665): the
//! live reads a property representation answers for the executor. The
//! default implementation is the in-memory store's compact live records
//! (ADR-0033); a columnar store implements the same trait, and the engine
//! adapter maps interned key ids at the boundary (ADR-0541).
//!
//! [`Graph::node_prop_id_at`](super::Graph::node_prop_id_at) reads through
//! [`PropStore::point`], so every point read has one implementation. The
//! equality lookup shape routes through the same attached store
//! (ADR-0665 A/B): a store installed with
//! [`Graph::attach_prop_store`](super::Graph::attach_prop_store) answers
//! it, and with none attached the index routes answer as before. The
//! label scan never routes to the store — the measured ordering
//! (ADR-0665 point 7) keeps it on the canonical label route.

use std::sync::{Arc, OnceLock};

use relate_core::{NodeId, Value};

use super::*;

/// One aggregate a fold read asks of one column — the aggregate ladder's
/// `min`/`max`/`count` asks and [`PropStore::folds`]'s wire, spelled once
/// (the ladder's items and the store's answer share it; the extremum
/// direction is `Extremum`'s, not a third vocabulary).
#[derive(Clone, Copy)]
pub enum PropFold {
    Count,
    Min,
    Max,
}

/// The property-read contract the executor's read boundary dispatches to.
pub trait PropStore: Send + Sync + std::fmt::Debug {
    /// Point read: the value of `kid` on `id`, visible at `ts`.
    fn point(&self, ts: u64, id: NodeId, kid: u32) -> Option<Value>;

    /// Whether the store carries a column for interned key `kid`. A store
    /// that promotes only some types answers the keys it carries. Required,
    /// with no permissive default: a store that forgot to declare it would
    /// answer `point` for a key it has no column for, reporting null where
    /// the compact records hold the value.
    fn carries(&self, kid: u32) -> bool;

    /// Whether the store answers reads at `ts`; a store built from one
    /// snapshot declines every other timestamp, and the compact records
    /// answer. Required for the same reason as [`Self::carries`].
    fn serves_at(&self, ts: u64) -> bool;

    /// `=` / `IN` rows over the store's lookup structure, filtered to
    /// `labels` and capped at `cap + 1`, **ascending and deduplicated**
    /// — the shape the engine's candidate merge consumes (a two-pointer
    /// union with no duplicate guard and no closing re-sort, LABS-803),
    /// so an answer in row order or with repeats is not a slow answer
    /// but a corrupting one. `None` leaves the lookup to the index
    /// route.
    fn equality(
        &self,
        _labels: &[u32],
        _kid: u32,
        _values: &[Value],
        _cap: Option<usize>,
    ) -> Option<Vec<NodeId>> {
        None
    }

    /// The extremum/count folds over one column — the aggregate shapes
    /// ADR-0611's ladder recognizes. The answer aligns position-wise with
    /// `wants`, one entry per ask; `None` for the whole call — a fold
    /// that cannot be answered included — leaves the shape to the
    /// postings path and the scan.
    fn folds(&self, _labels: &[u32], _kid: u32, _wants: &[PropFold]) -> Option<Vec<Value>> {
        None
    }

    /// One `(value, count)` pair per distinct non-null value of `kid`
    /// over the rows carrying `labels` — the shape ADR-0611's grouped
    /// rung folds into its charged table. The absent-value group is the
    /// engine's own (`null_group` reads the label's live count in the
    /// exactness window) — the store never carries it. `None` leaves the
    /// shape to the postings path and the scan.
    fn grouped_counts(&self, _labels: &[u32], _kid: u32) -> Option<Vec<(Value, u64)>> {
        None
    }

    /// The publication feed (ADR-0665's versioning step 1): every row a
    /// commit changed arrives here once, inside the publication's write
    /// window and BEFORE the commit's ts becomes visible to readers
    /// (`end_write` feeds the store, then stores `published_ts`) — a
    /// reader that resolves the new ts must find the delta already
    /// holding the publication, never the pre-commit columns. A store that keeps a delta overlay appends these as its
    /// versioned entries; one that serves a single immutable snapshot
    /// (the round-1/3 shape) can decline future reads instead, with an
    /// empty arm stating that choice. Required, with no permissive
    /// default, for the same reason as [`Self::carries`]: a store that
    /// forgot to declare its feed policy would silently drop every
    /// publication. `ts` is monotonic across calls; an aborted write
    /// never reaches here.
    fn on_publication(&self, ts: u64, rows: &[PublishedRow]);
}

/// One changed row a publication hands the attached store — the
/// delta-overlay feed (ADR-0665's versioning step 1): the row's committed
/// state at the publication's `ts`. `props` is the row's FULL property
/// state by interned key (a key the store carries that is absent here is
/// absent at `ts` — the row state, not a patch); `labels` are the interned
/// label ids it carries; `deleted` is the tombstone for a row the commit
/// removed.
pub struct PublishedRow {
    pub id: NodeId,
    pub labels: Vec<u32>,
    pub props: Vec<(u32, Value)>,
    pub deleted: bool,
}

/// The default store: the compact live records in the MVCC version chains.
impl PropStore for Graph {
    fn point(&self, ts: u64, id: NodeId, kid: u32) -> Option<Value> {
        // ADR-0665 A/B: an attached experimental store answers for the keys
        // its columns carry at its own snapshot; every other read stays on
        // the compact records (a store may promote only some types, so
        // untyped and unpromoted properties fall through unchanged).
        if let Some(store) = self.serving_store(ts).filter(|store| store.carries(kid)) {
            return store.point(ts, id, kid);
        }
        self.node_prop_row(id, ts, kid)
    }

    // The compact records carry every interned key and answer every
    // timestamp (visibility is resolved in `point`).
    fn carries(&self, _kid: u32) -> bool {
        true
    }

    fn serves_at(&self, _ts: u64) -> bool {
        true
    }

    // The compact records ARE the live store: a publication changes them
    // directly, so there is nothing to feed a second representation.
    fn on_publication(&self, _ts: u64, _rows: &[PublishedRow]) {}
}

/// The attached experimental store's whole state (ADR-0665 A/B) — the
/// shared cell, the publication feed's pending rows, and the
/// write-lifecycle hooks that keep them — one bundle beside its contract
/// in this module, so the graph's central state file carries one field.
#[derive(Debug, Default)]
pub(crate) struct AttachedStore {
    /// The installed store; empty until [`Self::attach`] publishes one.
    cell: OnceLock<Arc<dyn PropStore>>,
    /// Rows a write window marked for the store's publication feed
    /// (ADR-0665 versioning step 1) — taken and handed to the store at
    /// the commit's publication close, cleared on abort. Beside the
    /// checkpoint's own dirty sets, not part of them: the checkpoint's
    /// take-and-clear must never starve the feed.
    pending: std::sync::Mutex<std::collections::HashSet<NodeId>>,
}

impl AttachedStore {
    /// Install a store: first attach wins, a second call leaves the
    /// published store untouched, drops the argument, and returns
    /// `false`. The graph must be quiescent — the store is built from
    /// its state at attach time.
    pub(crate) fn attach(&self, store: Arc<dyn PropStore>) -> bool {
        self.cell.set(store).is_ok()
    }

    /// The installed store, if any.
    pub(crate) fn get(&self) -> Option<&Arc<dyn PropStore>> {
        self.cell.get()
    }

    /// Mark `id` for the publication feed — gated on a store being
    /// attached, so the default write path pays one acquire load, not a
    /// second mutex for a set nobody drains.
    pub(crate) fn mark_dirty(&self, id: NodeId) {
        if self.cell.get().is_some() {
            self.pending.lock().unwrap().insert(id);
        }
    }

    /// The write window's marked rows, taken: the commit's publication
    /// close materializes and hands them to the store.
    pub(crate) fn take_pending(&self) -> Vec<NodeId> {
        std::mem::take(&mut *self.pending.lock().unwrap())
            .into_iter()
            .collect()
    }

    /// An aborted write never reaches the feed.
    pub(crate) fn clear(&self) {
        self.pending.lock().unwrap().clear();
    }
}

impl Graph {
    /// Install an experimental property store (ADR-0665 A/B): point reads
    /// and the equality/aggregate lookups then route to it.
    #[doc(hidden)]
    pub fn attach_prop_store(&self, store: Arc<dyn PropStore>) -> bool {
        self.attached_store.attach(store)
    }

    /// The interned id of `name`, if a property or label of that name has
    /// been interned. The A/B harnesses map their name-keyed lookups through
    /// this boundary.
    #[doc(hidden)]
    pub fn interned_id(&self, name: &str) -> Option<u32> {
        self.interner.get_id(name)
    }

    /// The store attached to this graph, if any.
    pub(crate) fn attached_prop_store(&self) -> Option<&Arc<dyn PropStore>> {
        self.attached_store.get()
    }

    /// The attached store when it answers reads at `ts` — the one
    /// attached-store gate every read shape consults.
    fn serving_store(&self, ts: u64) -> Option<&Arc<dyn PropStore>> {
        self.attached_prop_store()
            .filter(|store| store.serves_at(ts))
    }

    /// [`Self::serving_store`] as an owned handle — the per-statement
    /// resolution's one-time call (ADR-0665 A/B): a statement's read guard
    /// pins the publication epoch, so this answer cannot move while the
    /// statement runs (ADR-0508's placement argument), and every read
    /// below the overlay dispatches through the one handle instead of
    /// re-resolving per read.
    pub(crate) fn serving_store_handle(&self, ts: u64) -> Option<Arc<dyn PropStore>> {
        self.serving_store(ts).cloned()
    }

    /// The compact records' own point read — the default arm of
    /// [`PropStore::point`] and the per-statement resolution's fallback
    /// when the resolved store declines a key.
    pub(crate) fn node_prop_row(&self, id: NodeId, ts: u64, kid: u32) -> Option<Value> {
        self.nodes
            .get(&id)
            .and_then(|vers| visible_version(&vers, ts).and_then(|v| v.live.prop_by_id(kid)))
    }

    /// The store's `=` / `IN` arm for the executor's equality serves. `cap`
    /// is the capped serve's stop (`cap + 1`); `None` answers in full. `None`
    /// leaves the lookup to the index route.
    pub(crate) fn prop_store_equality(
        &self,
        labels: &[String],
        key: &str,
        values: &[Value],
        ts: u64,
        cap: Option<usize>,
    ) -> Option<Vec<NodeId>> {
        let store = self.serving_store(ts)?;
        let (kid, lids) = self.resolved_serving(store, labels, key)?;
        let ids = store.equality(&lids, kid, values, cap)?;
        // The store answer's acceptance protocol (the folds alignment
        // and `null_group`'s checked subtraction are the siblings): the
        // engine's candidate merge is a two-pointer union over ascending,
        // deduplicated sides, so an out-of-crate answer that is unsorted
        // or repeats ids would silently corrupt the merge — decline to
        // the index route, the one answer to a broken store.
        let ascending_dedup = ids.windows(2).all(|pair| pair[0] < pair[1]);
        ascending_dedup.then_some(ids)
    }

    /// The store's aggregate arm for ADR-0611's ladder — the folds rung.
    /// Serves behind the declared postings (the caller checks), gated by
    /// the same attach/snapshot/carry rules as the set arms; `None`
    /// leaves the shape to the postings path and the scan.
    pub(crate) fn prop_store_folds(
        &self,
        labels: &[String],
        key: &str,
        wants: &[PropFold],
        ts: u64,
    ) -> Option<Vec<Value>> {
        let store = self.serving_store(ts)?;
        let (kid, lids) = self.resolved_serving(store, labels, key)?;
        // The store answer's ONE acceptance protocol, owned at the seam:
        // the answer aligns position-wise with `wants`, one entry per
        // ask — a store answering any other length is broken and the
        // whole arm declines to the postings path, never a partial row
        // (the ladder's rungs consume validated answers only; the
        // grouped arm's twin guard is `null_group`'s checked
        // subtraction, which declines a broken count the same way).
        let served = store.folds(&lids, kid, wants)?;
        (served.len() == wants.len()).then_some(served)
    }

    /// The store's aggregate arm for ADR-0611's ladder — the grouped
    /// rung's feed. Same gates; `None` leaves it to the postings path.
    pub(crate) fn prop_store_grouped_counts(
        &self,
        labels: &[String],
        key: &str,
        ts: u64,
    ) -> Option<Vec<(Value, u64)>> {
        let store = self.serving_store(ts)?;
        let (kid, lids) = self.resolved_serving(store, labels, key)?;
        store.grouped_counts(&lids, kid)
    }

    /// Hand the attached store the rows this commit changed — the
    /// delta-overlay feed (ADR-0665's versioning step 1). Runs inside the
    /// publication close, after `commit_ts` advanced and before the write
    /// window ends: every pending row's committed state materializes once
    /// (labels by id, properties by id from the compact record), the
    /// tombstone for a row the commit removed. A store with a delta
    /// overlay appends these as its versioned entries; the round-1/3
    /// single-snapshot stores ignore them with an empty arm. Aborted
    /// writes never reach here — `abort_write` clears the pending set
    /// without publishing.
    pub(in crate::graph) fn publish_to_prop_store(&self, ts: u64) {
        let ids = self.attached_store.take_pending();
        let Some(store) = self.attached_prop_store() else {
            // Nothing is attached: the pending set drained above, so a
            // later attach never receives a stale backlog.
            return;
        };
        if ids.is_empty() {
            return;
        }
        let mut rows = Vec::with_capacity(ids.len());
        for id in ids {
            let vers = self.nodes.get(&id);
            match vers.as_ref().and_then(|vers| visible_version(vers, ts)) {
                Some(v) => rows.push(PublishedRow {
                    id,
                    labels: v.live.labels.clone(),
                    props: v
                        .live
                        .props
                        .iter()
                        .map(|(kid, value)| (kid, value.clone()))
                        .collect(),
                    deleted: false,
                }),
                None => rows.push(PublishedRow {
                    id,
                    labels: Vec::new(),
                    props: Vec::new(),
                    deleted: true,
                }),
            }
        }
        store.on_publication(ts, &rows);
    }

    /// The one carry/label gate every key-shaped store arm consults: the
    /// interned id of `key` the store carries, and the interned label ids.
    /// `None` — the key or a label was never interned, or the store carries
    /// no column for the key — leaves the read to the index route, the
    /// postings path and the scan.
    fn resolved_serving(
        &self,
        store: &Arc<dyn PropStore>,
        labels: &[String],
        key: &str,
    ) -> Option<(u32, Vec<u32>)> {
        // A key no node carries, or one whose values the store does not
        // promote, falls through: a promoted column answers every value.
        let kid = self.interned_id(key)?;
        if !store.carries(kid) {
            return None;
        }
        let lids = self.interned_labels(labels)?;
        Some((kid, lids))
    }

    /// The interned ids of every label, or `None` when one was never
    /// interned (no node carries it, so the answer is empty).
    fn interned_labels(&self, labels: &[String]) -> Option<Vec<u32>> {
        labels.iter().map(|label| self.interned_id(label)).collect()
    }
}
