//! Private write overlay for optimistic execute (ADR-0016).

use std::cell::{Cell, RefCell};
use std::collections::HashSet;

use relate_core::{CancelToken, Namespace, NodeId, RelId, Result, Value};

use crate::csv::CsvReader;
use crate::graph::{Graph, Procedure, QueryStats, SchemaSnapshot};

mod budget;
mod idlists;

pub use budget::NESTED_EVAL_BUDGET;
pub(crate) use budget::{ExpressionBudgetCounter, NestedEvalCounter, RetainedTotal};
pub(crate) use idlists::{intersect_ids, union_ids};

/// The distinct labels of `labels`, in first-occurrence order — the one
/// dedup every label-accounting walk rides (live-label bumps, net
/// label-delta accounting, per-node counter accounting). Shared by the
/// scratch (its label accounting) and the write verbs (`writes.rs`).
fn distinct_labels(labels: &[String]) -> Vec<&str> {
    let mut seen = HashSet::new();
    labels
        .iter()
        .map(String::as_str)
        .filter(|label| seen.insert(*label))
        .collect()
}

/// Read view of committed graph plus optional overlay (ADR-0016).
pub struct ExecGraph<'a> {
    base: &'a Graph,
    schema: Option<&'a SchemaSnapshot>,
    scratch: Option<&'a RefCell<WriteScratch>>,
    pinned_ts: u64,
    /// Cooperative cancellation flag for this statement (ADR-0111). `None`
    /// for views that do not participate in cancellation (tests, diagnostics,
    /// and the internal write views that never outlive a checked statement).
    cancel: Option<&'a CancelToken>,
    /// Statement-wide cancellation sampling window (ADR-0111). One counter
    /// shared by every hot loop in the statement — scans, folds, and the
    /// recursive var-length walks — so recursion depth cannot reset the
    /// window and hide a cancellation. A `Cell` suffices: a statement's
    /// executor access is single-threaded (the query worker thread), and the
    /// `&RefCell<WriteScratch>` overlay already makes this view `!Sync`.
    cancel_ticks: Cell<usize>,
    /// The per-statement nested-evaluation counter (ADR-0395): one shared
    /// cell per top-level statement. The view owns it beside
    /// `cancel_ticks` — every nested entry ticks [`Self::nested_evals`],
    /// so nesting compounds on one counter and no nested context can
    /// reset it.
    nested_evals: NestedEvalCounter,
    /// The per-statement expression allocation meter (ADR-0463): one
    /// shared counter per top-level statement, reset at statement entry
    /// beside the nested-evaluation counter. Every bounded expression
    /// allocation charges here before it grows, so a single statement
    /// cannot build an arbitrarily large value through `range`,
    /// concatenation, comprehensions, accumulators, or the
    /// collection/string builtins.
    expression_budget: ExpressionBudgetCounter,
    /// The per-allocation expression cap this view enforces (ADR-0463):
    /// inherited from the store's configured limit. `None` is the explicit
    /// opt-out; otherwise one built value over this many bytes is refused.
    expression_limit: Option<u64>,
    /// The store's ontology document as the calling transaction sees it
    /// (ADR-0121 §3, ADR-0227): per-admission state attached like the
    /// cancel token ([`Self::with_ontology_document`]), read by the
    /// `db.ontology.get` / `db.ontology.apply` procedure arms.
    /// `None` = no ontology set (also every test/diagnostic view).
    ontology_document: Option<&'a str>,
    /// The per-statement label-interning memo (LABS-1116): a label check
    /// pays the interner's string-keyed lookup once per distinct label
    /// per statement, not once per candidate — the per-row candidates
    /// path's twin of the prepared programs' `InternedLabels`
    /// (ADR-0473). Keys are the statement's own label strings —
    /// client-controlled, so the seeded policy ([`SeededMap`],
    /// ADR-0355), never FxHash. Interning is append-only, so a cached
    /// `Some(id)` can never go stale; a cached `None` can —
    /// `commit_batch` (ADR-0210's `CALL { … } IN TRANSACTIONS`) lands a
    /// batch's writes and their labels in the base mid-statement, so it
    /// clears the memo: the one seam where statement writes enter the
    /// base while the statement runs.
    label_ids: RefCell<crate::hash_seed::SeededMap<Box<str>, Option<u32>>>,
    pub(crate) csv: CsvReader<'a>,
}

/// Sampling mask for cooperative cancellation checks on hot per-row loops
/// (ADR-0111). `ticks & CANCEL_SAMPLE_MASK == 0` checks the token roughly
/// once per 1024 iterations, so the atomic load is not paid on every row.
const CANCEL_SAMPLE_MASK: usize = 1023;

impl<'a> ExecGraph<'a> {
    /// Schema-less read view of the committed graph. `show_schema` falls back
    /// to the live committed theater (under its lock); use
    /// [`Self::with_schema`] when a pinned admission snapshot is
    /// required. This is the constructor for tests and callers that never
    /// consult committed SHOW metadata.
    pub fn read(base: &'a Graph) -> Self {
        Self::read_at(base, base.commit_ts())
    }
    /// The base graph this snapshot reads — estimate-time consumers
    /// (the live-graph stats, LABS-351) need the base's index counts.
    pub(crate) fn base(&self) -> &'a Graph {
        self.base
    }

    /// The count-only statistics short-circuit's exactness precondition
    /// (LABS-370 v1 follow-up): this view reads the base's latest
    /// published state with nothing the base stats cannot see — no
    /// scratch overlay (an explicit transaction's or write statement's
    /// own writes live there and the base stats cannot see them), no
    /// in-flight base write window (`apply_redo` bumps the stats before
    /// it publishes), and no pinned older snapshot (a concurrent apply
    /// advances the base stats past an older pin). Any other view
    /// declines the short-circuit to the scan, which reads its own
    /// writes. The predicate is checked twice (LABS-370 round 3, F-1-5):
    /// once up front as a cheap filter, then again under BOTH stats-map
    /// locks by the reader's consistency window
    /// (`Graph::windowed_stats_count`) — a write that opens between the
    /// two checks is closed by the under-lock re-check, so no
    /// out-of-snapshot write leaks into the count.
    pub(crate) fn sees_exact_base_stats(&self) -> bool {
        self.scratch.is_none()
            && !self.base.in_write_window()
            && self.pinned_ts == self.base.commit_ts()
    }

    /// The store's `RELATE_PREVIEW`-resolved set (ADR-0371), the
    /// executor-side gate the graph-type semantic layer reads for the
    /// stored-state half of the graph-type trio's gates.
    pub(crate) fn preview(&self) -> relate_core::PreviewFeatures {
        self.base.preview()
    }

    /// The scratch-aware property-defaults gate (CIP-262, ADR-0388
    /// review F-2-2): the committed theater's cheap answer
    /// (`Graph::has_property_defaults`), or a `SetCurrentGraphType` in
    /// this statement's unvalidated redo slice whose body could declare
    /// or inherit a default (round 5 deleted the `PutGraphType`
    /// alternative -- the only op that can change the current type). The one yes/no answer
    /// over both vintages, so the committed-theater half can never miss
    /// a default this very statement set; the statement boundary's
    /// default application consults the same gate.
    pub(crate) fn has_property_defaults(&self) -> bool {
        if self.base.has_property_defaults() {
            return true;
        }
        self.unvalidated_redo(|ops| {
            ops.iter().any(|op| match op {
                // SetCurrentGraphType is the only op that can change the
                // current graph type property defaults apply from (the
                // round-5 finding: the PutGraphType alternative never
                // changes this gate's decision).
                crate::redo::RedoOp::SetCurrentGraphType { body } => {
                    crate::graph::spec_has_property_defaults(body)
                }
                _ => false,
            })
        })
        .0
    }

    /// `read` at an explicit snapshot timestamp; same schema-less invariant.
    pub fn read_at(base: &'a Graph, snapshot_ts: u64) -> Self {
        Self::new_view(base, snapshot_ts, None)
    }

    /// Pin a schema snapshot on a view (combinator mirroring
    /// [`Self::with_cancel`]): `ExecGraph::read_at(base, ts).with_schema(schema)`.
    pub fn with_schema(mut self, schema: &'a SchemaSnapshot) -> Self {
        self.schema = Some(schema);
        self
    }

    /// Schema-less write view of the committed graph plus overlay.
    /// `show_schema`/`validate_constraints` fall back to the live committed
    /// theater, reached in two regimes: the autocommit path reaches it
    /// while holding the publication read guard, and ADR-0210 batched
    /// statements (`CALL { … } IN TRANSACTIONS`) reach it unguarded —
    /// pinned on the graph version but racing a concurrent schema change.
    /// A caller that needs a pinned admission adds it with
    /// [`Self::with_schema`].
    pub fn write(base: &'a Graph, scratch: &'a RefCell<WriteScratch>) -> Self {
        let pinned_ts = scratch.borrow().snapshot_ts();
        Self::new_view(base, pinned_ts, Some(scratch))
    }

    /// The constructor core the two view kinds share: every field but the
    /// pin and the overlay starts identically (schema-less, cancellation
    /// unattached, nesting and ontology unset), so a new field joins every
    /// view by construction instead of being hand-synced across the
    /// `read_at`/`write` literals.
    fn new_view(
        base: &'a Graph,
        pinned_ts: u64,
        scratch: Option<&'a RefCell<WriteScratch>>,
    ) -> Self {
        Self {
            base,
            schema: None,
            scratch,
            pinned_ts,
            cancel: None,
            cancel_ticks: Cell::new(0),
            nested_evals: NestedEvalCounter::default(),
            expression_budget: ExpressionBudgetCounter::default(),
            expression_limit: base.expression_memory_limit(),
            ontology_document: None,
            // The process-wide seeded state (LABS-822's rule): no view
            // construction pays a fresh `RandomState` read.
            label_ids: RefCell::new(std::collections::HashMap::with_hasher(
                crate::hash_seed::shared_random_state(),
            )),
            csv: CsvReader::new(base.csv_policy()),
        }
    }

    /// Which fixed namespace of the store the underlying graph is
    /// (ADR-0116 §1). Used for namespace-qualified element ids (§7).
    pub(crate) fn namespace(&self) -> Namespace {
        self.base.namespace()
    }

    /// Attach a cooperative cancellation flag to this view (ADR-0111).
    /// Construction sites that do not thread a token leave the view
    /// uncancellable, which preserves the existing behavior.
    pub fn with_cancel(mut self, cancel: Option<&'a CancelToken>) -> Self {
        self.cancel = cancel;
        self
    }

    /// Attach the ontology document the calling transaction sees
    /// (ADR-0121 §3, ADR-0227): the facade's admission-time capture for
    /// auto-commit statements, the begin-time capture for explicit
    /// transactions. Per-admission state on the view, like
    /// [`Self::with_cancel`] (ADR-0111); the `db.ontology.get` /
    /// `db.ontology.apply` procedure arms read it through
    /// [`Self::ontology_document`]. A view without one behaves as if the
    /// store holds no ontology, which preserves test/diagnostic behavior.
    pub fn with_ontology_document(mut self, document: Option<&'a str>) -> Self {
        self.ontology_document = document;
        self
    }

    /// The ontology document attached by [`Self::with_ontology_document`].
    pub(crate) fn ontology_document(&self) -> Option<&'a str> {
        self.ontology_document
    }

    /// Check the attached [`CancelToken`], returning
    /// `Error::Cancelled` (`query cancelled`) once it is cancelled. Called at
    /// row-production points and inside long loops; a view without a token is
    /// never cancelled. See ADR-0111.
    #[inline]
    pub(crate) fn check_cancelled(&self) -> Result<()> {
        match self.cancel {
            Some(token) if token.is_cancelled() => Err(relate_core::Error::cancelled()),
            _ => Ok(()),
        }
    }

    /// Sample one hot-loop iteration against the statement-wide cancellation
    /// window (ADR-0111).
    ///
    /// This is the single home of the sampling policy: cancellation is
    /// checked at statement entry, at the top of every clause, and — via
    /// this method — inside sampled hot loops, once every
    /// [`CANCEL_SAMPLE_MASK`] + 1 ticks. The counter lives on the statement's
    /// view, not on the call site, so recursive walks (var-length expansion
    /// in `exec/expand.rs`, `eval_walk/pattern.rs`, `exec/gpm.rs`) accumulate
    /// ticks across frames: a fresh per-frame window would let a
    /// bounded-degree `()-[*1..n]->()` walk explore combinatorially many
    /// frames that each stay under the window and never observe
    /// cancellation. The counter wraps so an unbounded loop is safe; every
    /// `CANCEL_SAMPLE_MASK + 1`th tick calls [`Self::check_cancelled`],
    /// which surfaces the typed `Error::Cancelled` (`query cancelled`) once
    /// the token is cancelled.
    #[inline]
    pub(crate) fn cancel_tick(&self) -> Result<()> {
        relate_core::tokens::poll()?;
        let ticks = self.cancel_ticks.get().wrapping_add(1);
        self.cancel_ticks.set(ticks);
        if ticks & CANCEL_SAMPLE_MASK == 0 {
            self.check_cancelled()?;
        }
        Ok(())
    }

    /// The per-statement nested-evaluation counter (ADR-0395): the one
    /// cell every nested entry ticks — the seeded statement engine and
    /// the direct clause-engine paths alike — so all nesting compounds
    /// on the statement's single budget.
    pub(crate) fn nested_evals(&self) -> &NestedEvalCounter {
        &self.nested_evals
    }

    /// Overlay scratch adj is unordered (ADR-0031 / ADR-0035).
    pub(crate) fn has_write_overlay(&self) -> bool {
        self.scratch.is_some()
    }

    pub(crate) fn overlay_changes_node_membership(&self) -> bool {
        self.scratch
            .is_some_and(|cell| cell.borrow().changes_node_membership())
    }

    fn snapshot_ts(&self) -> u64 {
        self.scratch
            .map(|s| s.borrow().snapshot_ts())
            .unwrap_or(self.pinned_ts)
    }

    fn scratch_mut(&self) -> std::cell::RefMut<'_, WriteScratch> {
        self.scratch
            .expect("write on read-only ExecGraph")
            .borrow_mut()
    }

    /// The one per-statement reset of the statement's own facts: the
    /// ADR-0309 mutation counters and the ADR-0402 applied-change count
    /// restart together at statement entry, so a view reused across
    /// statements reports this statement's counters and its own applied count
    /// — never the previous statement's. A read view carries no scratch and
    /// resets only the per-statement label memo.
    pub(crate) fn reset_statement(&self) {
        self.label_ids.borrow_mut().clear();
        if let Some(scratch) = self.scratch {
            scratch.borrow_mut().reset_statement();
        }
    }

    /// The interned id of `label`, memoized per statement (LABS-1116) —
    /// the one interning lookup a statement's label checks pay per
    /// distinct label, instead of one per candidate. A cached `None` is
    /// valid only until a mid-statement batch commit; see the
    /// `label_ids` field.
    pub(crate) fn memo_label_id(&self, label: &str) -> Option<u32> {
        if let Some(id) = self.label_ids.borrow().get(label) {
            return *id;
        }
        let id = self.base.interned_label_id(label);
        self.label_ids.borrow_mut().insert(Box::from(label), id);
        id
    }

    /// One statement's entry reset: the CSV memo, the statement-spanning
    /// scratch state (the ADR-0309 counters and the ADR-0402 applied count in
    /// one scratch reset), the ADR-0395 nested-evaluation budget and the
    /// ADR-0463 expression-build meter restart together. Every statement
    /// entry calls this and nothing else.
    pub(crate) fn begin_statement(&self) {
        self.csv.begin_statement();
        self.reset_statement();
        self.nested_evals().reset();
        self.expression_budget.reset();
    }

    pub fn stats(&self) -> QueryStats {
        self.scratch
            .map(|s| s.borrow().statement.stats.clone())
            .unwrap_or_default()
    }

    /// How many changes the current statement has applied so far
    /// (ADR-0402 Decision 2): the count the statement entry reset zeroed and
    /// every redo push grows, so a non-zero count is the statement's applied
    /// fact. A read view applies nothing and reads zero.
    pub(crate) fn applied_ops(&self) -> u64 {
        self.scratch.map(|s| s.borrow().applied_ops()).unwrap_or(0)
    }

    pub(crate) fn scratch_snapshot(&self) -> Option<ScratchCheckpoint> {
        self.scratch.map(|scratch| scratch.borrow().checkpoint())
    }

    /// Roll the scratch back to a captured [`Self::scratch_snapshot`]:
    /// symmetric with the capture — a read view (no scratch) or a `None`
    /// snapshot (a failed batch on a read view) is the same no-op, handled
    /// once at this layer instead of re-decided at every caller.
    pub(crate) fn restore_scratch(&self, snapshot: Option<ScratchCheckpoint>) {
        if let (Some(scratch), Some(snapshot)) = (self.scratch, snapshot) {
            scratch.borrow_mut().restore(snapshot);
        }
    }

    /// ADR-0210 per-batch commit for `CALL { … } IN TRANSACTIONS`: hand the
    /// current batch's redo ops to the durable-commit sink, then
    /// [`WriteScratch::rebase`] the scratch at the new commit timestamp so
    /// live overlay state stays bounded by one batch while the
    /// statement-spanning state (stats, id allocators) carries over. A
    /// read view (no scratch) or an empty batch (no ops) is a no-op.
    pub(crate) fn commit_batch(&self, sink: &dyn crate::ctx::BatchCommit) -> Result<()> {
        let Some(scratch) = self.scratch else {
            return Ok(());
        };
        let (ops, ts) = {
            let s = scratch.borrow();
            (s.redo_ops(), s.snapshot_ts())
        };
        if ops.is_empty() {
            return Ok(());
        }
        // The redo is drained only as part of the post-commit rebase
        // below: a failed commit must leave the scratch (entity maps +
        // redo) intact for the caller's abort/restore path, not
        // half-updated with the redo gone.
        let commit_ts = sink.commit_batch(ops, ts)?;
        // The batch's writes just entered the base — any label it
        // interned invalidates the memoized resolutions, so the next
        // label check of this statement re-resolves against the grown
        // interner (LABS-1116: a memoized `None` for a label the batch
        // went on to create would answer a wrong `Some(false)` for the
        // rest of the statement).
        // The batch's writes just entered the base — any label it
        // interned invalidates the memoized resolutions, so the next
        // label check of this statement re-resolves against the grown
        // interner (LABS-1116: a memoized `None` for a label the batch
        // went on to create would answer a wrong `Some(false)` for the
        // rest of the statement).
        self.label_ids.borrow_mut().clear();
        scratch.borrow_mut().rebase(commit_ts);
        Ok(())
    }

    pub fn procedure(&self, name: &str) -> Option<Procedure> {
        self.base.procedure(name)
    }

    /// Materialize a node as a `Value` for result rows and entity builtins
    /// (`startNode()`/`endNode()`). Falls back to the virtual-node side
    /// table (`virtual_nodes`, CIP-257) when the store/overlay lookup
    /// misses, so endpoints of synthesized visualization relationships
    /// resolve without a store backing. This is the one graph-level lookup
    /// shared by the entity builtins and the visualization rows. Unlike
    /// [`Self::node`], which write paths use and which must keep missing on
    /// virtual ids (CIP-257 "Write behavior on virtual entities"), this
    /// read-path materialization is the only place the fallback is
    /// consulted.
    pub fn node_value(&self, id: NodeId) -> Value {
        if let Some(node) = self.node(id) {
            return Value::from_node(node);
        }
        if let Some(value @ relate_model::CypherValue::Node { .. }) = lookup_virtual_node(id) {
            return relate_core::cypher_to_value(&value);
        }
        Value::Null
    }

    pub fn rel_value(&self, id: RelId) -> Value {
        self.rel(id)
            .map(Value::from_relationship)
            .unwrap_or(Value::Null)
    }

    pub fn is_deleted_node(&self, id: NodeId) -> bool {
        self.scratch
            .map(|s| s.borrow().batch.deleted_nodes.contains(&id))
            .unwrap_or(false)
    }

    pub fn is_deleted_rel(&self, id: RelId) -> bool {
        self.scratch
            .map(|s| s.borrow().batch.deleted_rels.contains(&id))
            .unwrap_or(false)
    }

    /// Every live node id, ascending: the snapshot's ids minus the scratch's
    /// deletions plus its creations (ADR-0173). A copy-on-write entry was
    /// read from the snapshot, so it is a snapshot id already, and a delete
    /// drops the id from `created_nodes`. The two sets meet only if an
    /// allocator handed out a committed id; `dedup` keeps that to one entry.
    pub fn all_node_ids(&self) -> Vec<NodeId> {
        let mut ids = self.base.all_node_ids_at(self.snapshot_ts());
        if let Some(cell) = self.scratch {
            let s = cell.borrow();
            ids.retain(|id| !s.batch.deleted_nodes.contains(id));
            ids.extend(s.batch.created_nodes.iter().copied());
            ids.sort_unstable();
            ids.dedup();
        }
        ids
    }

    /// Every live relationship id, ascending; built like
    /// [`Self::all_node_ids`] from `deleted_rels` and `created_rels`.
    pub fn all_rel_ids(&self) -> Vec<RelId> {
        let mut ids = self.base.all_rel_ids_at(self.snapshot_ts());
        if let Some(cell) = self.scratch {
            let s = cell.borrow();
            ids.retain(|id| !s.batch.deleted_rels.contains(id));
            ids.extend(s.batch.created_rels.iter().copied());
            ids.sort_unstable();
            ids.dedup();
        }
        ids
    }
}

mod adjacency;
mod catalog;
mod label_probe;
mod materialize;
mod prop_ix;
mod reads;
mod redo_log;
mod rel_seek;
mod scratch;
mod seek;
mod validation;
mod virtual_nodes;
mod writes;

pub(crate) use materialize::MaterializeCache;
pub(crate) use reads::ScanNode;
pub(crate) use redo_log::ValidatedCatalog;
pub(crate) use virtual_nodes::{lookup_virtual_node, register_virtual_node};

use scratch::ScratchCheckpoint;
pub use scratch::WriteScratch;

#[cfg(test)]
mod adjacency_tests;
#[cfg(test)]
mod allocator_exhaustion_tests;
#[cfg(test)]
pub(crate) mod batch_commit_tests;
#[cfg(test)]
mod budget_tests;
#[cfg(test)]
mod label_memo_tests;
#[cfg(test)]
mod materialize_tests;
#[cfg(test)]
mod payload_sharing_tests;
#[cfg(test)]
mod query_semantics_tests;
#[cfg(test)]
mod read_projection_tests;
#[cfg(test)]
mod seek_merge_tests;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod validation_cursor_tests;
#[cfg(test)]
mod workspace_peak_tests;
#[cfg(test)]
mod write_stat_tests;
