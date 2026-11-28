//! The machine-native plan form (ADR-0345): the engine `Value` map in
//! Neo4j's structured-plan field shape, so agent and tooling consumers
//! that already read Neo4j plans read this engine's plans unmodified. The
//! Neo4j-facing root decoration (ADR-0357) builds on the same node and
//! args assembly, never on a re-parse of the serialized form.

use std::collections::BTreeMap;

use relate_model::wire;
use relate_model::wire::plan_keys;

use super::metric_args::{put_estimate, put_metric, put_metrics, put_optional_metric};
use super::{PlanNode, PlanWarning};
use crate::Value;

impl PlanNode {
    /// The Neo4j-facing plan value (ADR-0357) — the one boundary every
    /// surface calls before converting the value to its own wire type, so
    /// the plan metadata cannot drift between Bolt, the HTTP Query API and
    /// the CLI's machine modes. Called on a result's plan **root**: the
    /// decoration is root-only by intent —
    /// `string-representation` renders the whole statement's plan, and
    /// `planner`/`runtime`/`version`/`Memory` are root facts — and the
    /// callers hold that root as `QueryResult::plan_root()`, destructuring
    /// their own `Option` once (they need it anyway for the `is_profiled`
    /// key pick), so the signature never re-states it.
    ///
    /// `plan_text` is the surface's rendering of this plan (ADR-0359:
    /// the boundary renders through `relate-formatter`); the engine
    /// only heads it with the identity block.
    ///
    /// The typed table/speedscope renderers walk the undecorated
    /// [`PlanNode::to_value`] form instead: this decoration's float
    /// `EstimatedRows` is Neo4j-facing and never reaches the engine-form
    /// wire accessor (ADR-0345's strict integer contract).
    pub fn to_neo4j_value(&self, plan_text: &str) -> Value {
        Value::Map(std::sync::Arc::new(
            self.node_map(self.neo4j_args(plan_text)),
        ))
    }

    /// The root's args with the Neo4j-facing additions (ADR-0357), built
    /// from the same fields the base args read — never a re-parse of the
    /// serialized form: `EstimatedRows` mirrors as Neo4j's float,
    /// `Memory` mirrors the row-materialization bytes under Neo4j's arg
    /// name, `string-representation` carries the engine's one rendering of
    /// `statement` (headed by the `Cypher`/`Planner`/`Runtime` lines
    /// tooling parses), and `version` plus the planner/runtime identity
    /// name this engine honestly (never Neo4j's `COST`/`SLOTTED`).
    fn neo4j_args(&self, plan_text: &str) -> BTreeMap<String, Value> {
        let mut args = self.base_args();
        // Neo4j's `EstimatedRows` is a float; the engine's own form keeps
        // the integer — the mirror converts it so float-parsing tooling
        // (cypher-shell's summary) reads it.
        if let Some(estimate) = self.estimated_rows() {
            args.insert(
                plan_keys::ESTIMATED_ROWS.to_string(),
                Value::Float(estimate as f64),
            );
        }
        // Neo4j's root carries `Memory`; this engine's root metric is the
        // row-materialization bytes — the same number the `Memory (Bytes)`
        // column renders, mirrored under Neo4j's arg name.
        if let Some(metrics) = self.metrics {
            args.insert(
                plan_keys::MEMORY.to_string(),
                Value::Int(wire::metric_i64(u128::from(metrics.memory_bytes))),
            );
        }
        let planner = wire::NEO4J_PLANNER_IMPL;
        let version = env!("CARGO_PKG_VERSION");
        // The representation's header block is an engine fact (the honest
        // planner/runtime identity); the plan text under it is the
        // boundary's rendering (ADR-0359), passed in by the surface that
        // rendered it through `relate-formatter`.
        let representation = format!(
            "Cypher {cypher_version}\n\nPlanner {planner}\n\nRuntime {planner}\n\nRuntime \
             version {version}\n\n{plan_text}",
            cypher_version = wire::CYPHER_VERSION,
        );
        args.insert(
            plan_keys::STRING_REPRESENTATION.to_string(),
            Value::String(representation.into()),
        );
        for (key, value) in [
            (plan_keys::VERSION, wire::CYPHER_VERSION.to_string()),
            (plan_keys::PLANNER, planner.to_string()),
            (plan_keys::PLANNER_IMPL, planner.to_string()),
            (plan_keys::PLANNER_VERSION, version.to_string()),
            (plan_keys::RUNTIME, planner.to_string()),
            (plan_keys::RUNTIME_IMPL, planner.to_string()),
            (plan_keys::RUNTIME_VERSION, version.to_string()),
        ] {
            args.insert(key.to_string(), Value::String(value.into()));
        }
        args
    }

    /// The machine-native form (ADR-0345): an engine `Value` map in
    /// Neo4j's structured-plan field shape — `operatorType`, `args` (the
    /// node's id, details, estimate), `identifiers`, `children` — so agent
    /// and tooling consumers that already read Neo4j plans read this
    /// engine's plans unmodified, and the later `PROFILE` phases add their
    /// metrics as further `args` entries. Surfaces convert to their wire
    /// value type (`plan_value_to_cypher`, the plan-wire crossing under
    /// its walk-sized budget, ADR-0628) at their own boundary — gated by
    /// [`Self::ensure_plan_wire_budget`] first, so this recursive build
    /// never runs on a past-budget tree.
    pub fn to_value(&self) -> Value {
        Value::Map(std::sync::Arc::new(self.node_map(self.base_args())))
    }

    /// The tree's container depth in the plan-wire crossing's accounting
    /// (ADR-0628's probe): each node contributes its map (one level) and,
    /// when it carries children, the list around them (one more), so the
    /// deepest chain of a real plan is the children chain; every other
    /// branch (the `args` map, `identifiers`, the warnings) holds scalars
    /// and never dominates a tree at the budget's scale. **Iterative** —
    /// an explicit worklist over the children slices, no recursion, no
    /// building, no allocation beyond the worklist — so probing a
    /// past-budget tree costs nothing on the caller's stack. Every
    /// plan-crossing boundary runs this before any recursive build (see
    /// [`Self::ensure_plan_wire_budget`]).
    pub fn plan_wire_depth(&self) -> u32 {
        let mut max = 0u32;
        // (node, its own level): the root's map is level 1.
        let mut work: Vec<(&PlanNode, u32)> = vec![(self, 1)];
        while let Some((node, level)) = work.pop() {
            max = max.max(level);
            // The children's list is one level deeper than this node's
            // map, each child's map one deeper still.
            work.extend(node.children.iter().map(|child| (child, level + 2)));
        }
        max
    }

    /// The plan-wire budget check (ADR-0628): `Ok` when this tree's
    /// [`Self::plan_wire_depth`] sits within `MAX_PLAN_WIRE_DEPTH`, else
    /// the typed capacity refusal every plan-crossing boundary raises
    /// **before** any recursive build (`to_value`/`to_neo4j_value`) or
    /// crossing runs — the abort class the security round closed: those
    /// builds recurse per plan operator on the caller's thread, so a
    /// past-budget plan must refuse here, from the typed tree, without
    /// ever being built.
    pub fn ensure_plan_wire_budget(&self) -> crate::Result<()> {
        let depth = self.plan_wire_depth();
        if depth > crate::MAX_PLAN_WIRE_DEPTH {
            Err(crate::Error::capacity_plan_wire(
                u64::from(depth),
                u64::from(crate::MAX_PLAN_WIRE_DEPTH),
            ))
        } else {
            Ok(())
        }
    }

    /// The node's base args (ADR-0345): `Id` (the pre-order number), the
    /// `Details` string, the estimate when the estimate phase filled it,
    /// and the PROFILE metrics set when the node carries one — one
    /// construction the undecorated and the Neo4j-facing forms both build
    /// from, so they cannot drift.
    fn base_args(&self) -> BTreeMap<String, Value> {
        let mut args: BTreeMap<String, Value> = [(
            plan_keys::DETAILS.to_string(),
            Value::String(self.details.as_str().into()),
        )]
        .into_iter()
        .collect();
        // Every plan integer goes through the one width policy
        // (`put_metric`'s `metric_i64`: saturate at the i64 boundary,
        // never an unchecked cast).
        put_metric(&mut args, plan_keys::ID, self.id as u128);
        put_estimate(&mut args, self.estimate);
        put_optional_metric(
            &mut args,
            plan_keys::PEAK_LOGICAL_BYTES,
            self.token_estimate
                .map(|peak| u128::from(peak.peak_logical_bytes)),
        );
        // LABS-1117: the store's static pattern-engine rung on a MATCH
        // operator — the one engagement fact EXPLAIN can state.
        if let Some(engine) = self.pattern_engine {
            args.insert(
                plan_keys::PATTERN_ENGINE.to_string(),
                Value::String(engine.as_str().into()),
            );
        }
        // Virtual plan operators: a fused child names its parent (the
        // `FusedInto` key's presence IS the fused fact — no separate
        // flag) and carries only the metrics a real counter measured
        // for its step (the seek pair) — never the PROFILE metric
        // block, which stays the fused parent's clause totals. The
        // never-blocks half is enforced here, at the production
        // boundary: a node the engine cannot write is one its own
        // parser can never reject.
        if let Some(fused) = self.fused() {
            assert!(
                self.metrics.is_none(),
                "a fused virtual child never carries the operator metric block"
            );
            put_metric(&mut args, plan_keys::FUSED_INTO, fused.parent as u128);
            if let Some(metrics) = fused.metrics {
                put_metric(&mut args, plan_keys::SEEK_TIME_NS, metrics.seek_ns);
                put_metric(
                    &mut args,
                    plan_keys::NODE_READS,
                    u128::from(metrics.node_reads),
                );
            }
        }
        // REL8-2047: a batched body subtree's root names its
        // `CallSubquery` parent — the presence of `BodyOf` IS the body
        // role, so a walk tells it from the data-flow child.
        if let Some(parent) = self.body_of() {
            // ADR-0681: a batched body's operators run inside the
            // `CallSubquery` operator's batch, so their work is that
            // operator's record — the body role carries no metric block
            // of its own, the fused role's rule, enforced here at the
            // production boundary.
            assert!(
                self.metrics.is_none(),
                "a batched body operator never carries the operator metric block"
            );
            put_metric(&mut args, plan_keys::BODY_OF, parent as u128);
        }
        // PROFILE metrics (ADR-0344 phase 2 + metric fields): present
        // only on PROFILE trees, emitted together — through the one
        // emission helper every metrics feature extends.
        if let Some(metrics) = self.metrics {
            put_metrics(&mut args, &metrics);
        }
        args
    }

    /// The node's map (ADR-0345) around `args`: `operatorType`, `args`,
    /// `identifiers`, the children as undecorated [`PlanNode::to_value`]
    /// subtrees (the decoration is root-only, ADR-0357) and the typed
    /// warnings. `to_value` and `to_neo4j_value` share this assembly, so
    /// the two forms cannot drift.
    fn node_map(&self, args: BTreeMap<String, Value>) -> BTreeMap<String, Value> {
        let mut node: BTreeMap<String, Value> = [
            (
                plan_keys::OPERATOR_TYPE.to_string(),
                Value::String(self.operator.as_str().into()),
            ),
            (
                plan_keys::ARGS.to_string(),
                Value::Map(std::sync::Arc::new(args)),
            ),
            (
                plan_keys::IDENTIFIERS.to_string(),
                Value::List(
                    self.identifiers
                        .iter()
                        .map(|name| Value::String(name.as_str().into()))
                        .collect::<Vec<_>>()
                        .into(),
                ),
            ),
        ]
        .into_iter()
        .collect();
        if !self.children.is_empty() {
            node.insert(
                plan_keys::CHILDREN.to_string(),
                Value::List(
                    self.children
                        .iter()
                        .map(|child| child.to_value())
                        .collect::<Vec<_>>()
                        .into(),
                ),
            );
        }
        // The typed warnings (typed node warnings): serialized as warning
        // maps — the shared vocabulary's name plus the warning's payload
        // (the `LabelDisjunctionScan` warning's index pairs) when it carries
        // one, present only when the node carries one, so the wire
        // consumer renders the engine's own message text.
        if !self.warnings.is_empty() {
            node.insert(
                plan_keys::WARNINGS.to_string(),
                Value::List(
                    self.warnings
                        .iter()
                        .map(warning_value)
                        .collect::<Vec<_>>()
                        .into(),
                ),
            );
        }
        node
    }
}

/// One warning's wire map (ADR-0345): the shared vocabulary's `name` plus
/// the warning's payload — the `LabelDisjunctionScan` warning's index pairs —
/// when the typed warning carries one. The one spelling of the warning
/// wire shape, beside [`PlanNode::node_map`] which assembles it.
fn warning_value(warning: &PlanWarning) -> Value {
    let mut fields: BTreeMap<String, Value> = [(
        plan_keys::WARNING_NAME.to_string(),
        Value::String(warning.name().into()),
    )]
    .into_iter()
    .collect();
    if let Some(index) = warning.suggested_index() {
        fields.insert(
            plan_keys::WARNING_INDEX.to_string(),
            Value::String(index.into()),
        );
    }
    Value::Map(std::sync::Arc::new(fields))
}

#[cfg(test)]
mod tests {
    use super::super::{ClauseMetrics, PlanWarning};
    use super::*;

    /// The wire warnings on a node carrying one: the typed vocabulary's
    /// names, by name — never a free-form string.
    #[test]
    fn to_value_serializes_warnings_by_name() {
        let node = PlanNode::new("AllNodesScan", "()".to_string(), Vec::new(), Vec::new())
            .with_warnings(vec![PlanWarning::AllNodesScan]);
        // Round-trip through the wire accessor's one validating parse
        // (`from_wire_parts`): the name is the wire spelling and the
        // warning carries no payload.
        let wire_value = relate_core::value_to_cypher(&node.to_value());
        assert_eq!(
            wire::plan_node_wire(&wire_value).warnings,
            vec![PlanWarning::AllNodesScan]
        );
        let with_payload =
            PlanNode::new("NodeByLabelScan", "()".to_string(), Vec::new(), Vec::new())
                .with_warnings(vec![PlanWarning::LabelDisjunctionScan {
                    indexes: ":A(x), :B(x)".to_string(),
                }]);
        assert_eq!(
            wire::plan_node_wire(&relate_core::value_to_cypher(&with_payload.to_value())).warnings,
            vec![PlanWarning::LabelDisjunctionScan {
                indexes: ":A(x), :B(x)".to_string()
            }],
            "the payload survives the round trip"
        );
        // A node with no warnings carries no `warnings` key — never an
        // empty list fabricated beside the absent ones.
        let clean = PlanNode::new(
            "ProduceResults",
            "1 item(s)".to_string(),
            Vec::new(),
            Vec::new(),
        );
        let Value::Map(clean) = clean.to_value() else {
            panic!("the wire node is a map");
        };
        assert!(clean.get(plan_keys::WARNINGS).is_none());
    }

    /// REL8-2047: the batched body role survives the wire round trip —
    /// `BodyOf` carries the `CallSubquery` parent's id, and the node is
    /// not an executed child on either side of the crossing.
    #[test]
    fn to_value_round_trips_the_batched_body_role() {
        let call = PlanNode::new("CallSubquery", String::new(), Vec::new(), Vec::new());
        let body = PlanNode::new("Create", "(:BC)".to_string(), Vec::new(), Vec::new());
        let mut root = call;
        root.children.push(body.with_body(root.id));
        let wire_value = relate_core::value_to_cypher(&root.to_value());
        let parsed = wire::plan_node_wire(&wire_value);
        let body = wire::plan_node_wire(&parsed.children[0]);
        assert_eq!(
            body.body_of,
            Some(parsed.id),
            "the body role survives the round trip"
        );
        assert!(
            parsed.executed_children().next().is_none(),
            "a batched body's operators are not executed children"
        );
    }

    /// REL8-2047: the `IN TRANSACTIONS` batch facts survive the wire round
    /// trip — the seven integers emitted together, present only when the
    /// arm ran batches (a zeroed-facts operator emits no key).
    #[test]
    fn to_value_round_trips_the_in_tx_batch_facts() {
        let intx = relate_executor::InTxBatches {
            batches: 4,
            committed: 3,
            rolled_back: 1,
            peak_input_rows_bytes: 900,
            peak_output_rows_bytes: 800,
            peak_scratch_bytes: 700,
            peak_wal_bytes: 600,
        };
        let base = zeroed_metrics();
        let mut node = PlanNode::new("CallSubquery", String::new(), Vec::new(), Vec::new());
        node.metrics = Some(ClauseMetrics { intx, ..base });
        let wire_value = relate_core::value_to_cypher(&node.to_value());
        let parsed = wire::plan_node_wire(&wire_value);
        assert_eq!(
            parsed.metrics.expect("the node carries metrics").intx,
            wire::PlanInTxWire {
                batches: 4,
                committed: 3,
                rolled_back: 1,
                peak_input_rows_bytes: 900,
                peak_output_rows_bytes: 800,
                peak_scratch_bytes: 700,
                peak_wal_bytes: 600,
            },
            "the batch facts survive the round trip"
        );
        // An arm that ran no batch emits no key: the zeroed facts never
        // appear as a group of zeros on the wire.
        node.metrics = Some(base);
        let args = match node.to_value() {
            Value::Map(node) => match node.get(plan_keys::ARGS) {
                Some(Value::Map(args)) => args.as_ref().clone(),
                other => panic!("the node carries args: {other:?}"),
            },
            other => panic!("the wire node is a map: {other:?}"),
        };
        for key in plan_keys::IN_TX_GROUP_KEYS {
            assert!(
                !args.contains_key(key),
                "no {key} without a batch: {args:?}"
            );
        }
    }

    /// The metrics fixture the batch-facts round trip's two arms share.
    fn zeroed_metrics() -> ClauseMetrics {
        ClauseMetrics {
            rows: 9,
            time_ns: 1,
            ..Default::default()
        }
    }

    /// ADR-0708: the provenance key rides with the estimate, decodes
    /// through the wire accessor, and is absent without an estimate; the
    /// name table matches the typed vocabulary.
    #[test]
    fn to_value_emits_the_estimate_provenance_with_the_estimate() {
        use relate_model::wire::{plan_node_wire, EstimateProvenance};
        let estimated = PlanNode::new("AllNodesScan", "(n)".to_string(), Vec::new(), Vec::new())
            .with_estimate(7, EstimateProvenance::Defaulted);
        let wire = estimated.to_value();
        let cypher = relate_core::value_to_cypher(&wire);
        let node = plan_node_wire(&cypher);
        assert_eq!(node.estimated_rows, Some(7));
        assert_eq!(
            node.estimate_provenance,
            Some(EstimateProvenance::Defaulted)
        );
        let bare = PlanNode::new("AllNodesScan", "(n)".to_string(), Vec::new(), Vec::new());
        let bare_cypher = relate_core::value_to_cypher(&bare.to_value());
        assert_eq!(plan_node_wire(&bare_cypher).estimate_provenance, None);
        // The spellings are pinned literally, through an exhaustive match: a
        // renamed wire name, or a new variant without a pinned name, fails here.
        for provenance in [
            EstimateProvenance::Measured,
            EstimateProvenance::Sampled,
            EstimateProvenance::Statistic,
            EstimateProvenance::Defaulted,
        ] {
            let pinned = match provenance {
                EstimateProvenance::Measured => "measured",
                EstimateProvenance::Sampled => "sampled",
                EstimateProvenance::Statistic => "statistic",
                EstimateProvenance::Defaulted => "defaulted",
            };
            assert_eq!(provenance.as_str(), pinned);
            assert_eq!(EstimateProvenance::from_name(pinned), Some(provenance));
            assert!(EstimateProvenance::NAMES.contains(&pinned));
        }
    }

    /// The Neo4j-facing entry is root-shaped (ADR-0357): the root carries
    /// the Neo4j-facing args while its children keep the engine's own form —
    /// `EstimatedRows` an integer on the child, a float only on the root.
    #[test]
    fn to_neo4j_value_decorates_the_root_and_keeps_children_undecorated() {
        let mut child = PlanNode::new("AllNodesScan", "(n)".to_string(), Vec::new(), Vec::new());
        child.estimate = Some(crate::PlanEstimate::measured(7));
        let mut root = PlanNode::new(
            "ProduceResults",
            "1 item(s)".to_string(),
            Vec::new(),
            vec![child],
        );
        root.estimate = Some(crate::PlanEstimate::measured(3));

        let Value::Map(root_map) = root.to_neo4j_value("Query: RETURN 1\nPlan:") else {
            panic!("a root decorates to a map");
        };
        let Some(Value::Map(root_args)) = root_map.get(plan_keys::ARGS) else {
            panic!("the decorated root carries an args map");
        };
        assert_eq!(
            root_args.get(plan_keys::ESTIMATED_ROWS),
            Some(&Value::Float(3.0)),
            "the root's estimate mirrors Neo4j's float"
        );
        assert!(
            matches!(root_args.get(plan_keys::PLANNER), Some(Value::String(name)) if name.as_ref() == "relate"),
            "the root names this engine honestly: {root_args:?}"
        );
        let Some(Value::List(children)) = root_map.get(plan_keys::CHILDREN) else {
            panic!("the decorated root keeps its children");
        };
        let Some(Value::Map(child_map)) = children.first() else {
            panic!("a child is a map");
        };
        let Some(Value::Map(child_args)) = child_map.get(plan_keys::ARGS) else {
            panic!("the child carries an args map");
        };
        assert_eq!(
            child_args.get(plan_keys::ESTIMATED_ROWS),
            Some(&Value::Int(7)),
            "a child keeps the engine's integer estimate"
        );
        assert!(
            child_args.get(plan_keys::PLANNER).is_none(),
            "the decoration never reaches a child: {child_args:?}"
        );
    }

    /// The vocabulary spellings stay in step: every decline the PROFILE
    /// record can carry (`FrontierRunDecline::as_str`, then
    /// `FrontierDecline::as_str`) is exactly the wire's vocabulary
    /// (`plan_keys::DECLINE_REASONS`), in order —
    /// the start reasons the same way (`StartReason::as_str` against
    /// `plan_keys::REASONS`), and the traversal rungs (`KernelRung::as_str`
    /// against `plan_keys::KERNEL_RUNGS`, itself derived from the typed
    /// `KernelRungWire`). A renamed or added variant fails here, not at a
    /// PROFILE consumer's decode.
    #[test]
    fn decline_and_start_reason_names_match_the_wire_tables() {
        use relate_executor::{FrontierDecline, FrontierRunDecline, KernelRung, StartReason};
        // Exhaustive matches tie the iterated lists below to the enums:
        // a new variant fails to compile here until it is listed.
        let _ = |reason: FrontierRunDecline| match reason {
            FrontierRunDecline::RearrivalBudget
            | FrontierRunDecline::TableBudget
            | FrontierRunDecline::ShortCap => (),
        };
        let _ = |reason: FrontierDecline| match reason {
            FrontierDecline::MultiplePatterns
            | FrontierDecline::CarryingRow
            | FrontierDecline::Mode
            | FrontierDecline::NestedSpine
            | FrontierDecline::AllReduceGroup
            | FrontierDecline::PinnedBinding
            | FrontierDecline::Conjunct
            | FrontierDecline::NfaShape
            | FrontierDecline::GuardedGroup
            | FrontierDecline::FullEnumeration
            | FrontierDecline::OrderedCap
            | FrontierDecline::PathCount
            | FrontierDecline::PathConsumer
            | FrontierDecline::Cost => (),
        };
        let _ = |reason: StartReason| match reason {
            StartReason::BoundPivot
            | StartReason::Cheapest
            | StartReason::SeekAnchored
            | StartReason::IndexZero => (),
        };
        let _ = |rung: KernelRung| match rung {
            KernelRung::Chain { .. }
            | KernelRung::HubStar
            | KernelRung::FrontierNfa
            | KernelRung::Walker => (),
        };
        let declines: Vec<&str> = FrontierRunDecline::ALL
            .iter()
            .map(|reason| reason.as_str())
            .chain(FrontierDecline::ALL.iter().map(|reason| reason.as_str()))
            .collect();
        assert_eq!(declines, plan_keys::DECLINE_REASONS);
        let starts: Vec<&str> = [
            StartReason::BoundPivot,
            StartReason::Cheapest,
            StartReason::SeekAnchored,
            StartReason::IndexZero,
        ]
        .iter()
        .map(|reason| reason.as_str())
        .collect();
        assert_eq!(starts, plan_keys::REASONS);
        let rungs: Vec<&str> = [
            KernelRung::Chain { hops: 3 },
            KernelRung::HubStar,
            KernelRung::FrontierNfa,
            KernelRung::Walker,
        ]
        .iter()
        .map(|rung| rung.as_str())
        .collect();
        assert_eq!(rungs, plan_keys::KERNEL_RUNGS);
    }

    /// ADR-0628's probe: the children-chain depth the plan-wire crossing
    /// counts — each node its map, the children list around the next —
    /// returned by an iterative DFS whose worklist is its only
    /// allocation. The 50 000-node chain pins the no-recursion shape
    /// twice over: a recursive probe over it would overflow a test
    /// thread's stack, and — dropped here on the ordinary test thread —
    /// it is the iterative `PlanNode` drop's regression proof (the
    /// derived recursive drop overflows at this depth).
    #[test]
    fn plan_wire_depth_counts_the_children_chain_iteratively() {
        let chain = |nodes: usize| {
            let mut root = PlanNode::new("ProduceResults", String::new(), Vec::new(), Vec::new());
            for _ in 1..nodes {
                root = PlanNode::new("Create", String::new(), Vec::new(), vec![root]);
            }
            root
        };
        assert_eq!(chain(1).plan_wire_depth(), 1, "a lone node is its own map");
        assert_eq!(
            chain(3).plan_wire_depth(),
            5,
            "a map and a children list per link"
        );
        // The deep chain, dropped at the end of this scope on the ordinary
        // test thread: with the iterative `Drop` that is a constant-stack
        // drain — the derived recursive drop overflows a test thread at
        // this depth, so this drop is the drain's regression proof as
        // much as the probe's own iterativity is.
        let deep = chain(50_000);
        assert_eq!(
            deep.plan_wire_depth(),
            2 * 50_000 - 1,
            "the deep chain, probed iteratively"
        );
        drop(deep);
        // The budget check over the same facts: a chain within the budget
        // passes, one past it takes the typed refusal.
        assert!(
            chain(2).ensure_plan_wire_budget().is_ok(),
            "a three-level chain sits far under the budget"
        );
        let past = chain(crate::MAX_PLAN_WIRE_DEPTH as usize / 2 + 2);
        assert_eq!(
            past.plan_wire_depth(),
            2 * (crate::MAX_PLAN_WIRE_DEPTH / 2 + 2) - 1
        );
        match past.ensure_plan_wire_budget() {
            Err(crate::Error::Capacity {
                kind: crate::CapacityKind::PlanWire,
                limit,
                ..
            }) => {
                assert_eq!(
                    limit,
                    u64::from(crate::MAX_PLAN_WIRE_DEPTH),
                    "the refusal names the budget"
                );
            }
            other => panic!("a past-budget tree takes the typed refusal: {other:?}"),
        }
    }
}
