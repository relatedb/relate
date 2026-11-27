//! Private write overlay for optimistic execute (ADR-0016).

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::HashSet;

use relate_core::{CancelToken, Namespace, NodeId, RelId, Result, TransactionClock, Value};
use relate_planner::LogicalPlan;

use crate::csv::CsvReader;
use crate::exec::kernels::construct_memo::ConstructMemos;
use crate::graph::{Graph, Procedure, QueryStats, SchemaSnapshot};
use crate::row::Row;

mod budget;
mod idlists;
mod spelled;

pub(crate) use spelled::is_internal_column;

pub use budget::NESTED_EVAL_BUDGET;
pub(crate) use budget::{
    memo_table_ceiling, ExpressionBudgetCounter, MemoLive, NestedEvalCounter, RetainedTotal,
    SeatShape,
};
pub(crate) use idlists::{intersect_ids, union_ids};
use spelled::SpelledNames;

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
    /// Live bytes of every stage memo in this statement (LABS-1478): one
    /// shared counter the memos reserve from and return on drop, so the
    /// query's memo footprint is bounded as a whole.
    memo_live: MemoLive,
    /// The statement's hashed-value memos over `datetime(text)`
    /// (ADR-0640): a pure parse, so it carries across the statement's stages.
    construct_memos: ConstructMemos,
    /// The store's ontology document as the calling transaction sees it
    /// (ADR-0121 §3, ADR-0227): per-admission state attached like the
    /// cancel token ([`Self::with_ontology_document`]), read by the
    /// `db.ontology.get` / `db.ontology.apply` procedure arms.
    /// `None` = no ontology set (also every test/diagnostic view).
    ontology_document: Option<&'a str>,
    /// The instant the calling explicit transaction began at, so every
    /// statement's clock pin reads one `<kind>.transaction()` instant
    /// ([`Self::with_transaction_clock`]); `None` = autocommit.
    transaction_clock: Option<TransactionClock>,
    pub(crate) csv: CsvReader<'a>,
    /// The `__`-prefixed names this statement declares itself, so the
    /// executor's synthetic columns can be told from a user's binding
    /// that spells the same prefix (REL8-1732).
    spelled: SpelledNames,
    /// The attached property store resolved for this statement (ADR-0665
    /// A/B, the per-read chain's one resolution): filled by the first
    /// property read and reused for the statement's lifetime — the
    /// statement's read guard pins the publication epoch, so the store's
    /// answers cannot move while the statement runs (ADR-0508's placement
    /// argument). `None` inside the cell = no store serves this snapshot,
    /// and the compact records answer as before. A cell like its
    /// neighbours: single-threaded executor access.
    serving_store: std::cell::OnceCell<Option<std::sync::Arc<dyn crate::graph::PropStore>>>,
}

/// Sampling mask for cooperative cancellation checks on hot per-row loops
/// (ADR-0111). `ticks & CANCEL_SAMPLE_MASK == 0` checks the token roughly
/// once per 1024 iterations, so the atomic load is not paid on every row.
const CANCEL_SAMPLE_MASK: usize = 1023;

/// Ticks between two token reads: the sampling interval the mask encodes.
#[cfg(test)]
pub(crate) const CANCEL_SAMPLE_INTERVAL: usize = CANCEL_SAMPLE_MASK + 1;

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
    /// scratch overlay carrying changes (an explicit transaction's or write
    /// statement's own writes live there and the base stats cannot see
    /// them; a scratch that has logged nothing yet is invisible to them
    /// and passes, ADR-0689), no
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
        self.has_no_pending_writes()
            && !self.base.in_write_window()
            && self.pinned_ts == self.base.commit_ts()
    }

    /// Whether this view reads its committed snapshot unchanged: no write
    /// scratch, or one with no changes yet (an explicit transaction always
    /// carries a scratch, even when it only reads). The vector matrix
    /// (ADR-0705) serves only such views; a view with its own uncommitted
    /// writes scans `Value`s, which read those writes.
    pub(crate) fn has_no_pending_writes(&self) -> bool {
        self.scratch
            .is_none_or(|scratch| scratch.borrow().has_no_changes())
    }

    /// The store's `RELATE_PREVIEW`-resolved set (ADR-0371), the
    /// executor-side gate the graph-type semantic layer reads for the
    /// stored-state half of the graph-type trio's gates.
    pub(crate) fn preview(&self) -> relate_core::PreviewFeatures {
        self.base.preview()
    }

    /// Whether `name`'s index was created inside the current write
    /// scratch — an explicit transaction's or a write statement's own
    /// uncommitted `CREATE INDEX`. A SEARCH over such an index is the
    /// still-populating rejection (ADR-0678): the index becomes part of a
    /// committed, scannable state only when the transaction commits. An
    /// `IF NOT EXISTS` create over an already-committed index records no
    /// create op, so it stays searchable.
    pub(crate) fn index_created_in_scratch(&self, name: &str) -> bool {
        self.scratch.is_some_and(|scratch| {
            scratch.borrow().redo.catalog_ops().any(|op| {
                matches!(
                    op,
                    crate::redo::RedoOp::CreateIndex { name: created, .. } if created == name
                )
            })
        })
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
        let memo_live = MemoLive::new(base.memo_budget_bytes());
        Self {
            base,
            schema: None,
            scratch,
            pinned_ts,
            cancel: None,
            cancel_ticks: Cell::new(0),
            nested_evals: NestedEvalCounter::new(base.nested_eval_budget()),
            expression_budget: ExpressionBudgetCounter::default(),
            expression_limit: base.expression_memory_limit(),
            construct_memos: ConstructMemos::new(memo_live.clone()),
            memo_live,
            ontology_document: None,
            transaction_clock: None,
            csv: CsvReader::new(base.csv_policy()),
            spelled: SpelledNames::default(),
            serving_store: std::cell::OnceCell::new(),
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
    /// [`Self::effective_ontology_document`], which overlays the write
    /// log's pending ontology op on it. A view without one behaves as if
    /// the store holds no ontology, which preserves test/diagnostic
    /// behavior.
    pub fn with_ontology_document(mut self, document: Option<&'a str>) -> Self {
        self.ontology_document = document;
        self
    }

    /// Attach the explicit transaction's begin-time clock: per-admission
    /// state like [`Self::with_cancel`]. A view without one is an autocommit
    /// statement, whose own instant is its transaction's.
    pub fn with_transaction_clock(mut self, clock: TransactionClock) -> Self {
        self.transaction_clock = Some(clock);
        self
    }

    /// The transaction clock attached by [`Self::with_transaction_clock`].
    pub(crate) fn transaction_clock(&self) -> Option<TransactionClock> {
        self.transaction_clock
    }

    /// The ontology document this view's readers must serve (ADR-0667):
    /// the write overlay's pending state when the statement or transaction
    /// pushed an ontology op — the log's last op wins, exactly as the
    /// commit pipeline's own last-op-wins rule — else the attached capture.
    /// This is the read-own-writes fact: a `get` after a `set` in the same
    /// transaction sees the pending document, and a rollback (the scratch
    /// discarded) returns every later read to the committed capture.
    pub(crate) fn effective_ontology_document(&self) -> Option<Cow<'a, str>> {
        if let Some(cell) = self.scratch {
            let scratch = cell.borrow();
            match scratch.redo.last_ontology_op() {
                Some(crate::redo::OntologyOp::Set(spec)) => {
                    return Some(Cow::Owned(spec.to_string()));
                }
                // A pending drop hides the committed document.
                Some(crate::redo::OntologyOp::Drop) => return None,
                None => {}
            }
        }
        self.ontology_document.map(Cow::Borrowed)
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

    pub(crate) fn snapshot_ts(&self) -> u64 {
        self.scratch
            .map(|s| s.borrow().snapshot_ts())
            .unwrap_or(self.pinned_ts)
    }

    /// The attached property store serving this statement's snapshot,
    /// resolved once per statement (ADR-0665 A/B): every property read
    /// below the overlay dispatches through this handle instead of
    /// re-resolving per read. `None` when no store is attached or the
    /// attached one serves a different snapshot — the compact records
    /// answer, exactly the per-read gate's own decline.
    pub(crate) fn resolved_store(&self) -> Option<&std::sync::Arc<dyn crate::graph::PropStore>> {
        self.serving_store
            .get_or_init(|| self.base.serving_store_handle(self.snapshot_ts()))
            .as_ref()
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
    /// has nothing to reset.
    pub(crate) fn reset_statement(&self) {
        if let Some(scratch) = self.scratch {
            scratch.borrow_mut().reset_statement();
        }
    }

    /// One statement's entry: the CSV memo, the statement-spanning scratch
    /// state (the ADR-0309 counters and the ADR-0402 applied count in one
    /// scratch reset), the ADR-0395 nested-evaluation budget, the
    /// ADR-0463 expression-build meter and the declared-name set restart
    /// together, and the declared-name set is re-seeded from `plan` in the
    /// same call — so no entry can produce a reset-but-undeclared
    /// statement. Every statement entry calls this and nothing else; the
    /// budget tests call it with an empty plan for the reset alone.
    ///
    /// The returned row is the statement's single root seed — the row its
    /// driving operator starts from, minted here so both entries take it from
    /// one home instead of fabricating it inline. A statement root drives
    /// exactly one row (ADR-0687 decision 6), which is why the plan grade
    /// measures store reads per execution and divides by no input count.
    pub(crate) fn begin_statement(&self, plan: &LogicalPlan) -> Row {
        self.csv.begin_statement();
        self.reset_statement();
        self.nested_evals().reset();
        self.expression_budget.reset();
        self.spelled.reset();
        crate::exec::declare_plan_names(self, plan);
        Row::new()
    }

    /// Record names the statement declares: the `__`-prefixed ones are the
    /// statement's own, not the executor's. Called only by the statement-entry
    /// plan walk ([`crate::exec::scope::declare_plan_names`]).
    pub(crate) fn declare_names<'n>(&self, names: impl IntoIterator<Item = &'n str>) {
        for name in names {
            self.spelled.declare(name);
        }
    }

    /// Whether `key` is a column the executor synthesized: in the `__`
    /// namespace and not declared by this statement. The one typed
    /// internality test every row-scanning consumer asks.
    pub(crate) fn is_internal_key(&self, key: &str) -> bool {
        spelled::is_internal_column(key) && !self.spelled.contains(key)
    }

    /// The statement's anonymous-seat prefixes, resolved once per
    /// statement and cached (see [`SpelledNames`]) — the data the
    /// pattern-walk schema builders take instead of the live view.
    pub(crate) fn seat_prefixes(&self) -> std::sync::Arc<crate::pattern_walk::SeatPrefixes> {
        self.spelled.seat_prefixes()
    }

    /// The statement's synthetic `__` prefix for `base`, lengthened past
    /// `names` and every declared name (see [`SpelledNames`]).
    pub(crate) fn uncolliding_prefix<'n>(
        &self,
        base: &str,
        names: impl IntoIterator<Item = &'n str>,
    ) -> String {
        self.spelled.uncolliding_prefix(base, names)
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

    /// The current batch's created bytes for PROFILE (REL8-2047): the
    /// `IN TRANSACTIONS` arm's per-batch scratch figure, read before the
    /// batch commits or rolls back. A read view reads zero — the same
    /// fold every sibling read on this graph makes.
    pub(crate) fn scratch_created_bytes(&self) -> u64 {
        self.scratch
            .map(|scratch| scratch.borrow().scratch_created_bytes())
            .unwrap_or(0)
    }

    /// The current batch's redo-log bytes for PROFILE (REL8-2047) — the
    /// WAL payload its commit writes, the arm's per-batch WAL figure. A
    /// read view reads zero.
    pub(crate) fn redo_reserved_bytes(&self) -> u64 {
        self.scratch
            .map(|scratch| scratch.borrow().redo_reserved_bytes())
            .unwrap_or(0)
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

    /// One past the largest node id this view can show: committed ids are
    /// below the graph's counter, and a write view's scratch-created ids
    /// come from the scratch's own counter (seeded from the graph's, then
    /// advanced independently), so the bound is the larger of the two. A
    /// dense per-node table sized to it never misses a node.
    pub(crate) fn node_id_bound(&self) -> NodeId {
        let committed = self.base.next_node_id();
        match self.scratch {
            None => committed,
            Some(cell) => {
                let scratch = cell
                    .borrow()
                    .node_id_alloc
                    .load(std::sync::atomic::Ordering::Relaxed);
                committed.max(
                    NodeId::try_from(scratch).expect("the scratch id counter stops at i64::MAX"),
                )
            }
        }
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

pub use scratch::{ScratchCheckpoint, StatementMark, WriteScratch};

#[cfg(test)]
mod adjacency_tests;
#[cfg(test)]
mod allocator_exhaustion_tests;
#[cfg(test)]
pub(crate) mod batch_commit_tests;
#[cfg(test)]
mod budget_tests;
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
