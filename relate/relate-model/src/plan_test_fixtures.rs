//! The one hand-built plan wire fixture (LABS-448's review round): the
//! shape the engine emits, every key spelled through the `plan_keys`
//! vocabulary — one builder for the sibling test modules (the wire
//! accessor's and the agent document's), never a second copy of the
//! metric-args block.
#![allow(dead_code)]

use crate::wire::plan_keys;
use crate::{CypherValue, IndexMap};

/// The mutable args map of a hand-built wire node — the one unwrap
/// preamble every fused/wire test shares, never a per-test copy.
pub(crate) fn args_mut(node: &mut CypherValue) -> &mut IndexMap<String, CypherValue> {
    let CypherValue::Map(entries) = node else {
        unreachable!("the fixture builds a map")
    };
    let Some(CypherValue::Map(args)) = entries.get_mut(plan_keys::ARGS) else {
        unreachable!("the fixture builds an args map")
    };
    args
}

/// A hand-built plan wire node: the operator, its id, PROFILE metric args
/// when `time_ns` carries one (the read kinds along with them, as the
/// engine emits), and the children composed in.
pub(crate) fn plan_wire_node(
    operator: &str,
    id: i64,
    time_ns: Option<i64>,
    children: Vec<CypherValue>,
) -> CypherValue {
    let mut args = IndexMap::new();
    args.insert(plan_keys::ID.to_string(), CypherValue::Int(id));
    args.insert(plan_keys::DETAILS.to_string(), CypherValue::Str("x".into()));
    if let Some(time_ns) = time_ns {
        // The metric block's keys come from the one table; the fixture
        // varies only the time and fixes the rest (rows 1, the others 0).
        for key in plan_keys::METRIC_BLOCK_KEYS {
            let value = if key == plan_keys::TIME_NS {
                time_ns
            } else if key == plan_keys::ROWS {
                1
            } else {
                0
            };
            args.insert(key.to_string(), CypherValue::Int(value));
        }
        args.insert(plan_keys::HOPS.to_string(), CypherValue::Int(0));
        args.insert(plan_keys::NEIGHBOR_WALKS.to_string(), CypherValue::Int(0));
        args.insert(plan_keys::NODE_READS.to_string(), CypherValue::Int(0));
        args.insert(plan_keys::SEEK_TIME_NS.to_string(), CypherValue::Int(0));
        args.insert(plan_keys::WALK_TIME_NS.to_string(), CypherValue::Int(0));
    }
    let mut map = IndexMap::new();
    map.insert(
        plan_keys::OPERATOR_TYPE.to_string(),
        CypherValue::Str(operator.into()),
    );
    map.insert(plan_keys::ARGS.to_string(), CypherValue::Map(args));
    map.insert(
        plan_keys::IDENTIFIERS.to_string(),
        CypherValue::List(vec![CypherValue::Str("name".into())]),
    );
    if !children.is_empty() {
        map.insert(plan_keys::CHILDREN.to_string(), CypherValue::List(children));
    }
    CypherValue::Map(map)
}

/// A hand-built index-seek plan wire node (ADR-0486): the seek operator
/// with its `RANGE INDEX …` details line and its own count as the
/// estimate — composed over [`plan_wire_node`]'s one args block, the
/// sibling test modules' one spelling of the seek shape.
pub(crate) fn plan_seek_wire_node(
    operator: &str,
    line: &str,
    estimated_rows: i64,
    id: i64,
) -> CypherValue {
    let mut node = plan_wire_node(operator, id, None, vec![]);
    let args = args_mut(&mut node);
    args.insert(
        plan_keys::DETAILS.to_string(),
        CypherValue::Str(line.into()),
    );
    args.insert(
        plan_keys::ESTIMATED_ROWS.to_string(),
        CypherValue::Int(estimated_rows),
    );
    // The estimate and its ladder level ride together (ADR-0708).
    args.insert(
        plan_keys::ESTIMATE_PROVENANCE.to_string(),
        CypherValue::Str(crate::wire::EstimateProvenance::Measured.as_str().into()),
    );
    node
}

/// Compose a fused wire child: the seek step's shape — `FusedInto`
/// beside the seek pair, no metric block.
pub(crate) fn fused_seek_wire_node(
    id: i64,
    parent: i64,
    seek_ns: i64,
    node_reads: i64,
) -> CypherValue {
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
