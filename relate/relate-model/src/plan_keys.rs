//! The plan wire form's key vocabulary (ADR-0344/0345): the names
//! `PlanNode::to_value` produces and every consumer reads, single-sourced
//! so drift between producer and consumer is a compile error, not a silent
//! mismatch. Its own module (the plan-node types and decoder stay in
//! `plan_wire`); the public path is `wire::plan_keys`.

use crate::wire::KernelRungWire;

pub const OPERATOR_TYPE: &str = "operatorType";
pub const ARGS: &str = "args";
pub const IDENTIFIERS: &str = "identifiers";
pub const CHILDREN: &str = "children";
pub const ID: &str = "Id";
pub const DETAILS: &str = "Details";
pub const ESTIMATED_ROWS: &str = "EstimatedRows";
/// PROFILE metrics (ADR-0344 phase 2).
pub const ROWS: &str = "Rows";
pub const TIME_NS: &str = "TimeNs";
/// PROFILE metric fields (ADR-0344 metric fields).
pub const DB_HITS: &str = "DbHits";
pub const MEMORY_BYTES: &str = "MemoryBytes";
/// The expression allocation bytes a profiled clause charged against the
/// ADR-0463 statement budget, emitted with the metric block.
pub const EXPRESSION_BUILT_BYTES: &str = "ExpressionBuiltBytes";
/// The store-read mix behind `DbHits` (LABS-369): the kinds the one
/// total blends — relationship hops, neighbor walks, node-store reads.
pub const HOPS: &str = "Hops";
pub const NEIGHBOR_WALKS: &str = "NeighborWalks";
pub const NODE_READS: &str = "NodeReads";
/// The phase times the read kinds' store work spent (LABS-448): the
/// seek (candidate resolution and admission — the `Nodes` kind's
/// work) and the walk (relationship traversal — the `Rels`/`Degree`
/// kinds' work), inclusive of a multi-element pattern's nested
/// phases. Emitted together with the metric block and the kinds.
pub const SEEK_TIME_NS: &str = "SeekTimeNs";
/// See [`SEEK_TIME_NS`].
pub const WALK_TIME_NS: &str = "WalkTimeNs";
/// The walk's chosen start (LABS-369): the pattern element index, its
/// direction, and the reason class (`bound-pivot`, `cheapest`,
/// `seek-anchored`, `index-0`) — the fact an opaque `Expand` hid.
pub const START_ELEMENT: &str = "StartElement";
pub const START_BACKWARD: &str = "StartBackward";
pub const START_REASON: &str = "StartReason";
/// Which rung served the clause's `MATCH` walk (LABS-1117), one of the
/// `KERNEL_RUNGS` vocabulary — emitted only when a rung served the
/// clause, with the metric set (a non-`MATCH` clause and a batch-chain
/// `MATCH` emit no key).
pub const KERNEL_RUNG: &str = "KernelRung";
/// The chain rung's hop count (LABS-1117) — emitted with `KernelRung`
/// when the rung is the chain, never for another rung.
pub const KERNEL_HOPS: &str = "KernelHops";
/// The store's active pattern-engine rung (LABS-1117): `auto`, `nfa`,
/// or `walk`, emitted on every `MATCH` operator of an EXPLAIN and
/// PROFILE tree — the one static rung fact the plan can state.
pub const PATTERN_ENGINE: &str = "PatternEngine";
/// The kernel-rung vocabulary (LABS-1117), the typed [`KernelRungWire`]'s
/// name list — one spelling, never a second list and never a fabricated
/// variant value. The executor's `KernelRung::as_str` reads the same name
/// constants, and the mapping is pinned by `relate`'s
/// `decline_and_start_reason_names_match_the_wire_tables`, like the start
/// reasons and the run-time declines.
pub const KERNEL_RUNGS: [&str; 4] = KernelRungWire::NAMES;
/// Whether the clause ran ADR-0468's batch scan/filter/project chain
/// over `Chunk` (LABS-359) — emitted only when true (like the start
/// trio's presence-only-when-walked), with the metric set.
pub const BATCHED: &str = "Batched";
/// The batch filter's zone skips (LABS-645): conjunct-batch
/// evaluations the chunk's derived min/max zone metadata provably
/// replaced with the correct empty survivor set — emitted only when
/// nonzero (a count's presence-only shape), with the metric set.
pub const ZONE_SKIPS: &str = "ZoneSkips";
/// The frontier family's run counters (ADR-0478): the BFS's edge
/// visits, completed levels, and accepted endpoints — emitted only
/// when the family ran the clause (`levels > 0`), the three together
/// (a half-present trio is a wire-contract break), with the metric
/// set.
pub const FRONTIER_EDGES: &str = "FrontierEdges";
pub const FRONTIER_LEVELS: &str = "FrontierLevels";
pub const FRONTIER_ENDPOINTS: &str = "FrontierEndpoints";
/// The frontier-NFA rung's own PROFILE facts (ADR-0478 §3, ADR-0516),
/// riding the frontier trio's presence-only-when-engaged group: the
/// (node, state) cells the runs created, the re-arrival rule memo's
/// hits and misses, the admission asks by verdict
/// (representative-proof / search-proven / search-disproven /
/// declined-exhausted), and the budget fraction the rule's work
/// spent — the seven integers and the one float emitted together
/// with the trio, a half-present set a wire-contract break.
pub const FRONTIER_CELLS: &str = "FrontierCells";
pub const FRONTIER_MEMO_HITS: &str = "FrontierMemoHits";
pub const FRONTIER_MEMO_MISSES: &str = "FrontierMemoMisses";
pub const FRONTIER_REPRESENTATIVE_PROOFS: &str = "FrontierRepresentativeProofs";
pub const FRONTIER_SEARCH_PROVEN: &str = "FrontierSearchProven";
pub const FRONTIER_SEARCH_DISPROVEN: &str = "FrontierSearchDisproven";
pub const FRONTIER_DECLINED_EXHAUSTED: &str = "FrontierDeclinedExhausted";
pub const FRONTIER_BUDGET_FRACTION: &str = "FrontierBudgetFraction";
/// Why the frontier rung did not serve the clause, one of the
/// `DECLINE_REASONS` vocabulary — emitted with the metric set when the
/// rung declined at plan time (counters absent: it never ran) or engaged,
/// ran, and fell through (ADR-0516, counters beside it).
pub const FRONTIER_DECLINE_REASON: &str = "FrontierDeclineReason";
/// The decline vocabulary: first the run-time declines (ADR-0516), the
/// executor's `FrontierRunDecline::as_str` names, then the plan-time
/// gates' (ADR-0478), `FrontierDecline::as_str` — single-sourced here so
/// a renamed decline fails loudly at the wire boundary, like the start
/// reasons.
pub const DECLINE_REASONS: [&str; 16] = [
    "rearrival-budget",
    "table-budget",
    "multiple-patterns",
    "carrying-row",
    "mode",
    "nested-spine",
    "all-reduce-group",
    "pinned-binding",
    "conjunct",
    "nfa-shape",
    "guarded-group",
    "full-enumeration",
    "ordered-cap",
    "path-count",
    "path-consumer",
    "cost",
];
/// The start-reason vocabulary (LABS-369), the executor's
/// `StartReason::as_str` names — single-sourced here so a renamed
/// reason fails loudly at the wire boundary, like the warning names.
pub const REASONS: [&str; 4] = ["bound-pivot", "cheapest", "seek-anchored", "index-0"];
/// The typed warnings the node carries (typed node warnings): a list
/// of warning maps from the [`PlanWarning`](crate::wire::PlanWarning)
/// vocabulary, present only when the node carries one. Each map
/// carries the warning's name and, when the warning has a payload
/// (the `AllNodesScan` warning's suggested index), that payload —
/// one spelling for the producer and every consumer.
pub const WARNINGS: &str = "warnings";
/// The warning map's name field (typed node warnings).
pub const WARNING_NAME: &str = "name";
/// The warning map's payload field (typed node warnings): the
/// `AllNodesScan` warning's suggested property index text.
pub const WARNING_INDEX: &str = "index";
/// The Neo4j-facing root args (ADR-0357): the plan's rendered text —
/// Neo4j's plan root carries it and tooling (Browser's Code tab,
/// strict clients) renders from it. The engine's own args stay as
/// they are; the one decoration lives on `PlanNode` (the
/// `relate` crate's `to_neo4j_value`, this vocabulary's only
/// writer) and each surface calls it at its boundary.
pub const STRING_REPRESENTATION: &str = "string-representation";
/// The planner and runtime identity args (ADR-0357): `planner` names
/// the planner, `planner-impl`/`planner-version` its implementation
/// and version; `runtime`/`runtime-impl`/`runtime-version` the same
/// for the runtime; `version` is the Cypher language version — Neo4j's
/// plan root carries all of them and tooling (cypher-shell's summary
/// block, Browser's footer) reads them.
pub const PLANNER_IMPL: &str = "planner-impl";
/// See [`PLANNER_IMPL`].
pub const PLANNER: &str = "planner";
/// See [`PLANNER_IMPL`].
pub const PLANNER_VERSION: &str = "planner-version";
/// See [`PLANNER_IMPL`].
pub const RUNTIME: &str = "runtime";
/// See [`PLANNER_IMPL`].
pub const RUNTIME_IMPL: &str = "runtime-impl";
/// See [`PLANNER_IMPL`].
pub const RUNTIME_VERSION: &str = "runtime-version";
/// See [`PLANNER_IMPL`]: the Cypher language version the plan ran
/// under.
pub const VERSION: &str = "version";
/// See [`PLANNER_IMPL`]: Neo4j's `Memory` arg — the root mirrors this
/// engine's row-materialization bytes under it (ADR-0357).
pub const MEMORY: &str = "Memory";
