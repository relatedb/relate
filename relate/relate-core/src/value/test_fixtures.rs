//! The shared `value` test fixtures (REL8-217): one owner for the
//! entity/map/list construction boilerplate the render and ordering
//! tests both need, so a fixture change lands once. The module-level
//! fixture convention of `exec/integer_arg_tests.rs`, lifted one level
//! because two test modules share it.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::{Node, PropertyMap, Relationship, Value};
use crate::{NodeId, RelId};

/// The one pairs-to-`BTreeMap` conversion every map-shaped fixture
/// builds on.
fn entries(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect()
}

/// The one `Value::Map` fixture.
pub(crate) fn map(pairs: &[(&str, Value)]) -> Value {
    Value::Map(entries(pairs).into())
}

/// The one `Value::List` fixture.
pub(crate) fn list(values: &[Value]) -> Value {
    Value::List(values.to_vec().into())
}

/// The one property-map fixture.
fn prop_map(pairs: &[(&str, Value)]) -> Arc<PropertyMap> {
    PropertyMap::share(entries(pairs))
}

/// The id-bearing node — the one construction every node-shaped fixture
/// builds on: labels and properties on a caller-chosen id.
pub(crate) fn node_at(id: NodeId, labels: &[&str], props: &[(&str, Value)]) -> Node {
    Node {
        id,
        labels: Arc::new(labels.iter().map(|l| (*l).to_string()).collect()),
        properties: prop_map(props),
    }
}

/// The id-bearing relationship — the relationship half of [`node_at`].
pub(crate) fn rel_at(
    id: RelId,
    start: NodeId,
    end: NodeId,
    rel_type: &str,
    props: &[(&str, Value)],
) -> Relationship {
    Relationship {
        id,
        start,
        end,
        rel_type: rel_type.to_string(),
        properties: prop_map(props),
    }
}

/// A fresh-id node as a [`Value`], through the crate's canonical
/// constructor — the `Arc` wrapping has one owner.
pub(crate) fn node(labels: &[&str], props: &[(&str, Value)]) -> Value {
    Value::from_node(node_at(1, labels, props))
}

/// A fresh-id relationship as a [`Value`], through the crate's canonical
/// constructor.
pub(crate) fn rel(rel_type: &str, props: &[(&str, Value)]) -> Value {
    Value::from_relationship(rel_at(2, 1, 3, rel_type, props))
}
