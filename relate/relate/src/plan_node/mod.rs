//! Query plan node tree for `EXPLAIN`/`PROFILE` (ADR-0344).
//!
//! [`PlanNode`] is the engine's plan representation: a tree of named
//! operators with details, identifiers, and — later phases — per-operator
//! estimates and `PROFILE` metrics. `EXPLAIN` builds the tree
//! without executing the wrapped statement; the regular execution path
//! never constructs a `PlanNode`, so a non-explained query pays nothing
//! for it.
//!
//! Split by concern: this module holds the type and the
//! `LogicalPlan` → tree construction; the text rendering lives at the
//! formatter crate (ADR-0359); [`serialize`] is the machine-native wire
//! form (ADR-0345).

mod clause;
mod estimate;
#[cfg(test)]
mod fixtures;
mod fused;
mod metric_args;
mod notifications;
mod seek;
mod serialize;
mod subject;
mod tree;
mod warning;

pub use estimate::PlanEstimate;
pub use fused::{FusedChild, FusedChildMetrics};
pub use warning::PlanWarning;

pub(crate) use notifications::{pattern_has_unbounded_expansion, tree_notifications};
pub(crate) use warning::patterns_share_an_anchor;

use std::collections::BTreeMap;

use relate_model::wire::Notification;
use relate_planner::LogicalPlan;

use crate::stats::PlanStats;
use relate_core::TokenEstimate;

/// One node of the query plan tree (ADR-0344). Estimates are an `Option`
/// because only the later estimate phase produces them; phase 1 carries
/// the operator, its details, and the identifiers it introduces.
#[derive(Clone, Debug, PartialEq)]
pub struct PlanNode {
    /// Operator name in the Neo4j vocabulary where the mechanism has an
    /// analog (`ProduceResults`, `Expand`, `EagerAggregation`), else this
    /// engine's own name.
    pub operator: String,
    /// The one-line details string shown in the `Details` column.
    pub details: String,
    /// Identifiers the operator introduces into the row.
    pub identifiers: Vec<String>,
    /// Child operators feeding this one, rendered beneath it.
    pub children: Vec<PlanNode>,
    /// The planner ladder's estimate for this operator and the level that
    /// produced it (ADR-0708), `None` where the ladder offers no count.
    /// One fact, never two fields: [`PlanNode::estimated_rows`] reads the
    /// rows.
    pub estimate: Option<PlanEstimate>,
    /// Pre-order id (parent before children), the one numbering fact both
    /// the text `Id` column and the wire form's `args.Id` read. Stamped
    /// when the engine builds the tree (`plan_tree`); a hand-built tree
    /// stamps its own via [`PlanNode::assign_ids`].
    pub id: usize,
    /// PROFILE per-operator metrics (ADR-0344 phase 2); `None` on every
    /// EXPLAIN node — stamped when the engine builds the profiled tree,
    /// never a constructor default.
    pub metrics: Option<ClauseMetrics>,
    /// The typed warnings the operator carries (typed node warnings):
    /// detected at tree construction (a cartesian multi-pattern `MATCH`,
    /// a label-less scan with a property index in the store) and — PROFILE
    /// only — derived from the 10x estimate-overshoot policy
    /// ([`PlanWarning::EstimationMiss`]). Empty is the constructor's
    /// value on every other node.
    pub warnings: Vec<PlanWarning>,
    /// The wrapped statement as the plan header echoes it (ADR-0359): the
    /// raw statement's one `EXPLAIN`/`PROFILE` preamble token removed.
    /// Stamped once by the engine at tree construction — the parse
    /// classified the preamble, so the boundary never re-scans the text
    /// for it; the formatter's header only whitespace-collapses it. Empty
    /// on a hand-built tree (a fixture with no statement), never a panic
    /// on a public path.
    pub statement_echo: String,
    /// Peak logical workspace for this plan, filled from the receipt when
    /// admission is on. `None` when admission is off, including every
    /// hand-built tree.
    pub token_estimate: Option<TokenEstimate>,
    /// The store's active pattern-engine rung (LABS-1117):
    /// `RELATE_PATTERN_ENGINE`'s `auto`/`nfa`/`walk`, stamped on every
    /// `MATCH` operator at tree construction — the one static fact
    /// EXPLAIN can state about which rung governs the clause's gates.
    /// `None` on every other operator and on a hand-built tree; the wire
    /// emits the `PatternEngine` arg when present. The *executed* rung
    /// (chain/hub-star/frontier/walker) is a runtime fact and lives on
    /// [`ClauseMetrics::kernel`].
    pub pattern_engine: Option<relate_executor::PatternEngine>,
    /// The node's child role, when it is not the pipeline's data-flow
    /// child: `None` on every real operator, `Some` on a child the walks
    /// must tell apart from the clause it feeds — the one owner of that
    /// fact, so a second role can never arrive as a second parallel field.
    /// The two roles are mutually exclusive by construction (a node is
    /// attached as one or the other), so the field is one `Option`.
    pub role: Option<ChildRole>,
}

/// Why a node is not its parent's data-flow child — the one child-role
/// fact [`PlanNode::is_executed_child`] reads.
#[derive(Clone, Debug, PartialEq)]
pub enum ChildRole {
    /// A display-only fused virtual step of its parent operator
    /// (ADR-0622): the seek step of an index-seek-started MATCH or one
    /// hop's `Expand` step. The parent link and the step's measured pair
    /// travel together inside [`FusedChild`], so the half-states are
    /// unrepresentable.
    Fused(FusedChild),
    /// The root of a batched `CALL { … } IN TRANSACTIONS` body subtree the
    /// `CallSubquery` operator renders as its children (REL8-2047), its
    /// parent's operator id: the body runs inside the operator's batch, so
    /// its work is that operator's record, never a per-body-operator one.
    Body { parent: usize },
}

/// The typed plan tree drops **iteratively** (ADR-0628's security-round
/// closure): the tree is built on the engine's big-stack worker but
/// dropped wherever the caller's `QueryResult` drops — an FFI host
/// thread (~1 MiB) — and a derived recursive drop overflows on a deep
/// chain (~10k+ clauses). The drain below takes each node's children
/// into a heap worklist and lets every other field drop naturally in
/// place — all of them shallow: `FusedChild` carries the parent id and
/// the seek metrics, never a nested node; the warnings, identifiers and
/// metrics hold scalars — so dropping an arbitrary-depth plan tree
/// costs constant stack. `children` is the struct's only PlanNode-bearing
/// field; a future one must join the worklist here.
impl Drop for PlanNode {
    fn drop(&mut self) {
        // A leaf needs no drain: its remaining fields are shallow, and
        // the derived field drops after this body return suffice.
        if self.children.is_empty() {
            return;
        }
        // Each popped node surrenders its children to the worklist before
        // it goes out of scope, so its own drop sees an empty children
        // vector and stays one frame deep.
        let mut work = std::mem::take(&mut self.children);
        while let Some(mut node) = work.pop() {
            work.append(&mut node.children);
        }
    }
}

/// The PROFILE metrics collected per `(stage, part, clause)` — one
/// spelling of the key type every metric-threading signature reads.
pub(crate) type ClauseMetricsMap = std::collections::BTreeMap<(usize, usize, usize), ClauseMetrics>;

/// A statement's plan artifacts: the structured tree an `EXPLAIN`/`PROFILE`
/// statement builds (ADR-0344/0345) and the query notifications derived with
/// it (ADR-0363). One record because the two facts co-vary — derivation runs
/// only where a tree is built — so "notifications without a tree" is
/// unrepresentable.
///
/// ```
/// use relate::Database;
///
/// let db = Database::open(":memory:")?;
/// let mut conn = db.connect()?;
/// let explained = conn.query("EXPLAIN MATCH (n) RETURN n")?;
/// let artifacts = explained.plan.as_deref().expect("EXPLAIN stamps the artifact");
/// assert_eq!(artifacts.root, *explained.plan_root().expect("the same root"));
/// assert_eq!(artifacts.notifications.as_slice(), explained.notifications());
/// # Ok::<(), relate::Error>(())
/// ```
#[derive(Debug)]
pub struct PlanArtifacts {
    /// The structured plan tree, ids stamped pre-order.
    pub root: PlanNode,
    /// The query notifications derived with the tree (ADR-0363 decision 6
    /// order): the operator-scoped entries first, each carrying its node's
    /// [`PlanNode::id`], then the statement-scoped entries. Empty when the
    /// tree derived none.
    pub notifications: Vec<Notification>,
}

/// The built plan tree and its operator-scoped notification facts (ADR-0363
/// decision 4): the construction walk stamps each node's id and records the
/// clause notifications it derived for that node in a side map keyed by the
/// id. The facts live beside the public tree — never on [`PlanNode`] — so a
/// node's identity, equality and debug view are its own content.
pub(crate) struct PlanTree {
    /// The public tree, ids already stamped.
    pub(crate) root: PlanNode,
    /// The clause notifications staged by node id, in no particular order.
    staged: BTreeMap<usize, Vec<Notification>>,
}

impl PlanTree {
    /// Stamp the wrapped statement the header echoes (ADR-0359).
    pub(crate) fn with_statement_echo(mut self, echo: String) -> Self {
        self.root = self.root.with_statement_echo(echo);
        self
    }

    /// Attach the statement's constructor-memo facts at the plan root
    /// (ADR-0654 §3): they serve any expression, so no operator owns them.
    /// The root's own guarded-state sums keep their place and the
    /// constructor sums add to them; `memo_kinds` then names every memo
    /// type behind the total (empty when none was probed). A root that recorded no metrics
    /// (nothing ran there to measure) has nowhere to carry them.
    pub(crate) fn with_statement_memos(
        mut self,
        record: relate_executor::StatementMemoRecord,
    ) -> Self {
        if let Some(metrics) = self.root.metrics.as_mut() {
            metrics.memo_kinds = record.kinds.union(metrics.memo.operator_kinds());
            metrics.memo += record.counters;
        }
        self
    }

    /// The operator-scoped notifications staged on `operator_id` — empty
    /// when the node derived none.
    pub(crate) fn staged(&self, operator_id: usize) -> &[Notification] {
        self.staged
            .get(&operator_id)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// A tree with caller-supplied staged facts — the hand-built-tree test
    /// constructor; the engine's one production path is [`plan_tree`].
    #[cfg(test)]
    pub(crate) fn from_parts(root: PlanNode, staged: BTreeMap<usize, Vec<Notification>>) -> Self {
        Self { root, staged }
    }
}

/// One operator's PROFILE metrics (ADR-0344 phase 2 + metric fields):
/// rows produced, store accesses, row-materialization bytes, and elapsed
/// time, collected by the executor's dispatch loop and stamped onto the
/// node the plan tree builds from the same clause.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ClauseMetrics {
    /// Rows the operator produced (the rows flowing out of the clause).
    pub rows: u64,
    /// Store accesses the clause's arm performed: the delta of the
    /// executor's three graph-access counters (hop expansions, neighbor
    /// walks and node-store reads) across the clause — the engine's
    /// `db hits`, exactly the sum of [`ClauseMetrics::reads`]' kinds.
    pub db_hits: u64,
    /// Row-materialization bytes: the summed logical size (ADR-0042) of
    /// the rows the operator produced. Stateful operators' non-row state
    /// (aggregation tables, sort buffers) is a follow-up.
    pub memory_bytes: u64,
    /// Expression allocation bytes the operator's clause charged against
    /// the ADR-0463 statement budget — the `Expression Built (Bytes)` column,
    /// reported separately from the row-materialization bytes above.
    pub expression_built_bytes: u64,
    /// The operator's elapsed time, nanoseconds.
    pub time_ns: u128,
    /// The store-read mix behind [`ClauseMetrics::db_hits`] (LABS-369):
    /// the kinds the one total blends — hops, neighbor walks, node reads.
    pub reads: relate_executor::StoreReads,
    /// How the clause's walk chose its start (LABS-369), when it walked
    /// one — `None` for clauses that walk nothing. The fact an opaque
    /// `Expand` hid: a displaced start shows here instead of only as a
    /// large `Db Hits` total.
    pub start: Option<relate_executor::WalkStart>,
    /// Which rung served the clause's `MATCH` walk (LABS-1117): the chain
    /// kernel, the hub-star kernel, the frontier-NFA rung, or the per-row
    /// walker. `None` exactly when no `MATCH` operator ran: a non-`MATCH`
    /// clause; a `MATCH` a fold answered; a `MATCH` inside a nested
    /// evaluation; and a `MATCH` the ADR-0468 batch chain answered
    /// ([`ClauseMetrics::batched`]). A `SEARCH` `MATCH` reports `Walker`
    /// (it runs the per-row walker directly). The plan table's `Details`
    /// cell marks it (`[kernel …]`) and the wire emits `KernelRung` with
    /// the metric set when present.
    pub kernel: Option<relate_executor::KernelRung>,
    /// The clause's inclusive phase times (LABS-448): the seek (candidate
    /// resolution and admission — the `Nodes` kind's store work) and the
    /// walk (relationship traversal — the `Rels`/`Degree` kinds' store
    /// work), stamped only while a PROFILE statement's arm ran.
    pub phases: relate_executor::PhaseTimes,
    /// Whether the clause ran ADR-0468's batch scan/filter/project chain
    /// over `Chunk` (LABS-359) — the PROFILE-visible fact that a
    /// scan-shaped `MATCH` interior executed as batch operators. `false`
    /// is every other clause and every clause the chain declined; the
    /// plan table's `Details` cell marks it (`[batched]`) and the wire
    /// emits it with the metric set when true.
    pub batched: bool,
    /// The batch filter's zone skips (LABS-645): conjunct-batch
    /// evaluations the chunk's derived min/max zone metadata provably
    /// replaced with the correct empty survivor set — `0` on every
    /// operator the chain declined and every clause that skipped
    /// nothing. The plan table's `Details` cell marks it
    /// (`[zone-skips N]`) and the wire emits `ZoneSkips` with the metric
    /// set when nonzero.
    pub zone_skips: u64,
    /// The frontier family's run counters (ADR-0478): the BFS's edge
    /// visits, completed levels, and accepted endpoints, summed over the
    /// clause's per-seed runs — zero on every clause the frontier's
    /// compile gates declined (it never ran). A *run-time* decline
    /// (`frontier_decline` a `Run` reason, ADR-0516) is the opposite: the rung ran,
    /// so its counters stay recorded beside the reason — that pairing is
    /// how the profile shows what the declined run spent. The plan
    /// table's `Details` cell marks the operator
    /// (`[frontier edges=N levels=L endpoints=P]`) when it ran, and the
    /// wire emits the counter trio with the metric set when engaged
    /// (`levels > 0`, a run always completes its level 0).
    pub frontier: relate_executor::FrontierCounters,
    /// Why the frontier rung did not serve the clause (ADR-0516,
    /// extended): a plan-time gate's typed reason (the rung never ran,
    /// counters zero) or the run-time decline of a rung that engaged,
    /// ran, and fell through — `None` on every clause the rung served
    /// and on every clause it never considered. The plan table's
    /// `Details` cell marks it (`[frontier-declined …]`) and the wire
    /// emits `FrontierDeclineReason` with the metric set when present.
    pub frontier_decline: Option<relate_executor::FrontierDeclineReason>,
    /// The expression-memo counters (ADR-0654): the guarded-state memos
    /// the clause's stage ran, summed — and, on the plan root only, the
    /// statement's constructor memos added ([`ClauseMetrics::memo_kinds`]
    /// naming the memo types behind the sums). Zero when no memo was
    /// probed; the wire emits the `Memo*` group only when `probes > 0`,
    /// and the plan table's `Memo` column renders `hits/probes`.
    pub memo: relate_executor::MemoCounters,
    /// The memo types behind the root's [`ClauseMetrics::memo`] (ADR-0654
    /// §3): empty on every operator but the root.
    pub memo_kinds: relate_model::wire::MemoKindsWire,
    /// The `CALL { … } IN TRANSACTIONS` batch facts (REL8-2047): batches
    /// run, committed, rolled back, and the per-batch maxima — the input
    /// batch's and the installed output's retained row bytes, the scratch
    /// the batch created, and the redo log's reserved bytes at commit (the
    /// WAL payload it wrote). All-zero on every other operator; carried by
    /// the `CallSubquery` operator the batched call's record reports at.
    /// Re-exported by this crate beside the other metric types.
    pub intx: relate_executor::InTxBatches,
}

/// The one record→metric conversion: destructured, so a new
/// [`relate_executor::ClauseMetricsRecord`] field is a compile error
/// here rather than a silently dropped metric. The position key stays with
/// the caller; `db_hits` is derived from the read mix, never carried twice.
impl From<relate_executor::ClauseMetricsRecord> for ClauseMetrics {
    fn from(record: relate_executor::ClauseMetricsRecord) -> Self {
        let relate_executor::ClauseMetricsRecord {
            stage: _,
            part: _,
            clause: _,
            rows,
            memory_bytes,
            expression_built_bytes,
            time_ns,
            reads,
            phases,
            start,
            kernel,
            batch,
            frontier,
            frontier_decline,
            memo,
            intx,
        } = record;
        ClauseMetrics {
            rows,
            db_hits: reads.total(),
            memory_bytes,
            expression_built_bytes,
            time_ns,
            reads,
            start,
            kernel,
            phases,
            batched: batch.ran,
            zone_skips: batch.zone_skips,
            frontier,
            frontier_decline,
            memo,
            memo_kinds: Default::default(),
            intx,
        }
    }
}

impl PlanNode {
    /// The one constructor: every field but the content fields starts at
    /// its phase-1 value (`estimated_rows: None` until the estimate phase
    /// fills it, `id: 0` until [`PlanNode::assign_ids`] stamps the tree),
    /// so each stamp exists exactly once.
    pub fn new(
        operator: impl Into<String>,
        details: String,
        identifiers: Vec<String>,
        children: Vec<PlanNode>,
    ) -> PlanNode {
        PlanNode {
            operator: operator.into(),
            details,
            identifiers,
            children,
            estimate: None,
            id: 0,
            metrics: None,
            warnings: Vec::new(),
            statement_echo: String::new(),
            token_estimate: None,
            pattern_engine: None,
            role: None,
        }
    }

    /// Stamp PROFILE metrics (ADR-0344 phase 2) — the only way a node
    /// gains them; `None` is the constructor's value everywhere else.
    pub fn with_metrics(mut self, metrics: Option<ClauseMetrics>) -> PlanNode {
        self.metrics = metrics;
        self
    }

    /// Stamp an estimate with its ladder level (ADR-0344 phase 3,
    /// ADR-0708) — rows and provenance together, the only way a node
    /// gains one; `None` is the constructor's value everywhere else.
    pub fn with_estimate(
        mut self,
        rows: u64,
        provenance: relate_model::wire::EstimateProvenance,
    ) -> PlanNode {
        self.estimate = Some(PlanEstimate { rows, provenance });
        self
    }

    /// The estimated rows, when the ladder priced the operator.
    pub fn estimated_rows(&self) -> Option<u64> {
        self.estimate.map(|estimate| estimate.rows)
    }

    /// Stamp the typed warnings (typed node warnings) — the one way a
    /// hand-built tree attaches them (the engine's own tree stamps them
    /// at construction and, on PROFILE, derives
    /// [`PlanWarning::EstimationMiss`] beside the metrics).
    pub fn with_warnings(mut self, warnings: Vec<PlanWarning>) -> PlanNode {
        self.warnings = warnings;
        self
    }

    /// Stamp the statement the header echoes (ADR-0359): the engine's one
    /// derivation, at tree construction, from the parse's typed preamble
    /// classification. The boundary reads [`PlanNode::statement_echo`]
    /// instead of re-scanning the raw text.
    pub fn with_statement_echo(mut self, echo: String) -> PlanNode {
        self.statement_echo = echo;
        self
    }

    /// Stamp the schedule-v1 estimate. Only the root carries it.
    pub fn with_token_estimate(mut self, estimate: Option<TokenEstimate>) -> PlanNode {
        self.token_estimate = estimate;
        self
    }

    /// Mark the node a fused virtual child of `parent` (virtual plan
    /// operators) — the breakdown builder's one stamp: the parent link
    /// and the step's measured pair (the seek step's own; `None` on a
    /// hop step and on EXPLAIN trees) are set together, so the
    /// half-state is unrepresentable.
    pub fn with_fused(mut self, parent: usize, metrics: Option<FusedChildMetrics>) -> PlanNode {
        self.role = Some(ChildRole::Fused(FusedChild { parent, metrics }));
        self
    }

    /// Mark the node the root of a batched `CALL { … } IN TRANSACTIONS`
    /// body subtree (REL8-2047), naming its `CallSubquery` parent — the
    /// body arm's one stamp, the [`Self::with_fused`] idiom, so no caller
    /// writes the `role` field directly.
    pub fn with_body(mut self, parent: usize) -> PlanNode {
        self.role = Some(ChildRole::Body { parent });
        self
    }

    /// The node's fused-child fact, when it is a display-only fused step
    /// (ADR-0622) — the `role`'s fused arm, read by the walkers and the
    /// wire emitter. The match is exhaustive over [`ChildRole`], so a
    /// third role cannot compile without deciding this narrowing.
    pub fn fused(&self) -> Option<FusedChild> {
        match &self.role {
            Some(ChildRole::Fused(fused)) => Some(*fused),
            Some(ChildRole::Body { .. }) | None => None,
        }
    }

    /// The `CallSubquery` parent operator's id, when this node is a
    /// batched body subtree's root (REL8-2047) — the `role`'s body arm,
    /// exhaustive for the same reason as [`Self::fused`].
    pub fn body_of(&self) -> Option<usize> {
        match self.role {
            Some(ChildRole::Body { parent }) => Some(parent),
            Some(ChildRole::Fused(_)) | None => None,
        }
    }

    /// Whether this node is an executed child of its parent — the one
    /// child-role rule both walks read. Two child roles are skipped: a
    /// display-only fused breakdown step (ADR-0622), whose measured pair
    /// is a phase of the fused parent's clause totals, and a batched
    /// `CALL { … } IN TRANSACTIONS` body's operator root (REL8-2047),
    /// whose work is the `CallSubquery` operator's batch record. The wire
    /// twin is `PlanNodeWire::is_executed_child`.
    fn is_executed_child(&self) -> bool {
        self.role.is_none()
    }

    /// The node's executed children: every child
    /// [`Self::is_executed_child`] admits — the one skip policy for a
    /// walk that reads executed operators or their metrics; the wire twin
    /// is `PlanNodeWire::executed_children`.
    pub fn executed_children(&self) -> impl Iterator<Item = &PlanNode> {
        self.children
            .iter()
            .filter(|child| child.is_executed_child())
    }

    /// The echoed statement — the formatter's header input (external
    /// callers read it, never re-derive). Empty on a hand-built tree: a
    /// public accessor that never panics (the engine stamps the real echo
    /// on every plan-carrying result).
    pub fn statement_echo(&self) -> &str {
        self.statement_echo_opt().unwrap_or("")
    }

    /// The wrapped statement as the plan header echoes it, or `None` on a
    /// hand-built tree — the fact stated once; [`PlanNode::statement_echo`]
    /// derives its never-panicking `&str` form from this.
    pub fn statement_echo_opt(&self) -> Option<&str> {
        (!self.statement_echo.is_empty()).then_some(self.statement_echo.as_str())
    }

    /// Whether the tree is a `PROFILE` tree (ADR-0344 phase 2): `EXPLAIN`
    /// never stamps metrics, `PROFILE` stamps them on every clause
    /// operator the dispatch loop executed — the projection included, so
    /// a tree carrying metrics anywhere is a profiled tree. The one
    /// classification fact every surface reads to pick its summary key
    /// (`plan` vs `profile`, `queryPlan` vs `profiledQueryPlan`) and the
    /// renderer's metric-column gate, so neither drifts between surfaces.
    pub fn is_profiled(&self) -> bool {
        self.metrics.is_some() || self.children.iter().any(PlanNode::is_profiled)
    }

    /// Stamp the pre-order ids (parent before children) the text `Id`
    /// column and the wire form's `args.Id` both read. The engine's
    /// construction walk stamps its own tree as it builds it; a hand-built
    /// tree calls this before rendering or serializing. This pass is also
    /// the role links' maintainer: a child's `role` carries the parent id
    /// it nests under (a fused step's and a batched body root's alike), so
    /// a renumbered tree cannot carry a stale link.
    pub fn assign_ids(&mut self) {
        let mut next = 0usize;
        self.assign_ids_at(&mut next);
    }

    /// The recursive walk behind [`PlanNode::assign_ids`].
    fn assign_ids_at(&mut self, next: &mut usize) {
        self.id = *next;
        *next += 1;
        let parent = self.id;
        for child in &mut self.children {
            match &mut child.role {
                Some(ChildRole::Fused(fused)) => fused.parent = parent,
                Some(ChildRole::Body { parent: body }) => *body = parent,
                None => {}
            }
            child.assign_ids_at(next);
        }
    }
}

/// Build the plan tree for one logical plan (ADR-0344): one stage node per
/// `NEXT` link, chained as `Next` children of the stage above. Ids are
/// stamped once, by the construction walk itself, so every consumer reads
/// one numbering fact; that same walk records each clause's staged
/// notifications by node id. `stats` supplies the cardinality estimates
/// (ADR-0218), read from the store the caller holds under its plan read
/// guard.
pub(crate) fn plan_tree(plan: &LogicalPlan, stats: &dyn PlanStats) -> PlanTree {
    profiled_tree(plan, None, stats)
}

/// The PROFILE tree (ADR-0344 phase 2): the one tree walk with the
/// per-clause metrics collected by the dispatch loop stamped onto their
/// nodes — the same walk `plan_tree` runs, never a parallel constructor.
pub(crate) fn profiled_tree(
    plan: &LogicalPlan,
    metrics: Option<&ClauseMetricsMap>,
    stats: &dyn PlanStats,
) -> PlanTree {
    let mut cursor = BuildCursor::default();
    let root = stage_tree(plan, StagePos::stage(1), metrics, stats, &mut cursor);
    PlanTree {
        root,
        staged: cursor.staged,
    }
}

/// The construction walk's one cursor: the next pre-order id and the
/// operator-scoped notifications staged by node id. Threading the pair as
/// one value keeps every builder under the argument budget and makes the id
/// and the fact move together.
#[derive(Default)]
struct BuildCursor {
    /// The next pre-order id to hand out.
    next_id: usize,
    /// The operator-scoped notifications staged by node id.
    staged: BTreeMap<usize, Vec<Notification>>,
}

impl BuildCursor {
    /// Claim `count` consecutive pre-order ids and return the first.
    fn reserve(&mut self, count: usize) -> usize {
        let base = self.next_id;
        self.next_id += count;
        base
    }

    /// Claim one pre-order id.
    fn take_id(&mut self) -> usize {
        self.reserve(1)
    }

    /// Record a node's own operator-scoped notifications. A node that
    /// derived none stays absent from the map.
    fn stage(&mut self, operator_id: usize, notifications: Vec<Notification>) {
        if !notifications.is_empty() {
            self.staged.insert(operator_id, notifications);
        }
    }
}

use tree::{stage_tree, StagePos};
