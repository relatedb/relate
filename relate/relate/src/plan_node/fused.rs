//! The fused MATCH breakdown (virtual plan operators): display-only
//! children under an index-seek-started MATCH — the seek step and one
//! `Expand` step per hop of the logical pattern, derived from the
//! pattern in the plan-tree builder. Execution is untouched (ADR-0344:
//! the clause stays the executed unit); the parent keeps the clause's
//! PROFILE totals and its rung, and a child carries a metric only where
//! a real counter measured the step — never a copy of the parent's
//! rows, time or memory.
//!
//! The gate is the seek narration's own [`IndexSeek`] (ADR-0486): the
//! plan never claims a seek the funnel would decline, so the breakdown
//! exists exactly where the narrated seek does. A lone-node seek has no
//! hops to break down — the parent already narrates the one step — so
//! the breakdown starts at the first hop.

use relate_ir::PathPattern;
use relate_planner::{apply_degree, seed_on_rel_start};

use super::seek::IndexSeek;
use super::{BuildCursor, ClauseMetrics, PlanNode};
use crate::stats::PlanStats;

/// The metrics a fused virtual child carries: only what a real counter
/// measured for the step — the seek step's seek-phase time (LABS-448)
/// and node reads (LABS-375), which for a seek-started MATCH are the
/// start element's candidate resolution. A hop step carries none: no
/// real per-hop counter exists (the per-hop tally split is a
/// follow-up).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FusedChildMetrics {
    /// The seek step's phase time, nanoseconds.
    pub seek_ns: u128,
    /// The seek step's node-store reads.
    pub node_reads: u64,
}

/// The one fused virtual child fact (virtual plan operators): the fused
/// parent operator's id and the step's own measured pair, travelling
/// together so the half-states (a flag without a parent, a parent
/// without the flag) are unrepresentable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FusedChild {
    /// The fused parent operator's pre-order id.
    pub parent: usize,
    /// The seek step's measured pair; `None` on a hop step and on
    /// EXPLAIN trees.
    pub metrics: Option<FusedChildMetrics>,
}

/// One MATCH's fused breakdown: the seek step's narration (the
/// [`IndexSeek`] itself, held — never re-declared) and one [`HopStep`]
/// per hop of the logical pattern. Built where the seek is derived
/// (`clause.rs`), assembled into children where the parent's id and
/// metrics are known (`part_tree`).
pub(super) struct FusedBreakdown {
    /// The seek step: operator, details line and own count.
    seek: IndexSeek,
    /// One `Expand` step per hop, pattern order.
    hops: Vec<HopStep>,
}

/// One hop's `Expand` step: the hop's subject text and its estimate off
/// the ADR-0218 ladder.
pub(super) struct HopStep {
    details: String,
    estimated_rows: Option<u64>,
}

impl FusedBreakdown {
    /// The child count the builder's capacity reservation reads.
    fn child_count(&self) -> usize {
        1 + self.hops.len()
    }
}

/// The breakdown of an index-seek-started MATCH: `None` unless the
/// pattern is the seek's own single pattern with at least one hop. The
/// hop estimates chain off the seek's count through the planner's walk
/// model ([`apply_degree`] — the same degree ladder the walk's cost
/// reads), the walk leaving the seek-anchored first element forward; a
/// seek the stats cannot price (the per-row keyed lookup, LABS-1083)
/// anchors nothing, so the hops carry no estimate either — never a
/// fabricated number.
pub(super) fn breakdown(
    pattern: &PathPattern,
    seek: &IndexSeek,
    stats: &dyn PlanStats,
) -> Option<FusedBreakdown> {
    if pattern.rels.is_empty() {
        return None;
    }
    let mut hops = Vec::with_capacity(pattern.rels.len());
    let mut rows = seek.rows.map(u128::from);
    for (hop, rel) in pattern.rels.iter().enumerate() {
        let estimated_rows = rows.map(|r| {
            // The one u128→u64 saturation: clamp, then convert — never a
            // silent `unwrap_or` that hides the width policy.
            u64::try_from(
                apply_degree(
                    r,
                    &pattern.nodes[hop],
                    rel,
                    seed_on_rel_start(rel, false),
                    stats,
                )
                .min(u128::from(u64::MAX)),
            )
            .expect("clamped to u64")
        });
        rows = estimated_rows.map(u128::from);
        hops.push(HopStep {
            details: super::subject::hop_text(pattern, hop),
            estimated_rows,
        });
    }
    Some(FusedBreakdown {
        seek: seek.clone(),
        hops,
    })
}

/// Assemble the breakdown's children: the seek step (built with its
/// measured pair), then one `Expand` step per hop — each child taking the
/// next pre-order id from the cursor (the builder reaches the steps right
/// behind the chain child's subtree) and pointing back at `parent_id`.
/// The seek step carries the measured pair on a PROFILE tree (the
/// clause's seek phase and node reads ARE the start element's candidate
/// resolution); the hop steps carry no metrics.
pub(super) fn build_children(
    breakdown: FusedBreakdown,
    cursor: &mut BuildCursor,
    parent_id: usize,
    parent_metrics: Option<ClauseMetrics>,
) -> Vec<PlanNode> {
    let seek_metrics = parent_metrics.map(|metrics| FusedChildMetrics {
        seek_ns: metrics.phases.seek_ns,
        node_reads: metrics.reads.node_reads,
    });
    // One construction for every step kind: the operator, the details, the
    // estimate and the step's own measured pair (the seek's, never a
    // hop's) are the only facts the kinds differ in.
    let mut step = |operator: &str, details: String, rows: Option<u64>, metrics| {
        let mut node = PlanNode::new(operator, details, Vec::new(), Vec::new())
            .with_estimated_rows(rows)
            .with_fused(parent_id, metrics);
        node.id = cursor.take_id();
        node
    };
    let mut children = Vec::with_capacity(breakdown.child_count());
    children.push(step(
        breakdown.seek.operator,
        breakdown.seek.line,
        breakdown.seek.rows,
        seek_metrics,
    ));
    children.extend(
        breakdown
            .hops
            .into_iter()
            .map(|hop| step("Expand", hop.details, hop.estimated_rows, None)),
    );
    children
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::super::fixtures::{compact, node, predicated, rel, row_bound, seeking};
    use super::super::seek::index_seek;
    use super::*;
    use relate_ir::{BinaryOp, Expr};

    /// The comparison fixture's `var.prop > 5` conjunct.
    fn age_window(var: &str) -> Expr {
        super::super::fixtures::comparison(var, "age", BinaryOp::Gt, Expr::Int(5))
    }

    /// A cursor past the chain child's subtree head — the tests'
    /// `first_id` (2) staged through the one id hand-out.
    fn cursor_at(next_id: usize) -> BuildCursor {
        let mut cursor = BuildCursor::default();
        cursor.reserve(next_id);
        cursor
    }

    /// The breakdown of a seek-started two-hop chain: the seek step and
    /// one Expand step per hop, each fused into the parent, the
    /// estimates chaining off the seek's count (the stub keeps no
    /// rel-type counts, so the ladder carries the seek's rows through).
    #[test]
    fn a_seek_started_match_breaks_down_per_hop() {
        let stats = seeking(&[("User", 10)], &[], &[("User", "age", 5)], BTreeMap::new());
        let patterns = [compact(
            vec![
                node(Some("n"), &["User"]),
                node(Some("m"), &["User"]),
                node(None, &[]),
            ],
            vec![rel(&["LINKS"], 1, None), rel(&["KNOWS"], 1, None)],
        )];
        let where_age = age_window("n");
        let seek = index_seek(&patterns, Some(&where_age), &stats).expect("the start seeks");
        let breakdown = breakdown(&patterns[0], &seek, &stats).expect("a hop chain breaks down");
        let children = build_children(breakdown, &mut cursor_at(2), 1, None);
        assert_eq!(children.len(), 3, "the seek step plus one per hop");
        let seek_child = &children[0];
        assert_eq!(seek_child.operator, "NodeIndexSeekByRange");
        assert_eq!(seek_child.details, "RANGE INDEX n:User(age) WHERE age > 5");
        assert_eq!(seek_child.estimated_rows, Some(5));
        assert_eq!(
            seek_child.fused,
            Some(FusedChild {
                parent: 1,
                metrics: None
            })
        );
        assert_eq!(seek_child.id, 2);
        let hop0 = &children[1];
        assert_eq!(hop0.operator, "Expand");
        assert_eq!(hop0.details, "(n:User)-[:LINKS]->(m:User)");
        assert_eq!(hop0.estimated_rows, Some(5), "the ladder's carry-through");
        assert_eq!(
            hop0.fused,
            Some(FusedChild {
                parent: 1,
                metrics: None
            })
        );
        assert_eq!(hop0.id, 3);
        let hop1 = &children[2];
        assert_eq!(hop1.details, "(m:User)-[:KNOWS]->()");
        assert_eq!(hop1.id, 4);
        assert!(hop1.metrics.is_none(), "a hop step carries no metrics");
    }

    /// The seek step's metrics are the clause's own seek phase and node
    /// reads — a real counter's numbers, never the parent's rows, time
    /// or memory.
    #[test]
    fn the_seek_child_carries_only_the_measured_pair() {
        let stats = seeking(&[("User", 10)], &[], &[("User", "age", 5)], BTreeMap::new());
        let patterns = [compact(
            vec![node(Some("n"), &["User"]), node(Some("m"), &[])],
            vec![rel(&["LINKS"], 1, None)],
        )];
        let where_age = age_window("n");
        let seek = index_seek(&patterns, Some(&where_age), &stats).expect("the start seeks");
        let breakdown = breakdown(&patterns[0], &seek, &stats).expect("a hop chain breaks down");
        let metrics = ClauseMetrics {
            rows: 9,
            db_hits: 9,
            memory_bytes: 9,
            expression_built_bytes: 9,
            time_ns: 9,
            reads: relate_executor::StoreReads {
                hops: 9,
                neighbor_walks: 9,
                node_reads: 3,
            },
            start: None,
            kernel: None,
            phases: relate_executor::PhaseTimes {
                seek_ns: 40,
                walk_ns: 9,
            },
            batched: false,
            zone_skips: 0,
            frontier: Default::default(),
            frontier_decline: None,
        };
        let children = build_children(breakdown, &mut cursor_at(2), 1, Some(metrics));
        assert_eq!(
            children[0].fused.and_then(|fused| fused.metrics),
            Some(FusedChildMetrics {
                seek_ns: 40,
                node_reads: 3
            })
        );
        assert!(
            children[0].metrics.is_none(),
            "never a copy of the parent's rows, time or memory"
        );
    }

    /// The declines: a lone-node seek has no hops to break down, and a
    /// keyed lookup the stats cannot price anchors no hop estimate.
    #[test]
    fn the_breakdown_declines_without_hops_and_prices_nothing_without_a_count() {
        let stats = seeking(
            &[("Person", 2)],
            &[("Person", "name", 1)],
            &[],
            BTreeMap::new(),
        );
        let lone = [compact(
            vec![predicated(
                Some("n"),
                "Person",
                "name",
                Expr::String("Ada".into()),
            )],
            Vec::new(),
        )];
        let seek = index_seek(&lone, None, &stats).expect("the point read seeks");
        assert!(
            breakdown(&lone[0], &seek, &stats).is_none(),
            "a lone-node seek is already the one step"
        );
        // The keyed lookup (LABS-1083): no count, so no hop estimate.
        let keyed_stats =
            seeking(&[("Model", 4)], &[], &[], BTreeMap::new()).indexed(&[("Model", "id")]);
        let where_eq =
            super::super::fixtures::comparison("m", "id", BinaryOp::Eq, row_bound("o", "id"));
        let hopping = [compact(
            vec![node(Some("m"), &["Model"]), node(Some("x"), &[])],
            vec![rel(&["LINKS"], 1, None)],
        )];
        let seek = index_seek(&hopping, Some(&where_eq), &keyed_stats).expect("the keyed seek");
        let breakdown = breakdown(&hopping[0], &seek, &keyed_stats).expect("the hop breaks down");
        assert_eq!(breakdown.seek.rows, None);
        assert_eq!(
            breakdown.hops[0].estimated_rows, None,
            "an unpriced seek anchors no hop estimate"
        );
    }
}
