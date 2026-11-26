//! The PROFILE metric-wire family (ADR-0344 phase 2 + metric fields,
//! split from `plan_wire.rs` when the IN-TRANSACTIONS batch facts made it
//! the family's seventh shape): the traversal rung a node reports, the
//! metric set as named fields, and the shapes the set composes — the
//! store-read mix, the phase times, the walk start, the frontier
//! family's run counters, the `CALL { … } IN TRANSACTIONS` batch facts
//! (REL8-2047) — plus the profiled-facts presentation bundle. The node
//! wire form and its decoder stay in `plan_wire.rs`; this module is the
//! family's one home, re-exported through `wire` the way the memo and
//! warning shapes are.

/// The traversal rung a PROFILE node reports (LABS-1117), typed from the
/// wire vocabulary: the chain carries its hop count, so a chain without
/// one — the half-state the parser rejects — is unrepresentable. [`Self::as_str`]
/// is the vocabulary's one spelling; `plan_keys::KERNEL_RUNGS` is derived
/// from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KernelRungWire {
    /// The unrolled fixed-k chain kernel, `hops` hops.
    Chain { hops: i64 },
    /// The hub-star candidate-intersection kernel.
    HubStar,
    /// The frontier-NFA rung over the BFS.
    FrontierNfa,
    /// The per-row walker — every gate's decline.
    Walker,
}

impl KernelRungWire {
    /// The chain rung's wire name — a name, never a variant value, so no
    /// hop count is fabricated to reach it.
    pub const CHAIN: &'static str = "chain";
    /// The hub-star rung's wire name.
    pub const HUB_STAR: &'static str = "hub-star";
    /// The frontier-NFA rung's wire name.
    pub const FRONTIER_NFA: &'static str = "frontier-nfa";
    /// The per-row walker's wire name.
    pub const WALKER: &'static str = "walker";
    /// Every rung name, in vocabulary order — the wire table's one
    /// spelling. `plan_keys::KERNEL_RUNGS` is this list; the executor's
    /// `KernelRung::as_str` reads the same constants.
    pub const NAMES: [&'static str; 4] = [
        Self::CHAIN,
        Self::HUB_STAR,
        Self::FRONTIER_NFA,
        Self::WALKER,
    ];

    /// The rung's wire name (`const` so the table derives from this match
    /// instead of repeating it).
    pub const fn as_str(self) -> &'static str {
        match self {
            KernelRungWire::Chain { .. } => Self::CHAIN,
            KernelRungWire::HubStar => Self::HUB_STAR,
            KernelRungWire::FrontierNfa => Self::FRONTIER_NFA,
            KernelRungWire::Walker => Self::WALKER,
        }
    }

    /// The chain rung's hop count — `None` for every other rung.
    pub const fn hops(self) -> Option<i64> {
        match self {
            KernelRungWire::Chain { hops } => Some(hops),
            KernelRungWire::HubStar | KernelRungWire::FrontierNfa | KernelRungWire::Walker => None,
        }
    }
}

/// The PROFILE metric set (ADR-0344 phase 2 + metric fields) as named
/// fields: which number is rows, nanoseconds, store accesses, or
/// row-materialization bytes is stated by the names, not by a position
/// convention.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PlanMetricsWire {
    pub rows: i64,
    pub time_ns: i64,
    pub db_hits: i64,
    pub memory_bytes: i64,
    /// Expression allocation bytes charged by the operator's clause
    /// (ADR-0463), beside the row-materialization `memory_bytes`.
    pub expression_built_bytes: i64,
    /// The store-read mix behind `db_hits` (LABS-369).
    pub reads: PlanReadsWire,
    /// The phase times behind the read kinds (LABS-448): the seek's and
    /// the walk's elapsed nanoseconds — emitted with the metric block,
    /// like the kinds, so a half-present pair is a wire-contract break.
    pub phases: PlanPhasesWire,
    /// The walk's chosen start, when the clause walked one (LABS-369).
    pub start: Option<PlanStartWire>,
    /// Which rung served the clause's `MATCH` walk (LABS-1117), typed: the
    /// chain carries its hop count, so the half-state the wire rejects (a
    /// chain rung without one) is unrepresentable here. `None` when the
    /// producer emitted no key (a non-`MATCH` clause, a folded or `SEARCH`
    /// `MATCH`, or a batch-chain `MATCH`).
    pub kernel: Option<KernelRungWire>,
    /// Whether the clause ran ADR-0468's batch scan/filter/project chain
    /// (LABS-359) — `false` on the wire means absent, the producer emits
    /// the key only when true.
    pub batched: bool,
    /// The batch filter's zone skips (LABS-645): conjunct-batch
    /// evaluations the chunk's derived min/max zone metadata provably
    /// replaced with the correct empty survivor set — `0` on the wire
    /// means absent, the producer emits the key only when nonzero.
    pub zone_skips: i64,
    /// The frontier family's run counters (ADR-0478), `Default` = the
    /// family declined the clause: the producer emits the whole set —
    /// the run counters, the NFA rung's facts, and the budget fraction —
    /// only when it ran (`levels > 0`), all together like the start
    /// trio.
    pub frontier: PlanFrontierWire,
    /// Why the frontier rung did not serve the clause: a run-time decline
    /// (ADR-0516) or a plan-time gate (ADR-0478), one of the
    /// `plan_keys::DECLINE_REASONS` vocabulary — `None` on every clause
    /// the rung served or never considered, present only on a PROFILE node.
    pub frontier_decline: Option<&'static str>,
    /// The expression-memo counters (ADR-0654): `None` = no memo was
    /// probed on this node, the producer emitting the group only when one
    /// was (the `batched` / `zone_skips` / `frontier` presence rule).
    pub memo: Option<super::plan_memo::PlanMemoWire>,
    /// The `CALL { … } IN TRANSACTIONS` batch facts (REL8-2047):
    /// `Default` = the arm ran no batch, the producer emitting the seven
    /// integers together only when one did (the `frontier` presence
    /// rule's).
    pub intx: PlanInTxWire,
}

/// The frontier family's run counters a PROFILE node reports (ADR-0478):
/// the BFS's edge visits (the work its walk budget caps), its completed
/// levels, and its accepted endpoints — the `[frontier …]` operator
/// marker's own trio — beside the NFA rung's own facts: the
/// (node, state) cells the runs created, the re-arrival rule memo's
/// hits and misses, the admission asks by verdict, and the budget
/// fraction the rule's work spent.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PlanFrontierWire {
    pub edge_visits: i64,
    pub levels: i64,
    pub endpoints: i64,
    /// (node, state) cells the runs created — the pairs that occupied
    /// the span×states tables.
    pub cells: i64,
    /// Re-arrival rule memo hits: asks the verdict table answered
    /// without a search.
    pub memo_hits: i64,
    /// Re-arrival rule memo misses: asks that ran the bounded search.
    pub memo_misses: i64,
    /// Admission asks the representative-chain proof settled.
    pub representative_proofs: i64,
    /// Admission asks the bounded search proved.
    pub search_proven: i64,
    /// Admission asks the bounded search disproved within its bounds.
    pub search_disproven: i64,
    /// Admission asks the shared work budget cut (ADR-0516).
    pub declined_exhausted: i64,
    /// The re-arrival work fraction the runs spent — one run's budget as
    /// the denominator, so `1.0` is a drained (declined) run and a
    /// multi-seed clause reads as run-equivalents.
    pub budget_fraction: f64,
}

/// The `CALL { … } IN TRANSACTIONS` batch facts a PROFILE node reports
/// (REL8-2047): the batch counters — run, committed, rolled back — and
/// the per-batch maxima: the input batch's and the installed output's
/// retained row bytes, the scratch the batch created, and the redo
/// log's reserved bytes at commit (the WAL payload it wrote). `Default`
/// = the arm ran no batch (or the node is not the batched call): the
/// producer emits the seven integers together only when one did.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PlanInTxWire {
    /// Batches that started a transaction.
    pub batches: i64,
    /// Batches that committed.
    pub committed: i64,
    /// Batches that rolled back under the `ON ERROR` policy.
    pub rolled_back: i64,
    /// The largest input batch's retained row bytes.
    pub peak_input_rows_bytes: i64,
    /// The largest installed output's retained row bytes.
    pub peak_output_rows_bytes: i64,
    /// The most scratch bytes one batch created (redo excluded).
    pub peak_scratch_bytes: i64,
    /// The most redo-log bytes one batch carried at its commit — the WAL
    /// payload it wrote.
    pub peak_wal_bytes: i64,
}

impl PlanInTxWire {
    /// Whether the arm ran any batch — the group's one presence rule (the
    /// executor twin is `relate_executor::InTxBatches::ran`): the wire
    /// emits the seven keys only when it holds, so their absence is the
    /// whole "no batches" fact.
    pub fn ran(&self) -> bool {
        self.batches > 0
    }
}

/// A metrics-carrying node's presentation bundle (cumulative vs self):
/// the PROFILE metric set and its subtree total — the derived total
/// travels with the metrics it sums, present together or not at all, so
/// no presentation can render a total beside unavailable metrics or a
/// metric set without one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlanProfiledFacts {
    /// The node's PROFILE metric set.
    pub metrics: PlanMetricsWire,
    /// The subtree total of the node's elapsed time: its own `time_ns`
    /// plus every descendant's — the one fact the `Total (ms)` cell and
    /// the document's `total_time_ms` render.
    pub total_time_ns: i64,
}

/// The store-read mix a PROFILE node reports (LABS-369): the kinds the
/// one `Db Hits` total blends — relationship hops, neighbor walks, and
/// node-store reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlanReadsWire {
    pub hops: i64,
    pub neighbor_walks: i64,
    pub node_reads: i64,
}

/// The phase times a PROFILE node reports (LABS-448): the elapsed time
/// the read kinds' store work spent — the seek (candidate resolution and
/// admission, the `Nodes` kind's work) and the walk (relationship
/// traversal, the `Rels`/`Degree` kinds' work), inclusive of a
/// multi-element pattern's nested phases.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlanPhasesWire {
    /// The seek phase's nanoseconds.
    pub seek_ns: i64,
    /// The walk phase's nanoseconds.
    pub walk_ns: i64,
}

/// How a PROFILE node's walk chose its start (LABS-369): the pattern
/// element index, the direction, and the reason class the executor
/// recorded (`bound-pivot`, `cheapest`, `seek-anchored`, `index-0`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlanStartWire {
    pub element: i64,
    pub backward: bool,
    /// The reason class, one of the `plan_keys::REASONS` vocabulary — a
    /// borrowed static, like the warning names.
    pub reason: &'static str,
}
