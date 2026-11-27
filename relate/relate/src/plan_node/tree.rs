//! The plan tree's stage and part assembly (REL8-2047's split from
//! `mod.rs`, the plan-node hub): the [`StageView`] both stage sources
//! implement — the statement's `LogicalPlan` and a `CALL { … } IN
//! TRANSACTIONS` body's `Query` — the one [`stage_tree`] assembler both
//! trees' stages go through, and the [`part_tree`] pipeline every stage's
//! parts build as, so the mutually recursive build and the pre-order id
//! contract it shares live in one file. Neither source's tree can drift
//! from the other's.

use relate_ir::{Clause, Query};
use relate_planner::LogicalPlan;

use super::clause::{clause_node, ClauseOperator};
use super::{fused, notifications, warning, BuildCursor, ClauseMetricsMap, PlanNode};
use crate::stats::PlanStats;

/// The stage sources the plan tree builds from: the statement's
/// [`LogicalPlan`] and a `CALL { … } IN TRANSACTIONS` body's
/// [`Query`](relate_ir::Query) (REL8-2047) — one view over the parts /
/// joiner / `next` shape both carry, so the one [`stage_tree`] assembler
/// serves both trees and neither can drift from the other.
pub(super) trait StageView {
    /// The stage's shape — its parts and their joiner as one value, so the
    /// assembler matches a single fact instead of re-deriving it from a
    /// count and a flag under a "multi-part only" precondition. The
    /// joiner invariant — a multi-part stage carries one flag per
    /// boundary, uniform because mixing `UNION` and `UNION ALL` is a
    /// parse error — is this view's one spelling.
    fn stage_shape(&self) -> StageShape<'_>;
    /// The next `NEXT`-chain stage, when the statement chains one.
    fn next_stage(&self) -> Option<&Self>;
}

/// One stage's shape, read once per stage: a single part's clause
/// pipeline, or N parts joined by one uniform joiner.
pub(super) enum StageShape<'a> {
    /// The stage's one part.
    Single(&'a [Clause]),
    /// The stage's parts in statement order, joined by `all` (`UNION
    /// ALL`) or deduplicating `UNION`.
    Union {
        /// The uniform `UNION ALL` joiner flag.
        all: bool,
        parts: Vec<&'a [Clause]>,
    },
}

/// Where a stage sits in the PROFILE metrics key: the NEXT-chain stage
/// and the part index inside it — one value, so the two same-typed indices
/// cannot be transposed at the key site, which would silently drop a
/// multi-stage or multi-part tree's records.
#[derive(Clone, Copy)]
pub(super) struct StagePos {
    /// The NEXT-chain stage (1-based), the tree's stage numbering.
    pub(super) stage: usize,
    /// The part index within the stage.
    pub(super) part_index: usize,
}

impl StagePos {
    /// The stage's first part — the entry position for a stage, and the
    /// position a batched `CALL { … } IN TRANSACTIONS` body's own stage
    /// carries (its records are `None`, but the walk needs a position).
    pub(super) fn stage(stage: usize) -> Self {
        Self {
            stage,
            part_index: 0,
        }
    }
}

/// The one stage-shape build every source shares: both stage sources
/// carry the identical `parts` / `union_all` / `next` vocabulary, so a
/// source differs only in how it lends one part's clauses — the one
/// projection each impl supplies.
fn stage_shape_from<'a, P>(
    parts: &'a [P],
    union_all: &[bool],
    clauses_of: impl Fn(&'a P) -> &'a [Clause],
) -> StageShape<'a> {
    match parts {
        // The parser and planner both build at least one part; naming the
        // empty case here keeps the union arm's "two or more parts"
        // invariant at the seam where the shape is built.
        [] => unreachable!("a stage carries at least one part"),
        [part] => StageShape::Single(clauses_of(part)),
        _ => {
            // The one joiner fact: a multi-part stage carries one flag per
            // boundary, so the two lists pair — the check lives where both
            // are in hand, and the first flag (uniform, because mixing
            // `UNION` and `UNION ALL` is a parse error) carries the joiner
            // for all of them.
            assert_eq!(
                union_all.len() + 1,
                parts.len(),
                "a multi-part stage carries a joiner flag per boundary"
            );
            StageShape::Union {
                all: union_all[0],
                parts: parts.iter().map(clauses_of).collect(),
            }
        }
    }
}

impl StageView for LogicalPlan {
    fn stage_shape(&self) -> StageShape<'_> {
        stage_shape_from(&self.parts, &self.union_all, Vec::as_slice)
    }
    fn next_stage(&self) -> Option<&Self> {
        self.next.as_deref()
    }
}

impl StageView for Query {
    fn stage_shape(&self) -> StageShape<'_> {
        stage_shape_from(&self.parts, &self.union_all, |part| part.clauses.as_slice())
    }
    fn next_stage(&self) -> Option<&Self> {
        self.next.as_deref()
    }
}

/// The recursive stage builder: `stage` is this stage's number in the
/// `NEXT` chain — the root of the chain is stage 1 and every `Next` child
/// introduces the stage one later, named in its details. One assembler
/// for every stage source ([`StageView`]): the statement's stages carry
/// their PROFILE records keyed `(stage, part, clause)`, a batched body's
/// pass `None` — the body runs inside the `CallSubquery` operator's
/// batch, so its work is that operator's record, never a
/// per-body-operator one (REL8-2047).
pub(super) fn stage_tree<S: StageView>(
    source: &S,
    pos: StagePos,
    metrics: Option<&ClauseMetricsMap>,
    stats: &dyn PlanStats,
    cursor: &mut BuildCursor,
) -> PlanNode {
    let mut node = match source.stage_shape() {
        StageShape::Single(part) => part_tree(part, pos, metrics, stats, cursor),
        // The joiner is uniform across boundaries (mixing UNION and UNION
        // ALL is a parse error), so the operator name carries it without
        // loss; children are the parts' pipelines in statement order.
        StageShape::Union { all, parts } => {
            let mut node = PlanNode::new(
                if all { "UnionAll" } else { "Union" },
                format!("{} parts", parts.len()),
                Vec::new(),
                Vec::new(),
            );
            node.id = cursor.take_id();
            for (index, part) in parts.iter().enumerate() {
                node.children.push(part_tree(
                    part,
                    StagePos {
                        part_index: index,
                        ..pos
                    },
                    metrics,
                    stats,
                    cursor,
                ));
            }
            node
        }
    };
    if let Some(next) = source.next_stage() {
        let mut next_node = PlanNode::new(
            "Next",
            format!("stage {}", pos.stage + 1),
            Vec::new(),
            Vec::new(),
        );
        next_node.id = cursor.take_id();
        next_node.children.push(stage_tree(
            next,
            StagePos::stage(pos.stage + 1),
            metrics,
            stats,
            cursor,
        ));
        node.children.push(next_node);
    }
    node
}

/// The pipeline of one plan part: one node per clause, the projection
/// outermost, each earlier clause as the child of the one after it. The
/// part's clause ids are claimed as one block up front
/// (`reserve(part.len())`) and dealt backwards — the part's last clause
/// is its root, so clause `i` takes `base + (len - 1 - i)` — because the
/// chain is built child-first; the fused steps (virtual plan operators: a
/// seek-started MATCH's seek and per-hop `Expand` steps) and a batched
/// body's subtree take theirs as the build reaches them. The breakdown
/// and the body ride after the chain child, so the pipeline's data-flow
/// child stays first (the wire's `input_rows` reads it). The clause's
/// operator-scoped notifications are staged under the node's id in the
/// same pass — never through a positional list zipped by a later walk.
pub(super) fn part_tree(
    part: &[Clause],
    pos: StagePos,
    metrics: Option<&ClauseMetricsMap>,
    stats: &dyn PlanStats,
    cursor: &mut BuildCursor,
) -> PlanNode {
    // The clause nodes' pre-order ids, claimed outermost-first: the part's
    // last clause is its root, so clause `i` takes `base + (len - 1 - i)`.
    // Each node precedes the whole subtree under it (the chain child, then
    // its fused steps).
    let base = cursor.reserve(part.len());
    let mut chain: Option<PlanNode> = None;
    for (clause_index, clause) in part.iter().enumerate() {
        let facts = clause_node(clause, stats);
        let mut node = PlanNode::new(
            facts.operator.name(),
            facts.details,
            facts.identifiers,
            Vec::new(),
        )
        .with_estimate_of(facts.estimate)
        .with_warnings(facts.warnings)
        .with_metrics(
            metrics
                .and_then(|m| m.get(&(pos.stage, pos.part_index, clause_index)))
                .copied(),
        );
        node.id = base + (part.len() - 1 - clause_index);
        // LABS-1117: the store's static pattern-engine rung, stamped on a
        // MATCH operator so every EXPLAIN/PROFILE tree carries it.
        node.pattern_engine = facts.pattern_engine;
        // `EstimationMiss` (typed node warnings): derived beside the
        // metrics through the one derivation helper — the same 10x
        // policy the ratio cell's warning marker reads.
        if let Some(miss) =
            warning::estimation_miss_warning(node.estimated_rows(), node.metrics.map(|m| m.rows))
        {
            node.warnings.push(miss);
        }
        // The input child's estimate, held while it is still the value it
        // means: the data-flow child is the `chain` node, and reading it
        // by name here keeps the eager-buffer notification independent of
        // the children vector's position.
        let input_rows = chain.as_ref().and_then(|child| child.estimated_rows());
        // The facts bundle is consumed field by field — nothing is taken
        // through a `&mut` escape: the notifications move out here and
        // are staged at the loop's end.
        let mut notifications = facts.notifications;
        attach_clause_children(&mut node, facts.fused, clause, chain.take(), stats, cursor);
        // The typed aggregation fact and the input child's estimate are
        // both in hand here, so the eager-buffer notification is staged
        // with the node instead of being re-derived by a tree walk.
        if matches!(facts.operator, ClauseOperator::EagerAggregation) {
            if let Some(notification) =
                notifications::eager_aggregation_notification(input_rows, node.id)
            {
                notifications.push(notification);
            }
        }
        cursor.stage(node.id, notifications);
        chain = Some(node);
    }
    chain.expect("part has a clause")
}

/// Attach a clause node's children in the one order the pipeline and the
/// wire contract require: the data-flow child first (the previous clause's
/// subtree — the wire's input-rows fact reads it), then the fused MATCH's
/// virtual steps, then a batched `CALL { … } IN TRANSACTIONS` body's own
/// stages. The position rule lives here once, so `part_tree`'s loop stays
/// the per-clause assembly and the eager-buffer notification can read the
/// input child's estimate by name rather than through the children
/// vector.
fn attach_clause_children(
    node: &mut PlanNode,
    fused: Option<super::fused::FusedBreakdown>,
    clause: &Clause,
    chain: Option<PlanNode>,
    stats: &dyn PlanStats,
    cursor: &mut BuildCursor,
) {
    // Each earlier clause becomes the child of its successor, so data
    // flows bottom-up like Neo4j's plan (scans deepest, `ProduceResults`
    // on top).
    if let Some(child) = chain {
        node.children.push(child);
    }
    // Virtual plan operators: the fused MATCH's breakdown steps, behind
    // the chain child, fused into this node.
    if let Some(breakdown) = fused {
        node.children.extend(fused::build_children(
            breakdown,
            cursor,
            node.id,
            node.metrics,
        ));
    }
    // REL8-2047: the `CALL { … } IN TRANSACTIONS` body's operators ride as
    // the node's plan children, behind the pipeline's data-flow child —
    // the body's own stages through the same [`stage_tree`] assembler,
    // with no records of its own, so EXPLAIN and PROFILE both show what
    // each batch runs.
    if let Some(body) = clause.in_transactions_body() {
        // The body subtree's root carries the role marker, so a walk can
        // tell it from the pipeline's data-flow child (REL8-2047).
        let body_node =
            stage_tree(body, StagePos::stage(1), None, stats, cursor).with_body(node.id);
        node.children.push(body_node);
    }
}
