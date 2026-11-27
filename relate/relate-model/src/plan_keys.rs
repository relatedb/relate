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
/// Which planner ladder level produced `EstimatedRows` (ADR-0708): one of
/// [`EstimateProvenance::NAMES`](crate::wire::EstimateProvenance::NAMES). Emitted exactly when `EstimatedRows` is, never
/// alone. The one place the key (and its siblings in ADR-0708) are named.
pub const ESTIMATE_PROVENANCE: &str = "EstimateProvenance";
/// The fused parent operator's `Id` (virtual plan operators) — present
/// exactly on a fused virtual child: a display-only breakdown step of its
/// fused parent operator, never an executed one. The key's presence IS
/// the fused fact (the `Batched` flag's presence-only shape); there is no
/// separate flag. A fused child never carries the PROFILE metric block:
/// rows, time and memory stay the fused parent's clause totals.
pub const FUSED_INTO: &str = "FusedInto";
/// The plan's peak logical workspace bytes (ADR-0463's receipt): the
/// root's one admission figure, emitted when admission is on.
pub const PEAK_LOGICAL_BYTES: &str = "PeakLogicalBytes";
/// The batched `CALL { … } IN TRANSACTIONS` body role marker
/// (REL8-2047): the `CallSubquery` parent operator's non-negative id on
/// the root of a body subtree the plan renders as children, so a walk can
/// tell a body operator from the pipeline's data-flow child — the
/// `FusedInto` shape for the display-only steps. Absent on every node of
/// the statement's own pipeline.
pub const BODY_OF: &str = "BodyOf";
/// The walk-side keys a fused virtual child never carries (virtual plan
/// operators): the walk's read kinds and its phase time stay the fused
/// parent's. The fused-child parser rejects these together with
/// [`METRIC_BLOCK_KEYS`] at one site — never a hand-listed subset there.
pub const FUSED_CHILD_NEVER: [&str; 3] = [HOPS, NEIGHBOR_WALKS, WALK_TIME_NS];
/// The PROFILE metric block's five scalar keys (ADR-0344 phase 2) — one
/// table for the accessor's scalar read, the fused-child parser's
/// rejection set and the fixtures, so a sixth scalar metric extends one
/// list instead of every hand-listed copy.
pub const METRIC_BLOCK_KEYS: [&str; 5] =
    [ROWS, TIME_NS, DB_HITS, MEMORY_BYTES, EXPRESSION_BUILT_BYTES];
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
/// The group's ten integer keys in the order of `PlanFrontierWire`'s
/// fields (edges, levels, endpoints, cells, memo hits, memo misses,
/// representative proofs, search proven, search disproven, declined
/// exhausted): the one table the decoder, the producer and any consumer
/// iterate.
pub const FRONTIER_GROUP_KEYS: [&str; 10] = [
    FRONTIER_EDGES,
    FRONTIER_LEVELS,
    FRONTIER_ENDPOINTS,
    FRONTIER_CELLS,
    FRONTIER_MEMO_HITS,
    FRONTIER_MEMO_MISSES,
    FRONTIER_REPRESENTATIVE_PROOFS,
    FRONTIER_SEARCH_PROVEN,
    FRONTIER_SEARCH_DISPROVEN,
    FRONTIER_DECLINED_EXHAUSTED,
];
/// The expression-memo group (ADR-0654): the probes, hits, inserts,
/// refused inserts, table bytes and disabled-memo count the operator's
/// memos reported, emitted together only on a PROFILE node whose memos
/// were probed at least once (`MemoProbes > 0`) — absent means no memo
/// was used. `MemoBytes` is never part of `MemoryBytes` (ADR-0654 §2).
pub const MEMO_PROBES: &str = "MemoProbes";
pub const MEMO_HITS: &str = "MemoHits";
pub const MEMO_INSERTS: &str = "MemoInserts";
pub const MEMO_REFUSED: &str = "MemoRefused";
pub const MEMO_BYTES: &str = "MemoBytes";
pub const MEMO_DISABLED_COUNT: &str = "MemoDisabledCount";
/// The group's six integer keys in the order of `PlanMemoWire`'s fields
/// (probes, hits, inserts, refused, bytes, disabled count): the one table
/// the decoder, the producer and any consumer iterate.
pub const MEMO_GROUP_KEYS: [&str; 6] = [
    MEMO_PROBES,
    MEMO_HITS,
    MEMO_INSERTS,
    MEMO_REFUSED,
    MEMO_BYTES,
    MEMO_DISABLED_COUNT,
];
/// The `CALL { … } IN TRANSACTIONS` batch facts (REL8-2047): the batch
/// counters — run, committed, rolled back — and the per-batch maxima (the
/// input batch's and the installed output's retained row bytes, the
/// scratch the batch created, the redo log's reserved bytes at commit —
/// the WAL payload it wrote), emitted together only on a PROFILE node
/// whose arm ran batches (`InTxBatches > 0`), with the metric set — a
/// half-present set is a wire-contract break.
pub const IN_TX_BATCHES: &str = "InTxBatches";
pub const IN_TX_COMMITTED: &str = "InTxCommitted";
pub const IN_TX_ROLLED_BACK: &str = "InTxRolledBack";
pub const IN_TX_PEAK_INPUT_ROWS_BYTES: &str = "InTxPeakInputRowsBytes";
pub const IN_TX_PEAK_OUTPUT_ROWS_BYTES: &str = "InTxPeakOutputRowsBytes";
pub const IN_TX_PEAK_SCRATCH_BYTES: &str = "InTxPeakScratchBytes";
pub const IN_TX_PEAK_WAL_BYTES: &str = "InTxPeakWalBytes";
/// The group's seven integer keys in the order of `PlanInTxWire`'s fields
/// (batches, committed, rolled back, peak input bytes, peak output bytes,
/// peak scratch bytes, peak WAL bytes): the one table the decoder, the
/// producer and any consumer iterate.
pub const IN_TX_GROUP_KEYS: [&str; 7] = [
    IN_TX_BATCHES,
    IN_TX_COMMITTED,
    IN_TX_ROLLED_BACK,
    IN_TX_PEAK_INPUT_ROWS_BYTES,
    IN_TX_PEAK_OUTPUT_ROWS_BYTES,
    IN_TX_PEAK_SCRATCH_BYTES,
    IN_TX_PEAK_WAL_BYTES,
];
/// The memo types behind the group, one name each from
/// [`MemoKindWire::NAMES`](crate::wire::MemoKindWire::NAMES) — emitted
/// only on the plan root, beside the group (ADR-0654 §3).
pub const MEMO_KINDS: &str = "MemoKinds";
/// Why the frontier rung did not serve the clause, one of the
/// `DECLINE_REASONS` vocabulary — emitted with the metric set when the
/// rung declined at plan time (counters absent: it never ran) or engaged,
/// ran, and fell through (ADR-0516, counters beside it).
pub const FRONTIER_DECLINE_REASON: &str = "FrontierDeclineReason";
/// The decline vocabulary: first the run-time declines (ADR-0516), the
/// executor's `FrontierRunDecline::as_str` names, then the plan-time
/// gates' (ADR-0478), `FrontierDecline::as_str` — single-sourced here so
/// a renamed decline fails loudly at the wire boundary, like the start
/// reasons. Each name is its own constant, read by the executor's
/// vocabulary macros and by the table below.
pub const DECLINE_REARRIVAL_BUDGET: &str = "rearrival-budget";
pub const DECLINE_TABLE_BUDGET: &str = "table-budget";
pub const DECLINE_SHORT_CAP: &str = "short-cap";
pub const DECLINE_MULTIPLE_PATTERNS: &str = "multiple-patterns";
pub const DECLINE_CARRYING_ROW: &str = "carrying-row";
pub const DECLINE_MODE: &str = "mode";
pub const DECLINE_NESTED_SPINE: &str = "nested-spine";
pub const DECLINE_ALL_REDUCE_GROUP: &str = "all-reduce-group";
pub const DECLINE_PINNED_BINDING: &str = "pinned-binding";
pub const DECLINE_CONJUNCT: &str = "conjunct";
pub const DECLINE_NFA_SHAPE: &str = "nfa-shape";
pub const DECLINE_GUARDED_GROUP: &str = "guarded-group";
pub const DECLINE_FULL_ENUMERATION: &str = "full-enumeration";
pub const DECLINE_ORDERED_CAP: &str = "ordered-cap";
pub const DECLINE_PATH_COUNT: &str = "path-count";
pub const DECLINE_PATH_CONSUMER: &str = "path-consumer";
pub const DECLINE_COST: &str = "cost";
/// The decline names, in the vocabulary's own order.
pub const DECLINE_REASONS: [&str; 17] = [
    DECLINE_REARRIVAL_BUDGET,
    DECLINE_TABLE_BUDGET,
    DECLINE_SHORT_CAP,
    DECLINE_MULTIPLE_PATTERNS,
    DECLINE_CARRYING_ROW,
    DECLINE_MODE,
    DECLINE_NESTED_SPINE,
    DECLINE_ALL_REDUCE_GROUP,
    DECLINE_PINNED_BINDING,
    DECLINE_CONJUNCT,
    DECLINE_NFA_SHAPE,
    DECLINE_GUARDED_GROUP,
    DECLINE_FULL_ENUMERATION,
    DECLINE_ORDERED_CAP,
    DECLINE_PATH_COUNT,
    DECLINE_PATH_CONSUMER,
    DECLINE_COST,
];
/// The start-reason vocabulary (LABS-369), the executor's
/// `StartReason::as_str` names — single-sourced here so a renamed
/// reason fails loudly at the wire boundary, like the warning names.
/// Each name is its own constant, so the variant↔name mapping reads the
/// same spelling this table does (the shape [`KernelRungWire`] takes for
/// the rungs).
pub const REASONS: [&str; 4] = [
    START_REASON_BOUND_PIVOT,
    START_REASON_CHEAPEST,
    START_REASON_SEEK_ANCHORED,
    START_REASON_INDEX_ZERO,
];
/// The bound-pivot start reason: see [`REASONS`].
pub const START_REASON_BOUND_PIVOT: &str = "bound-pivot";
/// The cheapest-start reason: see [`REASONS`].
pub const START_REASON_CHEAPEST: &str = "cheapest";
/// The seek-anchored start reason: see [`REASONS`].
pub const START_REASON_SEEK_ANCHORED: &str = "seek-anchored";
/// The first-element (un-optimised) start reason: see [`REASONS`].
pub const START_REASON_INDEX_ZERO: &str = "index-0";
/// The typed warnings the node carries (typed node warnings): a list
/// of warning maps from the [`PlanWarning`](crate::wire::PlanWarning)
/// vocabulary, present only when the node carries one. Each map
/// carries the warning's name and, when the warning has a payload
/// (the `LabelDisjunctionScan` warning's index pairs), that payload —
/// one spelling for the producer and every consumer.
pub const WARNINGS: &str = "warnings";
/// The warning map's name field (typed node warnings).
pub const WARNING_NAME: &str = "name";
/// The warning map's payload field (typed node warnings): the
/// `LabelDisjunctionScan` warning's index-pair text.
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
