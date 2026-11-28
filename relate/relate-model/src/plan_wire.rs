//! The structured query plan wire form (ADR-0344/0345): the typed plan
//! node accessor and the plan contract the presentation and the engine's
//! warning construction read (ADR-0400 — the table layout itself lives
//! with its one consumer in the formatter) — one spelling so no surface
//! can drift from the wire form it reads. The key vocabulary lives in its
//! own module (`plan_keys.rs`), and the typed warning vocabulary beside it
//! (LABS-461: `plan_warning.rs`), parsed here by the node accessor.

use super::plan_args::{
    metric_group_ints, metric_set_ints, optional_bool, optional_fraction, optional_int,
};
use super::plan_keys;
use super::plan_metrics_wire::{
    KernelRungWire, PlanFrontierWire, PlanInTxWire, PlanMetricsWire, PlanPhasesWire,
    PlanProfiledFacts, PlanReadsWire, PlanStartWire,
};
use super::plan_warning::PlanWarning;
use crate::CypherValue;

// ---------------------------------------------------------------------------
// Structured query plan wire form (ADR-0344/0345)
// ---------------------------------------------------------------------------

/// The one spelling of a plan column's unavailable value, shared by every
/// presentation of the plan table (the engine renderer and the surfaces'
/// table layout) so "column not produced" renders identically everywhere.
pub const PLAN_UNAVAILABLE: &str = "-";

// The PROFILE metric-wire family — the rung enum, the metric set, the
// read-kind/phase/start/frontier/IN-TRANSACTIONS shapes it composes, and
// the profiled-facts presentation bundle — lives in its own module
// beside this one (`plan_metrics_wire.rs`, REL8-2047's split), like the
// fused-child, memo and warning shapes; the node wire form below reads
// it here.

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
    /// The ladder level behind `estimated_rows` (ADR-0708); present
    /// exactly when the estimate is.
    pub estimate_provenance: Option<crate::wire::EstimateProvenance>,
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
    /// The fused virtual child fact (virtual plan operators): `Some` on
    /// a display-only breakdown step of its fused parent operator, never
    /// an executed one — the parent link and the step's own measured
    /// pair (the seek step's seek-phase time and node reads; `None` on a
    /// hop step, which no real per-hop counter measures) travel
    /// together, so the half-states are unrepresentable. Never the
    /// parent's rows, time or memory.
    pub fused: Option<super::plan_fused::FusedChildWire>,
    /// The batched `CALL { … } IN TRANSACTIONS` body role marker
    /// (REL8-2047): the `CallSubquery` parent's id on the root of a body
    /// subtree, `None` on every node of the statement's own pipeline —
    /// the role fact [`Self::input_rows`] and [`Self::executed_children`]
    /// read so a body operator is never taken for the data-flow child.
    pub body_of: Option<i64>,
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
    let estimate_provenance = match (estimated_rows, args.get(plan_keys::ESTIMATE_PROVENANCE)) {
        (None, None) => None,
        (Some(rows), None) => panic!(
            "plan wire node carries {} only with {} (ADR-0708), got {rows} alone",
            plan_keys::ESTIMATED_ROWS,
            plan_keys::ESTIMATE_PROVENANCE
        ),
        (Some(_), Some(CypherValue::Str(name))) => Some(
            crate::wire::EstimateProvenance::from_name(name.as_str()).unwrap_or_else(|| {
                panic!(
                    "plan wire node carries a known {} (ADR-0708), got {name:?}",
                    plan_keys::ESTIMATE_PROVENANCE
                )
            }),
        ),
        (estimate, other) => panic!(
            "plan wire node carries {} only with {} as a string (ADR-0708), got {other:?} beside {estimate:?}",
            plan_keys::ESTIMATE_PROVENANCE,
            plan_keys::ESTIMATED_ROWS
        ),
    };
    // Virtual plan operators: the fused fact gates the metric groups — a
    // fused child carries only the step's own measured pair, so the
    // ride-along groups (the phase times, the read kinds) stay the fused
    // parent's and never parse here.
    let fused = super::plan_fused::fused_child(args);
    // REL8-2047: the body role marker, the fused fact's optional-int
    // shape — its presence is the whole "this node is a batched body's
    // operator" fact. Like a fused child, a body operator runs inside its
    // parent's batch, so it never carries the PROFILE metric block
    // (ADR-0681): a half-state here is rejected loudly, never silently
    // summed into the parent's subtree total.
    let body_of = optional_int(args, plan_keys::BODY_OF);
    if body_of.is_some() {
        for key in plan_keys::METRIC_BLOCK_KEYS.iter() {
            if args.contains_key(*key) {
                panic!(
                    "plan wire node carries {} beside {} (REL8-2047): a batched \
                     body operator's work is its CallSubquery parent's batch \
                     record, so it never carries the metric block",
                    key,
                    plan_keys::BODY_OF
                );
            }
        }
    }
    // The metric block's five scalar keys ride the module's one all-or-none
    // group policy ([`plan_keys::METRIC_BLOCK_KEYS`]); their presence *is*
    // the metric-set fact. A fused child's parser already rejected them.
    let scalars = metric_group_ints(args, plan_keys::METRIC_BLOCK_KEYS);
    let has_metrics = scalars.is_some();
    // LABS-448: the phase times ride the metric set with the kinds,
    // through the one group policy.
    let phases = if fused.is_some() {
        PlanPhasesWire::default()
    } else {
        metric_set_ints(
            args,
            [plan_keys::SEEK_TIME_NS, plan_keys::WALK_TIME_NS],
            has_metrics,
        )
        .map(|[seek_ns, walk_ns]| PlanPhasesWire { seek_ns, walk_ns })
        .unwrap_or_default()
    };
    // LABS-369: the read-kind mix rides the metric set, through the one
    // group policy the phase times share — one spelling, never a
    // hand-rolled copy per field family.
    let reads = if fused.is_some() {
        PlanReadsWire::default()
    } else {
        metric_set_ints(
            args,
            [
                plan_keys::HOPS,
                plan_keys::NEIGHBOR_WALKS,
                plan_keys::NODE_READS,
            ],
            has_metrics,
        )
        .map(|[hops, neighbor_walks, node_reads]| PlanReadsWire {
            hops,
            neighbor_walks,
            node_reads,
        })
        .unwrap_or_default()
    };
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
            (true, Some(element), Some(backward), Some(reason)) => Some(PlanStartWire {
                element,
                backward,
                reason,
            }),
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
                    Some(hops) => Some(KernelRungWire::Chain { hops }),
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
        (true, Some(skips)) => skips,
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
        let ints = metric_group_ints(args, plan_keys::FRONTIER_GROUP_KEYS);
        let fraction = optional_fraction(args, plan_keys::FRONTIER_BUDGET_FRACTION);
        match (has_metrics, ints, fraction) {
            (_, None, None) => PlanFrontierWire::default(),
            (
                true,
                Some(
                    [edge_visits, levels, endpoints, cells, memo_hits, memo_misses, representative_proofs, search_proven, search_disproven, declined_exhausted],
                ),
                Some(budget_fraction),
            ) => PlanFrontierWire {
                edge_visits,
                levels,
                endpoints,
                cells,
                memo_hits,
                memo_misses,
                representative_proofs,
                search_proven,
                search_disproven,
                declined_exhausted,
                budget_fraction,
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
    // The expression-memo group (ADR-0654), read and validated by its
    // own module.
    let memo = super::plan_memo::memo_group(args, has_metrics);
    // The `IN TRANSACTIONS` batch facts (REL8-2047): the producer emits
    // the seven integers together, only when the arm ran batches and only
    // on a PROFILE node — the all-or-none group read, kept loud like the
    // frontier family's.
    let intx = {
        let ints = metric_group_ints(args, plan_keys::IN_TX_GROUP_KEYS);
        match (has_metrics, ints) {
            (_, None) => PlanInTxWire::default(),
            (
                true,
                Some(
                    [batches, committed, rolled_back, peak_input_rows_bytes, peak_output_rows_bytes, peak_scratch_bytes, peak_wal_bytes],
                ),
            ) => PlanInTxWire {
                batches,
                committed,
                rolled_back,
                peak_input_rows_bytes,
                peak_output_rows_bytes,
                peak_scratch_bytes,
                peak_wal_bytes,
            },
            (has_metrics, ints) => panic!(
                "plan wire {}..{} are non-negative integers emitted \
                 together, only on a PROFILE node (REL8-2047), got \
                 metrics={has_metrics} ints={ints:?}",
                plan_keys::IN_TX_BATCHES,
                plan_keys::IN_TX_PEAK_WAL_BYTES
            ),
        }
    };
    // The scalar group already enforced all-or-none and non-negative; the
    // wire form has one shape here, derived from the group read above.
    let metrics = scalars.map(
        |[rows, time_ns, db_hits, memory_bytes, expression_built_bytes]| PlanMetricsWire {
            rows,
            time_ns,
            db_hits,
            memory_bytes,
            expression_built_bytes,
            reads,
            phases,
            start,
            kernel,
            batched,
            zone_skips,
            frontier,
            frontier_decline,
            memo,
            intx,
        },
    );
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
        estimate_provenance,
        metrics,
        args,
        identifiers,
        children,
        warnings,
        fused,
        body_of,
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

impl<'a> PlanNodeWire<'a> {
    /// The node's input-row count (LABS-343's structural fact): the first
    /// child's produced rows — the bottom-up nesting puts each earlier
    /// clause under the one it feeds — `None` when the node has no input
    /// child (the statement's one seed row). The one spelling, read by
    /// the plan walk and the flag fold alike.
    pub fn input_rows(&self) -> Option<u64> {
        // The data-flow child is the first executed child
        // ([`Self::is_executed_child`]): a `CALL { … } IN TRANSACTIONS`
        // as the clause with no preceding clause carries only a body
        // child, whose rows are not this operator's input.
        self.executed_children()
            .next()
            .and_then(|child| child.metrics.map(|m| metric_u64(m.rows)))
    }

    /// Whether this node is an executed child of its parent — the one
    /// child-role rule both walks read. Two child roles are skipped: a
    /// display-only fused breakdown step (ADR-0622), whose measured pair
    /// is a phase of the fused parent's clause totals, and a batched
    /// `CALL { … } IN TRANSACTIONS` body's operator root (REL8-2047),
    /// whose work is the `CallSubquery` operator's batch record.
    pub fn is_executed_child(&self) -> bool {
        self.fused.is_none() && self.body_of.is_none()
    }

    /// The node's executed children: every child
    /// [`Self::is_executed_child`] admits — the one skip policy, so a
    /// walk reading executed operators or their PROFILE metrics never
    /// counts a display-only fused step or a batched body's operators a
    /// second time.
    pub fn executed_children(&self) -> impl Iterator<Item = PlanNodeWire<'a>> + 'a {
        self.children
            .iter()
            .map(plan_node_wire)
            .filter(|child| child.is_executed_child())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::plan_test_fixtures::{args_mut, plan_seek_wire_node, plan_wire_node};

    /// LABS-1117: a chain rung without its hop count is the same
    /// half-state the start trio rejects — the strict parse fails loudly
    /// instead of accepting a rung the contract says carries its count.
    #[test]
    #[should_panic(
        expected = "plan wire KernelHops is a non-negative integer when KernelRung is chain"
    )]
    fn a_chain_rung_without_its_hop_count_fails_the_parse() {
        let mut node = plan_wire_node("Match", 0, Some(1), vec![]);
        args_mut(&mut node).insert(
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
