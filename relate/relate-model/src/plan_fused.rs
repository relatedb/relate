//! The fused virtual child's wire shape (virtual plan operators): a
//! display-only breakdown step under its fused parent operator — the
//! seek step of an index-seek-started MATCH and one `Expand` step per
//! hop of the logical pattern. Execution is untouched (ADR-0344: the
//! clause stays the executed unit), so a fused child never carries the
//! PROFILE metric block — rows, time and memory stay the fused parent's
//! clause totals. A child carries a metric only where a real counter
//! measured the step: the seek step's seek-phase time (LABS-448) and
//! node reads (LABS-375), which for a seek-started MATCH are the start
//! element's candidate resolution. A hop step carries none — no real
//! per-hop counter exists (splitting the walk tallies per hop is a
//! follow-up), so `Hops` on a fused child is a contract break, kept
//! loud like every other half-state here.

use crate::wire::plan_keys;
use crate::CypherValue;

/// The metrics a fused virtual child carries: the seek step's seek-phase
/// time and node reads — the same candidate-funnel counter family, so
/// they ride together or not at all. An `Expand` hop step carries none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlanFusedMetricsWire {
    /// The seek step's phase time, nanoseconds (LABS-448's seek clock).
    pub seek_ns: i64,
    /// The seek step's node-store reads (LABS-375's candidate funnel).
    pub node_reads: i64,
}

/// One wire node's parsed fused facts (virtual plan operators): the
/// fused parent operator's id and the step's own measured pair. The one
/// fused fact — the `FusedInto` key's presence IS "this node is a fused
/// child", so a flag beside it (or a parent link without one) is
/// unrepresentable past the parse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FusedChildWire {
    /// The fused parent operator's id.
    pub parent: i64,
    /// The seek step's metrics; `None` on a hop step (no real per-hop
    /// counter exists).
    pub metrics: Option<PlanFusedMetricsWire>,
}

/// Read one node's fused facts, `None` on every non-fused node. The
/// contract, kept loud like the accessor's other groups: `FusedInto` is
/// the parent operator's non-negative id, and its presence is the whole
/// fact; a fused child never carries the walk-side keys
/// ([`plan_keys::FUSED_CHILD_NEVER`]) nor the PROFILE metric block
/// ([`plan_keys::METRIC_BLOCK_KEYS`]) — the walk and the totals stay the
/// fused parent's — and carries the seek pair (`SeekTimeNs`,
/// `NodeReads`) through the one all-or-none group policy every metric
/// family shares, together or not at all.
pub(crate) fn fused_child(args: &crate::IndexMap<String, CypherValue>) -> Option<FusedChildWire> {
    let parent = super::plan_args::optional_int(args, plan_keys::FUSED_INTO)?;
    for key in plan_keys::FUSED_CHILD_NEVER
        .iter()
        .chain(plan_keys::METRIC_BLOCK_KEYS.iter())
    {
        if args.contains_key(*key) {
            panic!(
                "plan wire {key} never rides a fused child — the walk and the \
                 PROFILE metric block stay the fused parent's (virtual plan operators)"
            );
        }
    }
    let metrics =
        super::plan_args::metric_group_ints(args, [plan_keys::SEEK_TIME_NS, plan_keys::NODE_READS])
            .map(|[seek_ns, node_reads]| PlanFusedMetricsWire {
                seek_ns,
                node_reads,
            });
    Some(FusedChildWire { parent, metrics })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::plan_node_wire;
    use crate::wire::plan_test_fixtures::{args_mut, plan_wire_node};

    /// Compose a fused wire child: the seek step's shape — `FusedInto`
    /// beside the seek pair, no metric block.
    fn fused_seek_wire_node(id: i64, parent: i64, seek_ns: i64, node_reads: i64) -> CypherValue {
        let mut node = plan_wire_node("NodeIndexSeek", id, None, vec![]);
        let args = args_mut(&mut node);
        args.insert(plan_keys::FUSED_INTO.to_string(), CypherValue::Int(parent));
        args.insert(
            plan_keys::SEEK_TIME_NS.to_string(),
            CypherValue::Int(seek_ns),
        );
        args.insert(
            plan_keys::NODE_READS.to_string(),
            CypherValue::Int(node_reads),
        );
        node
    }

    /// The fused seek child reads its parent link and its own measured
    /// pair — never the parent's metric block. The `FusedInto` key's
    /// presence is the whole fused fact.
    #[test]
    fn a_fused_seek_child_carries_its_parent_link_and_seek_pair() {
        let child = fused_seek_wire_node(2, 1, 40, 3);
        let root = plan_wire_node("NodeIndexSeek", 1, None, vec![child]);
        let wire = plan_node_wire(&root);
        assert_eq!(wire.fused, None);
        let child = plan_node_wire(&wire.children[0]);
        assert_eq!(
            child.fused,
            Some(FusedChildWire {
                parent: 1,
                metrics: Some(PlanFusedMetricsWire {
                    seek_ns: 40,
                    node_reads: 3
                })
            })
        );
        assert!(
            child.metrics.is_none(),
            "a fused child never carries the metric block"
        );
    }

    /// A fused hop child carries no metrics — no real per-hop counter
    /// exists.
    #[test]
    fn a_fused_hop_child_carries_no_metrics() {
        let mut hop = plan_wire_node("Expand", 3, None, vec![]);
        args_mut(&mut hop).insert(plan_keys::FUSED_INTO.to_string(), CypherValue::Int(1));
        let wire = plan_node_wire(&hop);
        assert_eq!(
            wire.fused,
            Some(FusedChildWire {
                parent: 1,
                metrics: None
            })
        );
    }

    /// The walk-side keys stay the fused parent's: `Hops` on a fused
    /// child is a wire-contract break, kept loud.
    #[test]
    #[should_panic(expected = "plan wire Hops never rides a fused child")]
    fn a_fused_child_never_carries_the_walk_kinds() {
        let mut hop = plan_wire_node("Expand", 3, None, vec![]);
        let args = args_mut(&mut hop);
        args.insert(plan_keys::FUSED_INTO.to_string(), CypherValue::Int(1));
        args.insert(plan_keys::HOPS.to_string(), CypherValue::Int(7));
        let _ = plan_node_wire(&hop);
    }

    /// The parent's rows, time and memory stay the parent's: the metric
    /// block on a fused child is a wire-contract break. The block's keys
    /// are the one table the accessor and the fixtures read, never a
    /// second list here.
    #[test]
    #[should_panic(expected = "plan wire Rows never rides a fused child")]
    fn a_fused_child_never_carries_the_metric_block() {
        let mut child = fused_seek_wire_node(2, 1, 40, 3);
        let args = args_mut(&mut child);
        for key in plan_keys::METRIC_BLOCK_KEYS {
            args.insert(key.to_string(), CypherValue::Int(1));
        }
        let _ = plan_node_wire(&child);
    }
}
