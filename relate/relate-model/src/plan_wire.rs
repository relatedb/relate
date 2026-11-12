//! The structured query plan wire form (ADR-0344/0345): the typed plan
//! node accessor and the plan contract the presentation and the engine's
//! warning construction read (ADR-0400 — the table layout itself lives
//! with its one consumer in the formatter) — one spelling so no surface
//! can drift from the wire form it reads. The key vocabulary lives in its
//! own module (`plan_keys.rs`), and the typed warning vocabulary beside it
//! (LABS-461: `plan_warning.rs`), parsed here by the node accessor.

use super::plan_keys;
use super::plan_warning::PlanWarning;
use crate::CypherValue;

// ---------------------------------------------------------------------------
// Structured query plan wire form (ADR-0344/0345)
// ---------------------------------------------------------------------------

/// The one spelling of a plan column's unavailable value, shared by every
/// presentation of the plan table (the engine renderer and the surfaces'
/// table layout) so "column not produced" renders identically everywhere.
pub const PLAN_UNAVAILABLE: &str = "-";

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

// The typed warning vocabulary lives in its own module beside this one
// (LABS-461: one warning type — the enum, its estimate policy, and its
// wire-shape serializer in `plan_warning.rs`, re-exported through
// `wire`); the node wire form below reads it here.

/// The PROFILE metric set (ADR-0344 phase 2 + metric fields) as named
/// fields: which number is rows, nanoseconds, store accesses, or
/// row-materialization bytes is stated by the names, not by a position
/// convention.
#[derive(Clone, Copy, Debug, PartialEq)]
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

/// The typed view of one structured-plan wire node (ADR-0344/0345): the
/// producer (`relate`'s `PlanNode::to_value`) emits exactly this
/// shape, so this accessor states the invariants instead of tolerating
/// misses — a malformed plan fails loudly rather than silently dropping
/// a row or shifting cells into the wrong column.
pub struct PlanNodeWire<'a> {
    pub operator_type: &'a str,
    pub id: i64,
    pub details: &'a str,
    pub estimated_rows: Option<i64>,
    /// PROFILE metrics (ADR-0344 phase 2 + metric fields): rows
    /// produced, elapsed time, store accesses, and row-materialization
    /// bytes — present only on PROFILE trees: the producer always emits
    /// the four `args` metric fields together, so a half-present set is
    /// a wire-contract break that fails loudly.
    pub metrics: Option<PlanMetricsWire>,
    /// The node's args map, validated by the accessor (a map, present): a
    /// consumer reshaping the node (the HTTP Query API v2 plan) reads its
    /// display and identity entries from here rather than re-matching the
    /// raw map, so the invariant is stated once.
    pub args: &'a crate::IndexMap<String, CypherValue>,
    /// The identifier names the operator introduces, validated as strings
    /// at the accessor: the producer always emits the key as a list of
    /// strings, so a miss, a non-list or a non-string element is a
    /// wire-contract break that fails loudly here — the one place the
    /// invariant lives, never a per-consumer element check.
    pub identifiers: Vec<&'a str>,
    /// The child nodes' raw wire values — the consumer recurses through
    /// [`plan_node_wire`] itself, so the typed view stays shallow.
    pub children: &'a [CypherValue],
    /// The typed warnings the node carries (typed node warnings): parsed
    /// into the one [`PlanWarning`] vocabulary — an entry outside it (or
    /// a payload shape its variant does not carry) is a wire-contract
    /// break that fails loudly at the accessor, never a silently
    /// dropped warning.
    pub warnings: Vec<PlanWarning>,
}

impl PlanNodeWire<'_> {
    /// The operator's id as the width the notification side stamps
    /// (`Notification::operator_id`): the wire carries the engine's `usize`
    /// `PlanNode.id` as an `i64` (narrowed by the saturating `metric_i64`
    /// on serialize), so this is the one place the join key is widened
    /// back. A negative id is a wire-contract break and fails loudly, like
    /// the accessor's other non-negative integer fields — never a silent
    /// wrap that drops the operator's warnings.
    pub(crate) fn operator_id(&self) -> usize {
        usize::try_from(self.id).expect("plan wire Id is non-negative (ADR-0345)")
    }
}

/// The `planner-impl` value every surface sends (ADR-0357): this engine's
/// own planner, named honestly — never Neo4j's `IDP`/`COST`, which would
/// claim a planner this engine does not run.
pub const NEO4J_PLANNER_IMPL: &str = "relate";

/// The Cypher language version the plan's `version` arg reports (ADR-0357):
/// this engine's front-end is openCypher 5-compatible (ADR-0046), so the
/// Neo4j-shaped `version` arg says `5` — the language version, not a
/// product version.
pub const CYPHER_VERSION: &str = "5";

/// Read one plan wire node (ADR-0344/0345). Panics on a malformed node —
/// the wire form is engine-produced and invariant-bound, so a deviation
/// is an engine contract break, kept loud like every other boundary here.
pub fn plan_node_wire(value: &CypherValue) -> PlanNodeWire<'_> {
    let CypherValue::Map(node) = value else {
        panic!("plan wire node is a map, got {value:?}")
    };
    let operator_type = match node.get(plan_keys::OPERATOR_TYPE) {
        Some(CypherValue::Str(op)) => op.as_str(),
        other => panic!(
            "plan wire node carries a string {} (ADR-0345), got {other:?}",
            plan_keys::OPERATOR_TYPE
        ),
    };
    let args = match node.get(plan_keys::ARGS) {
        Some(CypherValue::Map(args)) => args,
        other => panic!(
            "plan wire node carries a map {} (ADR-0345), got {other:?}",
            plan_keys::ARGS
        ),
    };
    let id = match args.get(plan_keys::ID) {
        Some(CypherValue::Int(id)) => *id,
        other => panic!(
            "plan wire node carries an integer {} (ADR-0345), got {other:?}",
            plan_keys::ID
        ),
    };
    let details = match args.get(plan_keys::DETAILS) {
        Some(CypherValue::Str(details)) => details.as_str(),
        other => panic!(
            "plan wire node carries a string {} (ADR-0345), got {other:?}",
            plan_keys::DETAILS
        ),
    };
    let estimated_rows = optional_int(args, plan_keys::ESTIMATED_ROWS);
    // The five scalar metrics ride the module's one all-or-none,
    // non-negative group policy; their presence *is* the metric-set fact.
    let scalars = metric_group_ints(
        args,
        &[
            plan_keys::ROWS,
            plan_keys::TIME_NS,
            plan_keys::DB_HITS,
            plan_keys::MEMORY_BYTES,
            plan_keys::EXPRESSION_BUILT_BYTES,
        ],
    );
    let has_metrics = scalars.is_some();
    // LABS-448: the phase times ride the metric set with the kinds,
    // through the one group policy.
    let phases = metric_set_ints(
        args,
        &[plan_keys::SEEK_TIME_NS, plan_keys::WALK_TIME_NS],
        has_metrics,
    )
    .map(|values| PlanPhasesWire {
        seek_ns: values[0],
        walk_ns: values[1],
    })
    .unwrap_or_default();
    // LABS-369: the read-kind mix rides the metric set, through the one
    // group policy the phase times share — one spelling, never a
    // hand-rolled copy per field family.
    let reads = metric_set_ints(
        args,
        &[
            plan_keys::HOPS,
            plan_keys::NEIGHBOR_WALKS,
            plan_keys::NODE_READS,
        ],
        has_metrics,
    )
    .map(|values| PlanReadsWire {
        hops: values[0],
        neighbor_walks: values[1],
        node_reads: values[2],
    })
    .unwrap_or_default();
    // The walk's chosen start (LABS-369): the element, direction and
    // reason class, present only when the clause walked a pattern —
    // emitted together, like the metric set.
    let start = {
        let element = optional_int(args, plan_keys::START_ELEMENT);
        let backward = optional_bool(args, plan_keys::START_BACKWARD);
        let reason = match args.get(plan_keys::START_REASON) {
            None => None,
            Some(CypherValue::Str(reason)) => Some(
                match plan_keys::REASONS.iter().find(|r| **r == reason.as_str()) {
                    Some(name) => *name,
                    None => panic!(
                        "plan wire {} is one of {:?} (LABS-369), got {reason:?}",
                        plan_keys::START_REASON,
                        plan_keys::REASONS
                    ),
                },
            ),
            other => panic!(
                "plan wire {} is a string when present (LABS-369), got {other:?}",
                plan_keys::START_REASON
            ),
        };
        match (has_metrics, element, backward, reason) {
            (_, None, None, None) => None,
            (true, Some(element), Some(backward), Some(reason)) if element >= 0 => {
                Some(PlanStartWire {
                    element,
                    backward,
                    reason,
                })
            }
            (has_metrics, element, backward, reason) => panic!(
                "plan wire {}, {} and {} are emitted together, only on a \
                 PROFILE node (LABS-369), got metrics={has_metrics} \
                 element={element:?} backward={backward:?} reason={reason:?}",
                plan_keys::START_ELEMENT,
                plan_keys::START_BACKWARD,
                plan_keys::START_REASON
            ),
        }
    };
    // Which rung served the clause's MATCH walk (LABS-1117): the typed
    // name, validated against the one vocabulary like the start reasons —
    // present only on a PROFILE node, and only when a rung served it. The
    // chain's hop count is its own arg, and the typed value makes the
    // half-state (a chain rung without it) unrepresentable past this
    // parse: a chain without hops, hops beside another rung, or hops
    // alone are all kept loud.
    let kernel = match (has_metrics, args.get(plan_keys::KERNEL_RUNG)) {
        (_, None) => {
            if optional_int(args, plan_keys::KERNEL_HOPS).is_some() {
                panic!(
                    "plan wire {} rides only the {} key (LABS-1117)",
                    plan_keys::KERNEL_HOPS,
                    plan_keys::KERNEL_RUNG
                );
            }
            None
        }
        (true, Some(CypherValue::Str(name))) => {
            // The three payload-free rungs each reject a stray hop count;
            // the chain builds its `Chain { hops }` once from the arg.
            let payload_free = |rung: KernelRungWire| {
                if let Some(hops) = optional_int(args, plan_keys::KERNEL_HOPS) {
                    panic!(
                        "plan wire {} rides only the {} rung (LABS-1117), \
                         got rung={} hops={hops:?}",
                        plan_keys::KERNEL_HOPS,
                        KernelRungWire::CHAIN,
                        rung.as_str()
                    );
                }
                Some(rung)
            };
            match name.as_str() {
                KernelRungWire::CHAIN => match optional_int(args, plan_keys::KERNEL_HOPS) {
                    Some(hops) if hops >= 0 => Some(KernelRungWire::Chain { hops }),
                    other => panic!(
                        "plan wire {} is a non-negative integer when {} is {} \
                         (LABS-1117), got {other:?}",
                        plan_keys::KERNEL_HOPS,
                        plan_keys::KERNEL_RUNG,
                        KernelRungWire::CHAIN
                    ),
                },
                KernelRungWire::HUB_STAR => payload_free(KernelRungWire::HubStar),
                KernelRungWire::FRONTIER_NFA => payload_free(KernelRungWire::FrontierNfa),
                KernelRungWire::WALKER => payload_free(KernelRungWire::Walker),
                other => panic!(
                    "plan wire {} is one of {:?} (LABS-1117), got {other:?}",
                    plan_keys::KERNEL_RUNG,
                    plan_keys::KERNEL_RUNGS
                ),
            }
        }
        (has_metrics, other) => panic!(
            "plan wire {} is a string when present, only on a PROFILE \
             node (LABS-1117), got metrics={has_metrics} value={other:?}",
            plan_keys::KERNEL_RUNG
        ),
    };
    // Whether the clause ran ADR-0468's batch scan/filter/project chain
    // (LABS-359): the producer emits the key only when true (never a
    // `false` beside the absent fact), and only on a PROFILE node.
    let batched = match (has_metrics, args.get(plan_keys::BATCHED)) {
        (_, None) => false,
        (true, Some(CypherValue::Bool(true))) => true,
        (has_metrics, other) => panic!(
            "plan wire {} is true, only on a PROFILE node (ADR-0468, \
             LABS-359), got metrics={has_metrics} value={other:?}",
            plan_keys::BATCHED
        ),
    };
    // The batch filter's zone skips (LABS-645): emitted only when nonzero,
    // and only on a PROFILE node.
    let zone_skips = match (has_metrics, optional_int(args, plan_keys::ZONE_SKIPS)) {
        (_, None) => 0,
        (true, Some(skips)) if skips >= 0 => skips,
        (has_metrics, other) => panic!(
            "plan wire {} is a non-negative integer, only on a PROFILE \
             node (LABS-645), got metrics={has_metrics} value={other:?}",
            plan_keys::ZONE_SKIPS
        ),
    };
    // The frontier family's run counters (ADR-0478) with the NFA rung's
    // own facts: the producer emits the ten integers and the budget
    // fraction together, only when the family ran the clause, and only
    // on a PROFILE node — anything else is a wire-contract break, kept
    // loud like the batched flag's and the start trio's half-states.
    let frontier = {
        let ints = metric_group_ints(
            args,
            &[
                plan_keys::FRONTIER_EDGES,
                plan_keys::FRONTIER_LEVELS,
                plan_keys::FRONTIER_ENDPOINTS,
                plan_keys::FRONTIER_CELLS,
                plan_keys::FRONTIER_MEMO_HITS,
                plan_keys::FRONTIER_MEMO_MISSES,
                plan_keys::FRONTIER_REPRESENTATIVE_PROOFS,
                plan_keys::FRONTIER_SEARCH_PROVEN,
                plan_keys::FRONTIER_SEARCH_DISPROVEN,
                plan_keys::FRONTIER_DECLINED_EXHAUSTED,
            ],
        );
        let fraction = optional_fraction(args, plan_keys::FRONTIER_BUDGET_FRACTION);
        match (has_metrics, ints, fraction) {
            (_, None, None) => PlanFrontierWire::default(),
            (true, Some(values), Some(fraction)) => PlanFrontierWire {
                edge_visits: values[0],
                levels: values[1],
                endpoints: values[2],
                cells: values[3],
                memo_hits: values[4],
                memo_misses: values[5],
                representative_proofs: values[6],
                search_proven: values[7],
                search_disproven: values[8],
                declined_exhausted: values[9],
                budget_fraction: fraction,
            },
            (has_metrics, ints, fraction) => panic!(
                "plan wire {}..{} are non-negative integers emitted \
                 together with {}, only on a PROFILE node (ADR-0478, \
                 ADR-0516), got metrics={has_metrics} ints={ints:?} \
                 fraction={fraction:?}",
                plan_keys::FRONTIER_EDGES,
                plan_keys::FRONTIER_DECLINED_EXHAUSTED,
                plan_keys::FRONTIER_BUDGET_FRACTION
            ),
        }
    };
    // The frontier rung's run-time decline (ADR-0516), present only when
    // the rung engaged, ran, and fell through — the typed reason's
    // stable wire name, validated against the one vocabulary like the
    // start reasons.
    let frontier_decline = match (has_metrics, args.get(plan_keys::FRONTIER_DECLINE_REASON)) {
        (_, None) => None,
        (true, Some(CypherValue::Str(reason))) => Some(
            match plan_keys::DECLINE_REASONS
                .iter()
                .find(|name| **name == reason.as_str())
            {
                Some(name) => *name,
                None => panic!(
                    "plan wire {} is one of {:?} (ADR-0516), got {reason:?}",
                    plan_keys::FRONTIER_DECLINE_REASON,
                    plan_keys::DECLINE_REASONS
                ),
            },
        ),
        (has_metrics, other) => panic!(
            "plan wire {} is a string when present, only on a PROFILE \
             node (ADR-0516), got metrics={has_metrics} value={other:?}",
            plan_keys::FRONTIER_DECLINE_REASON
        ),
    };
    // The scalar group already enforced all-or-none and non-negative; the
    // wire form has one shape here, derived from the group read above.
    let metrics = scalars.map(|values| PlanMetricsWire {
        rows: values[0],
        time_ns: values[1],
        db_hits: values[2],
        memory_bytes: values[3],
        expression_built_bytes: values[4],
        reads,
        phases,
        start,
        kernel,
        batched,
        zone_skips,
        frontier,
        frontier_decline,
    });
    let children = match node.get(plan_keys::CHILDREN) {
        None => &[],
        Some(CypherValue::List(children)) => children.as_slice(),
        other => panic!(
            "plan wire {} is a list when present (ADR-0345), got {other:?}",
            plan_keys::CHILDREN
        ),
    };
    // The typed warnings (typed node warnings): warning maps — the
    // vocabulary name and, when the warning carries one, its payload —
    // anything else is a wire-contract break kept loud.
    let warnings = match node.get(plan_keys::WARNINGS) {
        None => Vec::new(),
        Some(CypherValue::List(items)) => items
            .iter()
            .map(|item| {
                let CypherValue::Map(warning) = item else {
                    panic!(
                        "plan wire {} entries are warning maps, got {item:?}",
                        plan_keys::WARNINGS
                    )
                };
                let name = match warning.get(plan_keys::WARNING_NAME) {
                    Some(CypherValue::Str(name)) => name.as_str(),
                    other => panic!(
                        "plan wire warning {} is a string, got {other:?}",
                        plan_keys::WARNING_NAME
                    ),
                };
                let index = match warning.get(plan_keys::WARNING_INDEX) {
                    None => None,
                    Some(CypherValue::Str(index)) => Some(index.as_str()),
                    other => panic!(
                        "plan wire warning {} is a string when present, got {other:?}",
                        plan_keys::WARNING_INDEX
                    ),
                };
                // The one validating match into the typed vocabulary: the
                // name must come from it, carrying the payload shape its
                // variant carries. A miss is a contract break kept loud,
                // never a silently dropped or reinterpreted warning.
                PlanWarning::from_wire_parts(name, index).unwrap_or_else(|| {
                    panic!(
                        "plan wire warnings come from the typed vocabulary \
                         (ADR-0344 typed node warnings) with the payload shape \
                         their variant carries, got {name:?}"
                    )
                })
            })
            .collect(),
        other => panic!(
            "plan wire {} is a list of warning maps when present, got {other:?}",
            plan_keys::WARNINGS
        ),
    };
    // The producer always writes the identifiers key as a list: a miss or
    // a non-list is a wire-contract break, kept loud like the node's own
    // shape (the consumer reshaping the node reads them from the typed
    // view, never re-matching the raw map).
    let identifiers: Vec<&str> = match node.get(plan_keys::IDENTIFIERS) {
        Some(CypherValue::List(identifiers)) => identifiers
            .iter()
            .map(|identifier| match identifier {
                CypherValue::Str(name) => name.as_str(),
                other => panic!(
                    "a plan wire node's {} element is a string (ADR-0345), got {other:?}",
                    plan_keys::IDENTIFIERS
                ),
            })
            .collect(),
        other => panic!(
            "plan wire node carries a list {} (ADR-0345), got {other:?}",
            plan_keys::IDENTIFIERS
        ),
    };
    PlanNodeWire {
        operator_type,
        id,
        details,
        estimated_rows,
        metrics,
        args,
        identifiers,
        children,
        warnings,
    }
}

/// The one subtree-total pass (cumulative vs self): every node's own
/// elapsed time plus every descendant's, saturated through the metric
/// width policy — ONE walk over the tree, keyed by the node's pre-order
/// id, so the table's walk and the agent document's walk read the same
/// map instead of each re-folding its subtree (one policy, one spelling).
/// Reads the typed accessor (the same validation every other consumer
/// takes), so a malformed tree fails loudly here rather than silently
/// summing partial subtrees.
pub fn plan_subtree_totals(plan: &CypherValue) -> crate::IndexMap<i64, i64> {
    let mut totals = crate::IndexMap::new();
    subtree_total_into(plan, &mut totals);
    totals
}

/// The pass's recursion: a node's own time plus its children's totals,
/// inserted under the node's id and returned as the parent's summand —
/// the saturation lands once per node (`metric_i64` into the map), never
/// an i64→u64→u128 round-trip on the way up.
fn subtree_total_into(plan: &CypherValue, totals: &mut crate::IndexMap<i64, i64>) -> u128 {
    let node = plan_node_wire(plan);
    let own = node
        .metrics
        .map_or(0u128, |m| metric_u64(m.time_ns) as u128);
    let children = node
        .children
        .iter()
        .map(|child| subtree_total_into(child, totals))
        .fold(0u128, u128::saturating_add);
    let sum = own.saturating_add(children);
    totals.insert(node.id, metric_i64(sum));
    sum
}

/// The wire form's one width policy for metric integers: saturate at
/// `i64::MAX` — one policy for every metric field (rows and time
/// alike), never an unchecked cast on one and a clamp on the other.
pub fn metric_i64(value: u128) -> i64 {
    value.min(i64::MAX as u128) as i64
}
/// The consumer side of the metric width policy: a wire metric is
/// non-negative when present (the producer saturates), so the
/// one conversion every metric cell reads goes through here — never a
/// bare `as` cast at a call site.
pub fn metric_u64(value: i64) -> u64 {
    value.max(0) as u64
}
/// The one nanoseconds-to-milliseconds conversion, as a float: every
/// millisecond presentation reads it — the table cell's `{:.3}` render,
/// the HTTP v2 node's `time` field, the agent document's `time_ms` and
/// `total_time_ms` — so the spellings cannot drift (and none truncates
/// a sub-millisecond operator to 0).
pub fn time_ns_ms(time_ns: i64) -> f64 {
    metric_u64(time_ns) as f64 / 1e6
}

/// A metrics-carrying node's presentation bundle, read from the totals
/// map — the ONE spelling of the pairing's contract ("the map covers
/// every walked node"), so the table's walk and the agent document's
/// walk cannot each re-state the lookup, and a third consumer inherits
/// it instead of cloning the panic.
pub fn profiled_facts(
    metrics: PlanMetricsWire,
    totals: &crate::IndexMap<i64, i64>,
    id: i64,
) -> PlanProfiledFacts {
    PlanProfiledFacts {
        metrics,
        total_time_ns: *totals
            .get(&id)
            .unwrap_or_else(|| panic!("the totals map covers every walked node, id {id}")),
    }
}

/// The rides-the-metric-set group policy (one loud spelling, never a
/// hand-rolled copy per field family): the keys' values are an
/// all-or-none group of non-negative integers, present only when the
/// metric set is — a partial group, a group without metrics, or a
/// negative value is a wire-contract break kept loud. `None` when the
/// whole group is absent (an EXPLAIN node).
fn metric_set_ints(
    args: &crate::IndexMap<String, CypherValue>,
    keys: &[&str],
    has_metrics: bool,
) -> Option<Vec<i64>> {
    match (has_metrics, metric_group_ints(args, keys)) {
        (false, None) => None,
        (true, Some(values)) => Some(values),
        (false, Some(_)) => panic!(
            "plan wire {keys:?} are present without the metric fields \
             (all-or-none group policy)"
        ),
        (true, None) => panic!(
            "plan wire {keys:?} are absent from the metric fields \
             (all-or-none group policy)"
        ),
    }
}

/// The all-or-none, non-negative group read — `metric_set_ints` minus its
/// metric-set gate, so the scalar metric block and the riding families
/// (read kinds, phase times) share one extraction policy. A partial group
/// or a negative value is a wire-contract break kept loud; `None` when
/// every key is absent.
fn metric_group_ints(
    args: &crate::IndexMap<String, CypherValue>,
    keys: &[&str],
) -> Option<Vec<i64>> {
    let values: Vec<Option<i64>> = keys.iter().map(|k| optional_int(args, k)).collect();
    let any_present = values.iter().any(Option::is_some);
    let all_present = values.iter().all(Option::is_some);
    match (any_present, all_present) {
        (false, _) => None,
        (true, false) => panic!(
            "plan wire {keys:?} are an all-or-none group, got a partial group: \
             values={values:?}"
        ),
        (true, true) => {
            if let Some((key, value)) = keys
                .iter()
                .zip(&values)
                .find_map(|(key, value)| value.filter(|n| *n < 0).map(|n| (*key, n)))
            {
                panic!(
                    "plan wire {key} is non-negative when present, got {value} \
                     (all-or-none non-negative group)"
                );
            }
            Some(
                values
                    .into_iter()
                    .map(|v| v.expect("the group is all-present"))
                    .collect(),
            )
        }
    }
}

impl PlanNodeWire<'_> {
    /// The node's input-row count (LABS-343's structural fact): the first
    /// child's produced rows — the bottom-up nesting puts each earlier
    /// clause under the one it feeds — `None` when the node has no input
    /// child (the statement's one seed row). The one spelling, read by
    /// the plan walk and the flag fold alike.
    pub fn input_rows(&self) -> Option<u64> {
        self.children
            .first()
            .and_then(|first| plan_node_wire(first).metrics.map(|m| metric_u64(m.rows)))
    }
}

/// The optional-integer arg — one extraction policy for every optional
/// metric key: absent or an integer, a malformed value fails loudly.
fn optional_int(args: &crate::IndexMap<String, CypherValue>, key: &str) -> Option<i64> {
    match args.get(key) {
        None => None,
        Some(CypherValue::Int(n)) => Some(*n),
        other => panic!("plan wire {key} is an integer when present (ADR-0345), got {other:?}"),
    }
}

/// The optional-boolean arg — the same policy as [`optional_int`] for the
/// one boolean fact (LABS-369's walk direction).
fn optional_bool(args: &crate::IndexMap<String, CypherValue>, key: &str) -> Option<bool> {
    match args.get(key) {
        None => None,
        Some(CypherValue::Bool(flag)) => Some(*flag),
        other => panic!("plan wire {key} is a boolean when present (LABS-369), got {other:?}"),
    }
}

/// The optional-fraction arg — [`optional_int`]'s policy for the one
/// float metric (the frontier budget fraction, ADR-0516): absent or a
/// non-negative float, a malformed value fails loudly.
fn optional_fraction(args: &crate::IndexMap<String, CypherValue>, key: &str) -> Option<f64> {
    match args.get(key) {
        None => None,
        Some(CypherValue::Float(fraction)) if *fraction >= 0.0 => Some(*fraction),
        other => {
            panic!("plan wire {key} is a non-negative float when present (ADR-0516), got {other:?}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::plan_test_fixtures::{plan_seek_wire_node, plan_wire_node};

    /// LABS-1117: a chain rung without its hop count is the same
    /// half-state the start trio rejects — the strict parse fails loudly
    /// instead of accepting a rung the contract says carries its count.
    #[test]
    #[should_panic(
        expected = "plan wire KernelHops is a non-negative integer when KernelRung is chain"
    )]
    fn a_chain_rung_without_its_hop_count_fails_the_parse() {
        let mut node = plan_wire_node("Match", 0, Some(1), vec![]);
        let CypherValue::Map(entries) = &mut node else {
            unreachable!("the fixture builds a map")
        };
        let Some(CypherValue::Map(args)) = entries.get_mut(plan_keys::ARGS) else {
            unreachable!("the fixture builds an args map")
        };
        args.insert(
            plan_keys::KERNEL_RUNG.to_string(),
            CypherValue::Str("chain".into()),
        );
        let _ = plan_node_wire(&node);
    }

    /// The one subtree-total pass (cumulative vs self): a node's own time
    /// plus every descendant's — the leaf equals its own time, the root
    /// sums the pipeline, an inner node's total lands under its id, and an
    /// unprofiled subtree contributes zero.
    #[test]
    fn subtree_totals_map_the_tree() {
        // root(10) -> [scan(3), filter(2) -> [expand(5)]]: the root's
        // total is 10 + 3 + 2 + 5 = 20; the filter heads the expand.
        let expand = plan_wire_node("Expand", 3, Some(5), vec![]);
        let filter = plan_wire_node("Filter", 2, Some(2), vec![expand]);
        let scan = plan_wire_node("NodeByLabelScan", 1, Some(3), vec![]);
        let root = plan_wire_node("ProduceResults", 0, Some(10), vec![scan, filter]);
        let totals = plan_subtree_totals(&root);
        assert_eq!(totals[&0], 20, "the root's total covers the pipeline");
        assert_eq!(totals[&2], 7, "the filter's total heads the expand");
        assert_eq!(totals[&1], 3, "a leaf's total equals its own time");
        // An unprofiled node contributes nothing — an EXPLAIN tree's
        // totals are zero, never a fabricated number.
        let explain = plan_subtree_totals(&plan_wire_node("ProduceResults", 0, None, vec![]));
        assert_eq!(explain[&0], 0);
    }

    /// ADR-0486: the index-seek operators ride the wire like every other
    /// operator — no operator-specific mapping anywhere in the form — the
    /// typed accessor reading `NodeIndexSeekByRange` with its seek-line
    /// details and its own count as the estimate.
    #[test]
    fn the_seek_operators_ride_the_wire_form() {
        let seek = plan_seek_wire_node(
            "NodeIndexSeekByRange",
            "RANGE INDEX n:User(age) WHERE age > 5, WHERE filter",
            5,
            1,
        );
        let point = plan_seek_wire_node(
            "NodeIndexSeek",
            "RANGE INDEX n:Person(name) WHERE name = 'Alice'",
            1,
            2,
        );
        let root = plan_wire_node("ProduceResults", 0, None, vec![seek, point]);
        let wire = plan_node_wire(&root);
        assert_eq!(wire.operator_type, "ProduceResults");
        let window = plan_node_wire(&wire.children[0]);
        assert_eq!(window.operator_type, "NodeIndexSeekByRange");
        assert_eq!(
            window.details,
            "RANGE INDEX n:User(age) WHERE age > 5, WHERE filter"
        );
        assert_eq!(window.estimated_rows, Some(5));
        let read = plan_node_wire(&wire.children[1]);
        assert_eq!(read.operator_type, "NodeIndexSeek");
        assert_eq!(
            read.details,
            "RANGE INDEX n:Person(name) WHERE name = 'Alice'"
        );
        assert_eq!(read.estimated_rows, Some(1));
    }
}
