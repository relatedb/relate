//! The per-attempt write buffer (ADR-0016) and its rollback image: the
//! scratch the [`ExecGraph`](super::ExecGraph) write view overlays on the
//! committed graph, the one funnel every write posts through
//! (`push_redo`), and the `CALL { … } IN TRANSACTIONS` per-batch
//! checkpoint/restore pair.

use std::collections::{HashMap, HashSet};
use std::mem::size_of;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use crate::graph::{EntityId, LabelProbe, QueryStats};
use crate::redo::RedoOp;
use relate_core::tokens::WorkspaceCharge;
use relate_core::{Node, NodeId, RelId, Relationship, Result};

use super::distinct_labels;
use super::prop_ix::ScratchPropIndex;
use super::redo_log::RedoLog;

/// The per-batch state (ADR-0210): everything a `CALL { … } IN
/// TRANSACTIONS` batch's checkpoint captures and a failed batch's
/// restore puts back — one struct, embedded in [`WriteScratch`], so a
/// new field joins the rollback vocabulary by construction: the struct
/// literal itself is the complete capture, where flat fields needed the
/// same entry hand-synced across the `Clone`, `checkpoint`, and
/// `restore` lists, and a forgotten entry there was a silent rollback
/// gap the compiler could not see.
#[derive(Clone)]
pub(super) struct PerBatchState {
    pub(super) snapshot_ts: u64,
    pub(super) nodes: HashMap<NodeId, Node>,
    /// Lazy equality index over `nodes`, posted to only by `push_redo`.
    pub(super) prop_ix: ScratchPropIndex<Node>,
    pub(super) rels: HashMap<RelId, Relationship>,
    /// Lazy relationship equality index over `rels`, posted to only by
    /// `push_redo` (ADR-0185).
    pub(super) rel_prop_ix: ScratchPropIndex<Relationship>,
    pub(super) deleted_nodes: HashSet<NodeId>,
    pub(super) deleted_rels: HashSet<RelId>,
    pub(super) created_nodes: HashSet<NodeId>,
    pub(super) created_rels: HashSet<RelId>,
    pub(super) out_adj: HashMap<NodeId, Vec<RelId>>,
    pub(super) in_adj: HashMap<NodeId, Vec<RelId>>,
    /// Per-label probe of what `snapshot_ts` carries, filled lazily by
    /// `+labels`/`-labels` accounting and shared by clones (ADR-0171).
    pub(super) label_probe: LabelProbe,
    /// Net live occupancy vs the snapshot (this statement's creates /
    /// deletes / SET/REMOVE). `live = snapshot + delta`.
    pub(super) live_label_delta: HashMap<String, i32>,
}

impl PerBatchState {
    /// Undo one created node (ADR-0642's per-statement rollback): the inverse
    /// of the batch entries the create path makes, stated on the state that
    /// owns them — so the rollback vocabulary lives once, and a field added
    /// here is a compile error at its inverse rather than a silent gap.
    pub(super) fn uncreate_node(&mut self, id: NodeId) {
        let node = self
            .nodes
            .remove(&id)
            .expect("a batched CREATE's node is live in the batch");
        self.bump_live_labels(&node.labels, -1);
        self.created_nodes.remove(&id);
    }

    /// The relationship half of [`Self::uncreate_node`]. Both endpoints'
    /// adjacency lists are cleared separately, in one bulk pass over the
    /// statement's dropped ids ([`Self::drop_adjacency`]) — not a scan per
    /// id, so a statement that created many relationships on one endpoint
    /// rolls back in O(adjacency) instead of O(creates × list).
    pub(super) fn uncreate_rel(&mut self, id: RelId) {
        self.rels
            .remove(&id)
            .expect("a batched CREATE's relationship is live in the batch");
        self.created_rels.remove(&id);
    }

    /// Take the statement's dropped relationship ids back off both endpoint
    /// adjacency lists — one retain pass per map, the bulk inverse of the
    /// push each create made. Called once per rolled-back statement, before
    /// the per-entity undo.
    pub(super) fn drop_adjacency(&mut self, dropped: &HashSet<RelId>) {
        for adj in [&mut self.out_adj, &mut self.in_adj] {
            adj.retain(|_, list| {
                list.retain(|id| !dropped.contains(id));
                !list.is_empty()
            });
        }
    }

    /// Net `delta` onto each distinct label's live-carrier count. The one
    /// home of the accounting: every write verb and the rollback consult it.
    pub(super) fn bump_live_labels(&mut self, labels: &[String], delta: i32) {
        for label in distinct_labels(labels) {
            *self.live_label_delta.entry(label.to_string()).or_insert(0) += delta;
        }
    }
}

/// The statement-spanning scratch state (ADR-0309 counters, ADR-0402 applied
/// fact, REL8-1188 statement-local created sets): the four facts that outlive
/// one `CALL { … } IN TRANSACTIONS` batch (ADR-0210). One struct, and one
/// unit on both edges of a batch's life:
///
/// - **carried** by [`WriteScratch::rebase`] — a batch commit moves it whole
///   to the next batch's scratch, so a new statement-spanning field joins the
///   survivor set by construction, and a rebase cannot carry one field and
///   drop the next;
/// - **captured** by [`WriteScratch::checkpoint`] and put back whole by
///   [`WriteScratch::restore`], so a rolled-back batch's counters and its
///   applied-change count describe only changes that survived.
///
/// The per-batch [`PerBatchState`] is the other half of the rollback image and
/// is never carried; the redo log is the third (its positional cursor).
#[derive(Clone, Default)]
pub(super) struct StatementState {
    /// The ADR-0309 mutation counters accumulated over the statement's
    /// batches; handed out as the statement's `QueryStats`.
    pub(super) stats: QueryStats,
    /// How many changes the current statement has applied (ADR-0402
    /// Decision 2's applied fact): zero at statement entry
    /// ([`WriteScratch::reset_statement`]) and one per change through the
    /// redo funnel, so the statement's own count *is* the fact — no growth
    /// baseline has to be threaded beside it. An ADR-0210 batch drain
    /// (`take_redo`) does not touch it, and it is part of the rollback
    /// image, so a restored batch leaves it exactly as it was at the
    /// capture — a batch that rolled back applied nothing.
    pub(super) applied_ops: u64,
    /// The node ids *this statement* created through the redo funnel: the
    /// statement-local twin of [`PerBatchState::created_nodes`], which
    /// additionally spans the earlier statements of the transaction. Delete
    /// accounting nets a create only against the statement that counted it —
    /// a delete of a node an earlier statement of the transaction created
    /// counts as this statement's delete, never as a negative create
    /// (REL8-1188). The node and rel twins are filled at the one funnel and
    /// cleared at a statement entry and a batch rebase, so each holds exactly
    /// its statement's creates inside the current batch — never more ids than
    /// the workspace-charged batch twin, whose size the charge measures.
    pub(super) created_nodes: HashSet<NodeId>,
    /// The relationship ids *this statement* created: the rel half of
    /// [`Self::created_nodes`]'s rule.
    pub(super) created_rels: HashSet<RelId>,
}

/// Per-attempt write buffer. The shared `Graph` is not mutated until `apply_redo`.
///
/// `workspace` is not cloned. A clone keeps the created entities and takes
/// its own reservation once, at clone time, so dropping the original
/// releases the original reservation once.
pub struct WriteScratch {
    /// The per-batch state — checkpointed, restored, and rebased as one.
    pub(super) batch: PerBatchState,
    /// The statement-spanning state — checkpointed and restored with the
    /// per-batch state as one rollback image, and the only half a batch rebase
    /// carries forward.
    pub(super) statement: StatementState,
    /// The change log, with its catalog positions and validation cursor.
    pub(super) redo: RedoLog,
    pub(super) node_id_alloc: Arc<AtomicU64>,
    pub(super) rel_id_alloc: Arc<AtomicU64>,
    /// Owned workspace of entities this statement created. Released on drop.
    pub(super) workspace: relate_core::tokens::WorkspaceCharge,
}

impl Clone for WriteScratch {
    fn clone(&self) -> Self {
        let mut cloned = Self {
            batch: self.batch.clone(),
            statement: self.statement.clone(),
            redo: self.redo.clone(),
            node_id_alloc: Arc::clone(&self.node_id_alloc),
            rel_id_alloc: Arc::clone(&self.rel_id_alloc),
            workspace: relate_core::tokens::WorkspaceCharge::empty(),
        };
        // One walk, and only while a ledger is installed. Later writes add
        // a delta on top of this total.
        cloned.sync_workspace_best_effort();
        cloned
    }
}

impl WriteScratch {
    pub fn new(
        snapshot_ts: u64,
        node_id_alloc: Arc<AtomicU64>,
        rel_id_alloc: Arc<AtomicU64>,
    ) -> Self {
        Self::with_statement(
            snapshot_ts,
            StatementState::default(),
            node_id_alloc,
            rel_id_alloc,
        )
    }

    /// Build a scratch at `snapshot_ts` carrying `statement` — the one
    /// constructor [`Self::new`] and [`Self::rebase`] share, so the rebase
    /// survivor set is exactly the [`StatementState`] handed over here.
    fn with_statement(
        snapshot_ts: u64,
        statement: StatementState,
        node_id_alloc: Arc<AtomicU64>,
        rel_id_alloc: Arc<AtomicU64>,
    ) -> Self {
        Self {
            batch: PerBatchState {
                snapshot_ts,
                nodes: HashMap::new(),
                prop_ix: ScratchPropIndex::default(),
                rels: HashMap::new(),
                rel_prop_ix: ScratchPropIndex::default(),
                deleted_nodes: HashSet::new(),
                deleted_rels: HashSet::new(),
                created_nodes: HashSet::new(),
                created_rels: HashSet::new(),
                out_adj: HashMap::new(),
                in_adj: HashMap::new(),
                label_probe: LabelProbe::new(snapshot_ts),
                live_label_delta: HashMap::new(),
            },
            statement,
            redo: RedoLog::default(),
            node_id_alloc,
            rel_id_alloc,
            workspace: relate_core::tokens::WorkspaceCharge::empty(),
        }
    }

    /// Move the workspace reservation onto the created entities currently
    /// in this scratch. A refusal leaves the previous reservation and sets
    /// the ledger's sticky error.
    fn sync_workspace(&mut self) -> Result<()> {
        let nodes = &self.batch.nodes;
        let rels = &self.batch.rels;
        let created_nodes = &self.batch.created_nodes;
        let created_rels = &self.batch.created_rels;
        let out_adj = &self.batch.out_adj;
        let in_adj = &self.batch.in_adj;
        let redo_bytes = self.redo.reserved_bytes();
        self.workspace.resize_with(|| {
            crate::workspace::scratch_created_bytes(
                nodes,
                rels,
                created_nodes,
                created_rels,
                out_adj,
                in_adj,
                redo_bytes,
            )
        })?;
        relate_core::tokens::peak_hold("create");
        Ok(())
    }

    /// The post-mutation workspace sync, best-effort (ADR-0042's token
    /// ledger): the charge resize for state already installed — a
    /// clone's created set, a restored checkpoint's entity set. A live
    /// ledger's admission refusal is deliberately not propagated: the
    /// state it would charge for is already applied, so failing the call
    /// would strand half-installed state; the refusal instead sets the
    /// ledger's sticky error, and the next `tokens::poll()` (the
    /// engine's per-clause checkpoint) surfaces it as the typed capacity
    /// error. The one named home of that deferral. The pre-insert
    /// reservations (`reserve_new_node` / `reserve_new_rel`) are the
    /// propagating contrast: nothing is applied yet there, so they fail
    /// closed.
    fn sync_workspace_best_effort(&mut self) {
        let _ = self.sync_workspace();
    }

    /// Add `after - before` to the running workspace total. Best-effort,
    /// like every post-mutation charge (see
    /// [`Self::sync_workspace_best_effort`]): the op is already in the
    /// redo log, so a refusal defers to the ledger's sticky error; the
    /// high-water mark records only the accepted resize. The
    /// admission-off gate is the charge API's own
    /// [`resize_with`](relate_core::tokens::WorkspaceCharge::resize_with)
    /// — the one home of that early exit, like every charge here.
    pub(super) fn shift_workspace(&mut self, before: u64, after: u64) {
        if before == after {
            return;
        }
        let base = self.workspace.bytes();
        let next = if after > before {
            base.saturating_add(after - before)
        } else {
            base.saturating_sub(before - after)
        };
        if self.workspace.resize_with(|| next).is_ok() {
            relate_core::tokens::peak_hold("create");
        }
    }

    /// Reserve the workspace `node` will take before it is inserted. The
    /// admission-off gate is the charge API's own `resize_with`, the one
    /// home of that early exit.
    pub(super) fn reserve_new_node(&mut self, node: &Node) -> Result<()> {
        let before = self.workspace.bytes();
        let delta =
            crate::workspace::created_node_insert_delta(self.batch.created_nodes.len(), node);
        self.workspace
            .resize_with(|| before.saturating_add(delta))?;
        relate_core::tokens::peak_hold("create");
        Ok(())
    }

    /// Reserve the workspace `rel` will take, including its adjacency slot,
    /// before it is inserted. The admission-off gate is the charge API's
    /// own `resize_with`, the one home of that early exit.
    pub(super) fn reserve_new_rel(&mut self, rel: &Relationship) -> Result<()> {
        let before = self.workspace.bytes();
        let delta = crate::workspace::created_rel_insert_delta(
            self.batch.created_rels.len(),
            rel,
            self.batch.out_adj.get(&rel.start).map(Vec::len),
            self.batch.in_adj.get(&rel.end).map(Vec::len),
        );
        self.workspace
            .resize_with(|| before.saturating_add(delta))?;
        relate_core::tokens::peak_hold("create");
        Ok(())
    }

    /// True when this overlay created, updated, or deleted a node, so a
    /// committed range-index cardinality no longer describes the view.
    pub(crate) fn changes_node_membership(&self) -> bool {
        !self.batch.nodes.is_empty() || !self.batch.deleted_nodes.is_empty()
    }

    pub(super) fn live_label_delta(&self, label: &str) -> i64 {
        i64::from(self.batch.live_label_delta.get(label).copied().unwrap_or(0))
    }

    /// Net one `label_deltas` outcome into the statement's `+labels` /
    /// `-labels` counters (ADR-0171): each possibly-negative delta is added
    /// saturating and the counter clamps at zero, so a statement never
    /// reports a net-negative count for its own changes. The one home of
    /// the signed label-stat netting every write verb shares.
    pub(super) fn net_label_stats(&mut self, labels_added: i64, labels_removed: i64) {
        self.statement.stats.labels_added = self
            .statement
            .stats
            .labels_added
            .saturating_add(labels_added)
            .max(0);
        self.statement.stats.labels_removed = self
            .statement
            .stats
            .labels_removed
            .saturating_add(labels_removed)
            .max(0);
    }

    pub fn snapshot_ts(&self) -> u64 {
        self.batch.snapshot_ts
    }

    /// Whether this scratch has logged no change yet: every mutation
    /// appends its redo op, so an empty log means the view reads exactly
    /// the base's pinned state (the count short-circuit's exactness
    /// predicate, ADR-0689).
    pub(super) fn has_no_changes(&self) -> bool {
        self.redo.len() == 0
    }

    /// Per-batch rebase (ADR-0210): reset the per-batch state (redo log,
    /// entity maps, prop indexes, adjacency, deleted/created sets, label
    /// probe, live-label delta) at the new snapshot timestamp, keeping the
    /// statement-spanning [`StatementState`] — the accumulated `stats` and the
    /// applied-change count — which moves over as one unit. Its created-id
    /// sets are the one exception, cleared here with the batch's own created
    /// sets: an id nets a create only inside the batch that counted it
    /// (REL8-1188), so a carried id would only grow the sets past the bytes
    /// the workspace charge measures. The survivor set is decided here, next
    /// to the fields, not at the `commit_batch` call site.
    pub(crate) fn rebase(&mut self, new_ts: u64) {
        let mut statement = std::mem::take(&mut self.statement);
        statement.created_nodes.clear();
        statement.created_rels.clear();
        let node_id_alloc = Arc::clone(&self.node_id_alloc);
        let rel_id_alloc = Arc::clone(&self.rel_id_alloc);
        *self = Self::with_statement(new_ts, statement, node_id_alloc, rel_id_alloc);
    }

    pub fn take_redo(&mut self) -> Vec<RedoOp> {
        self.redo.take()
    }

    /// Drain the redo and hand its live workspace reservation to the caller.
    ///
    /// The scratch's execution allocation is still represented by the redo
    /// vector while WAL commit serializes and applies it. The caller holds
    /// this charge through that boundary, then drops it once the commit has
    /// either been rejected or acknowledged.
    pub fn take_redo_with_workspace(&mut self) -> Result<(Vec<RedoOp>, WorkspaceCharge)> {
        // `RedoLog::take` converts its compact live entries into a larger
        // final `Vec<RedoOp>`. Reserve that vector before allocating it; the
        // compact source buffer remains live during `collect`, so both
        // allocations are covered at the conversion peak.
        let final_redo_bytes = (self.redo.len() as u64).saturating_mul(size_of::<RedoOp>() as u64);
        self.workspace
            .resize(self.workspace.bytes().saturating_add(final_redo_bytes))?;
        let workspace = std::mem::replace(&mut self.workspace, WorkspaceCharge::empty());
        Ok((self.redo.take(), workspace))
    }

    /// Borrow the accumulated redo without draining it (see
    /// [`RedoLog::ops`]); ADR-0210 batch commits hand the sink a clone
    /// and drain only after the commit succeeds.
    pub(crate) fn redo_ops(&self) -> Vec<RedoOp> {
        self.redo.materialize()
    }

    /// The redo ops at or after `from`: what the statements run since that
    /// position appended. `None` when `from` is past the end — the log
    /// shrank under the caller's cursor, which the caller refuses rather
    /// than reading as nothing. Reads without draining, so a caller
    /// bounding the transaction's prospective commit measures the same ops
    /// that commit will.
    pub fn redo_since(&self, from: usize) -> Option<Vec<RedoOp>> {
        self.redo.ops_since(from)
    }

    /// The one funnel every write goes through: post the op to the lazy
    /// equality indexes, account the ADR-0309 update counters, then append
    /// it to the redo log.
    pub fn push_redo(&mut self, op: RedoOp) {
        self.post_to_indexes(&op);
        self.account_update_counters(&op);
        let before = self.redo.reserved_bytes();
        self.redo.push(op);
        let after = self.redo.reserved_bytes();
        self.shift_workspace(before, after);
        // The one funnel every write posts through also counts the applied
        // changes the ADR-0402 effect reads — entity, schema and catalog
        // writes alike, so a catalog-only statement is not "no effect".
        self.statement.applied_ops += 1;
    }

    /// How many changes the current statement has applied (ADR-0402
    /// Decision 2): zero from the statement entry reset and one per change
    /// through the redo funnel, across ADR-0210 batch drains too.
    pub(crate) fn applied_ops(&self) -> u64 {
        self.statement.applied_ops
    }

    /// The statement-entry reset of the statement-spanning state: the
    /// ADR-0309 mutation counters and the ADR-0402 applied-change count
    /// restart together, as one unit, because both describe one statement
    /// and both survive a batch rebase together. A caller reusing a scratch
    /// across statements (an explicit transaction, the executor's own tests)
    /// therefore reads this statement's facts only.
    pub(crate) fn reset_statement(&mut self) {
        self.statement = StatementState::default();
    }

    /// The ADR-0309 update counters, derived from the redo op at the one
    /// funnel every write goes through: `node_labels_added`/
    /// `node_labels_removed` count labels per node and `properties_written`
    /// counts one per property write. Deriving them from the op keeps the
    /// two counter vocabularies in one accounting site, so a write verb
    /// cannot bump one and forget the other. The same match records which
    /// entities were created — the statement-local set ([`StatementState::
    /// created_nodes`] / `created_rels`) and its transaction-spanning batch
    /// twin ([`PerBatchState::created_nodes`] / `created_rels`) in the same
    /// arm, so the two registration sites cannot drift — the fact the delete
    /// verbs' net-or-count decision reads (REL8-1188).
    fn account_update_counters(&mut self, op: &RedoOp) {
        match op {
            RedoOp::CreateNode {
                id,
                labels,
                properties,
                ..
            } => {
                self.statement.stats.node_labels_added += distinct_labels(labels).len() as i64;
                self.statement.stats.properties_written += properties.len() as i64;
                self.statement.created_nodes.insert(*id);
                self.batch.created_nodes.insert(*id);
            }
            RedoOp::CreateRel { id, properties, .. } => {
                self.statement.stats.properties_written += properties.len() as i64;
                self.statement.created_rels.insert(*id);
                self.batch.created_rels.insert(*id);
            }
            RedoOp::AddLabels { labels, .. } => {
                self.statement.stats.node_labels_added += distinct_labels(labels).len() as i64;
            }
            RedoOp::RemoveLabels { labels, .. } => {
                self.statement.stats.node_labels_removed += distinct_labels(labels).len() as i64;
            }
            // One write per pushed property op: `set_prop_core` pushes a
            // Set op for a non-null value and a Remove op for a null over
            // an existing key; either is one property write (ADR-0309).
            RedoOp::SetNodeProp { .. }
            | RedoOp::RemoveNodeProp { .. }
            | RedoOp::SetRelProp { .. }
            | RedoOp::RemoveRelProp { .. } => {
                self.statement.stats.properties_written += 1;
            }
            // Deletions count no property write or label change (ADR-0309);
            // the catalog ops carry their own counters
            // (`overlay/catalog.rs`) and the store-level ops touch no
            // entity.
            RedoOp::DeleteNode { .. }
            | RedoOp::DeleteRel { .. }
            | RedoOp::CreateIndex { .. }
            | RedoOp::DropIndex { .. }
            | RedoOp::CreateConstraint { .. }
            | RedoOp::DropConstraint { .. }
            | RedoOp::CatalogIndexDetail { .. }
            | RedoOp::CatalogDerivedConstraint { .. }
            | RedoOp::PutDatabase { .. }
            | RedoOp::DropDatabase { .. }
            | RedoOp::PutGraphType { .. }
            | RedoOp::SetCurrentGraphType { .. }
            | RedoOp::PullIdentityPut { .. }
            | RedoOp::OntologySet { .. }
            | RedoOp::OntologyDrop { .. } => {}
        }
    }

    /// The scratch copy of node `id`. Every `push_redo` producer installs
    /// into `nodes` before pushing, so a miss is internal-state corruption.
    fn scratch_node(nodes: &HashMap<NodeId, Node>, id: NodeId) -> &Node {
        nodes
            .get(&id)
            .expect("producer installs into the scratch before push_redo")
    }

    /// The scratch copy of relationship `id`; same invariant as
    /// [`Self::scratch_node`].
    fn scratch_rel(rels: &HashMap<RelId, Relationship>, id: RelId) -> &Relationship {
        rels.get(&id)
            .expect("producer installs into the scratch before push_redo")
    }

    /// Keep `prop_ix` and `rel_prop_ix` current from the redo funnel
    /// (ADR-0173, ADR-0185): post the `(label, value)` and
    /// `(rel type, value)` pairings each op introduces. Every arm sources
    /// from the scratch maps through [`Self::scratch_node`] /
    /// [`Self::scratch_rel`]. Exhaustive, so a new op must decide here.
    /// Removals post nothing: both indexes are additions-only and every
    /// seek re-verifies its ids. A null value posts nothing too — that
    /// policy is the index API's own ([`ScratchPropIndex::post_value`]'s
    /// shared core), not re-spelled per arm here.
    fn post_to_indexes(&mut self, op: &RedoOp) {
        match op {
            RedoOp::CreateNode { id, .. } => {
                let n = Self::scratch_node(&self.batch.nodes, *id);
                for (key, value) in &*n.properties {
                    self.batch.prop_ix.post_value(*id, &n.labels, key, value);
                }
            }
            RedoOp::CreateRel { id, .. } => {
                let r = Self::scratch_rel(&self.batch.rels, *id);
                for (key, value) in &*r.properties {
                    self.batch
                        .rel_prop_ix
                        .post_value(*id, &r.rel_type, key, value);
                }
            }
            RedoOp::SetNodeProp { id, key, value } => {
                let n = Self::scratch_node(&self.batch.nodes, *id);
                self.batch.prop_ix.post_value(*id, &n.labels, key, value);
            }
            RedoOp::AddLabels { id, labels } => {
                let n = Self::scratch_node(&self.batch.nodes, *id);
                self.batch.prop_ix.post_labels(*id, labels, &n.properties);
            }
            RedoOp::SetRelProp { id, key, value } => {
                let r = Self::scratch_rel(&self.batch.rels, *id);
                self.batch
                    .rel_prop_ix
                    .post_value(*id, &r.rel_type, key, value);
            }
            RedoOp::DeleteNode { .. }
            | RedoOp::DeleteRel { .. }
            | RedoOp::RemoveNodeProp { .. }
            | RedoOp::RemoveRelProp { .. }
            | RedoOp::RemoveLabels { .. }
            | RedoOp::CreateIndex { .. }
            | RedoOp::DropIndex { .. }
            | RedoOp::CreateConstraint { .. }
            | RedoOp::DropConstraint { .. }
            | RedoOp::CatalogIndexDetail { .. }
            | RedoOp::CatalogDerivedConstraint { .. }
            | RedoOp::PutDatabase { .. }
            | RedoOp::DropDatabase { .. }
            | RedoOp::PutGraphType { .. }
            | RedoOp::SetCurrentGraphType { .. }
            | RedoOp::PullIdentityPut { .. }
            | RedoOp::OntologySet { .. }
            | RedoOp::OntologyDrop { .. } => {}
        }
    }

    pub(super) fn alloc_node(&self) -> Result<NodeId> {
        crate::graph::alloc_exhaustible_id(self.node_id_alloc.as_ref(), "node")
    }

    pub(super) fn alloc_rel(&self) -> Result<RelId> {
        crate::graph::alloc_exhaustible_id(self.rel_id_alloc.as_ref(), "relationship")
    }

    /// Pre-batch state for `CALL { … } IN TRANSACTIONS`. The rollback image is
    /// the whole scratch state as of here, in three parts: the per-batch maps
    /// (the one embedded [`PerBatchState`] capture), the statement-spanning
    /// [`StatementState`] (its ADR-0309 counters **and** its applied-change
    /// count, so a rolled-back batch restores both), and the redo log
    /// positionally — a cursor, not a second op vec — so per-batch capture is
    /// O(1) in the accumulated ops (a by-value copy would be quadratic on the
    /// no-sink path, where each batch re-captures over an ever-growing log).
    /// Id allocators stay on the live scratch so a rolled-back batch does not
    /// hand the same id out again; they are not part of the image.
    ///
    /// The pipelined write batch does not use this: its per-statement rollback
    /// is [`Self::mark_statement`], which the batch's CREATE-only eligibility
    /// rule makes exact at O(statement) instead of O(batch).
    pub fn checkpoint(&self) -> ScratchCheckpoint {
        ScratchCheckpoint {
            batch: self.batch.clone(),
            statement: self.statement.clone(),
            redo: self.redo.cursor(),
        }
    }

    /// Roll back to a captured [`Self::checkpoint`]: the per-batch state and
    /// the statement-spanning state restores as one, and the redo log truncates
    /// to its captured position (the live log's ops stay with the live log).
    /// Because the applied-change count rides the restored [`StatementState`],
    /// a batch that rolled back applied nothing — a statement whose every batch
    /// failed is still a zero-effect write (ADR-0402 Decision 2) — and the
    /// restore never has to derive that from how many ops the truncation
    /// dropped.
    pub fn restore(&mut self, checkpoint: ScratchCheckpoint) {
        self.batch = checkpoint.batch;
        self.statement = checkpoint.statement;
        self.redo.restore_cursor(checkpoint.redo);
        self.sync_workspace_best_effort();
    }

    /// Pre-statement mark for the pipelined write batch (ADR-0642): the redo
    /// cursor and the statement-spanning state as of the statement's entry.
    /// Cheap by construction — O(statement), never a copy of the batch image —
    /// because the batch admits only CREATE-only statements, so the entries a
    /// failed statement can have added are exactly the ids its own
    /// [`StatementState::created_nodes`] / `created_rels` recorded at the redo
    /// funnel.
    pub fn mark_statement(&self) -> StatementMark {
        StatementMark {
            redo: self.redo.cursor(),
            statement: self.statement.clone(),
        }
    }

    /// Roll back to a [`Self::mark_statement`]: drop the entities the failed
    /// statement created, truncate the redo to the mark's cursor and put the
    /// statement-spanning state back. The batch's other entries — the earlier
    /// statements' rows — are untouched, so a failing batched statement loses
    /// only its own writes.
    ///
    /// The lazy equality indexes keep their postings for the dropped ids: they
    /// are additions-only supersets every seek re-verifies on its view
    /// (ADR-0172 §3), which is the same state a delete leaves behind.
    ///
    /// A redo this mark cannot invert is refused, not half-applied: the
    /// statement's entries stay where they are and the caller fails the batch,
    /// so a rollback that could not finish never leaves partial writes to be
    /// committed with the batch — the durability lie the batch's whole
    /// failure contract exists to avoid.
    pub fn rollback_statement(&mut self, mark: StatementMark) -> Result<()> {
        // The batch admits CREATE-only statements, so the only non-create ops
        // a statement appends are the property defaults and label changes the
        // engine makes for the entities it just created — and dropping those
        // entities undoes those ops with them. Anything else is a redo this
        // mark cannot invert.
        let created: HashSet<EntityId> = self
            .statement
            .created_nodes
            .iter()
            .map(|id| EntityId::Node(*id))
            .chain(
                self.statement
                    .created_rels
                    .iter()
                    .map(|id| EntityId::Rel(*id)),
            )
            .collect();
        for op in self.redo.ops_after(&mark.redo) {
            if !invertible_by_dropping_creates(&op, &created) {
                return Err(relate_core::Error::storage(
                    "a batched statement's redo holds an op its rollback cannot invert",
                ));
            }
        }
        let created_rels = std::mem::take(&mut self.statement.created_rels);
        let created_nodes = std::mem::take(&mut self.statement.created_nodes);
        // The bulk adjacency pass before the per-entity undo: one retain per
        // map, never a scan per dropped id.
        self.batch.drop_adjacency(&created_rels);
        for id in created_rels {
            self.batch.uncreate_rel(id);
        }
        for id in created_nodes {
            self.batch.uncreate_node(id);
        }
        self.redo.restore_cursor(mark.redo);
        self.statement = mark.statement;
        self.sync_workspace_best_effort();
        Ok(())
    }
}

/// Whether dropping the statement's created entities undoes `op`: a create is
/// the entity itself, and a property or label op is undone by the same drop
/// when it names one of them — which is the only target a CREATE-only
/// statement's defaults can reach.
pub(super) fn invertible_by_dropping_creates(op: &RedoOp, created: &HashSet<EntityId>) -> bool {
    match op {
        RedoOp::CreateNode { .. } | RedoOp::CreateRel { .. } => true,
        RedoOp::SetNodeProp { id, .. }
        | RedoOp::RemoveNodeProp { id, .. }
        | RedoOp::AddLabels { id, .. }
        | RedoOp::RemoveLabels { id, .. } => created.contains(&EntityId::Node(*id)),
        RedoOp::SetRelProp { id, .. } | RedoOp::RemoveRelProp { id, .. } => {
            created.contains(&EntityId::Rel(*id))
        }
        _ => false,
    }
}

/// The pipelined write batch's pre-statement rollback image
/// ([`WriteScratch::mark_statement`]): the redo cursor and the
/// statement-spanning state as of the statement's entry. Opaque to the facade,
/// which marks once per batched statement and rolls back when it fails.
pub struct StatementMark {
    redo: super::redo_log::RedoCursor,
    statement: StatementState,
}

/// Rollback image of a [`WriteScratch`]: its two state halves — the per-batch
/// state (via the one embedded capture) and the statement-spanning state
/// ([`StatementState`]: counters and applied count as of the capture) — plus
/// the redo log's positional cursor. Cloning the maps is shallow once node and
/// relationship payloads are `Arc`s; the redo ops stay with the live log, and
/// the id allocators are not captured (they never roll back).
///
/// Opaque to the facade, which captures one per `CALL { … } IN TRANSACTIONS`
/// batch and restores it when that batch fails. Its clone is O(batch), so it
/// is a per-batch image — never a per-statement one ([`StatementMark`]).
pub struct ScratchCheckpoint {
    batch: PerBatchState,
    statement: StatementState,
    redo: super::redo_log::RedoCursor,
}
