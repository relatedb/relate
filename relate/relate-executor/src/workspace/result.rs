//! The public result's retention walk: the ownership rule that charges a
//! fully materialized result's payload by allocation identity, each shared
//! `Arc` once — split from `workspace.rs` beside `retained.rs`, one module
//! per ownership policy. The identity set is keyed by allocation addresses
//! no client chooses, so it uses the id policy ([`IdSet`]).

use std::mem::size_of;
use std::sync::Arc;

use relate_core::{str_logical_bytes, Node, Relationship, Value};

use crate::hash_seed::IdSet;
use crate::memory::OVERHEAD_MAP_ENTRY;

use super::{buffer_bytes, created_label_bytes, property_container_bytes};

/// Retained heap of a fully materialized public result. Shared payloads are
/// counted once by allocation identity, including payloads referenced by
/// more than one output row.
pub fn result_rows_bytes(rows: &[Vec<Value>]) -> u64 {
    let mut seen = IdSet::default();
    let mut bytes = buffer_bytes(rows.len(), size_of::<Vec<Value>>() as u64);
    for row in rows {
        bytes += buffer_bytes(row.len(), size_of::<Value>() as u64);
        for value in row {
            bytes += result_value_heap(value, &mut seen);
        }
    }
    bytes
}

fn mark_arc<T: ?Sized>(value: &Arc<T>, seen: &mut IdSet<usize>) -> bool {
    seen.insert(Arc::as_ptr(value) as *const () as usize)
}

fn result_value_heap(value: &Value, seen: &mut IdSet<usize>) -> u64 {
    match value {
        Value::Null
        | Value::Bool(_)
        | Value::Int(_)
        | Value::Float(_)
        | Value::Temporal(_)
        | Value::Point(_)
        | Value::Uuid(_) => 0,
        Value::String(text) => {
            if mark_arc(text, seen) {
                str_logical_bytes(text)
            } else {
                0
            }
        }
        Value::Bytes(bytes) => {
            if mark_arc(bytes, seen) {
                relate_core::bytes_logical_bytes(bytes)
            } else {
                0
            }
        }
        Value::List(items) | Value::Vector { values: items, .. } => {
            if !mark_arc(items, seen) {
                return 0;
            }
            buffer_bytes(items.len(), size_of::<Value>() as u64)
                + items
                    .iter()
                    .map(|item| result_value_heap(item, seen))
                    .sum::<u64>()
        }
        Value::Map(entries) => {
            if !mark_arc(entries, seen) {
                return 0;
            }
            entries
                .iter()
                .map(|(key, value)| {
                    OVERHEAD_MAP_ENTRY + str_logical_bytes(key) + result_value_heap(value, seen)
                })
                .sum()
        }
        Value::Node(node) => {
            if !mark_arc(node, seen) {
                return 0;
            }
            size_of::<Node>() as u64 + result_node_heap(node, seen)
        }
        Value::Relationship(rel) => {
            if !mark_arc(rel, seen) {
                return 0;
            }
            size_of::<Relationship>() as u64 + result_rel_heap(rel, seen)
        }
        Value::Path(path) => {
            if !mark_arc(path, seen) {
                return 0;
            }
            size_of::<relate_core::Path>() as u64
                + buffer_bytes(path.nodes.len(), size_of::<Node>() as u64)
                + buffer_bytes(path.rels.len(), size_of::<Relationship>() as u64)
                + path
                    .nodes
                    .iter()
                    .map(|node| result_node_heap(node, seen))
                    .sum::<u64>()
                + path
                    .rels
                    .iter()
                    .map(|rel| result_rel_heap(rel, seen))
                    .sum::<u64>()
        }
    }
}

fn result_node_heap(node: &Node, seen: &mut IdSet<usize>) -> u64 {
    let labels = if mark_arc(&node.labels, seen) {
        created_label_bytes(&node.labels)
    } else {
        0
    };
    labels + result_property_heap(&node.properties, seen)
}

fn result_rel_heap(rel: &Relationship, seen: &mut IdSet<usize>) -> u64 {
    str_logical_bytes(&rel.rel_type) + result_property_heap(&rel.properties, seen)
}

fn result_property_heap(entries: &Arc<relate_core::PropertyMap>, seen: &mut IdSet<usize>) -> u64 {
    if !mark_arc(entries, seen) {
        return 0;
    }
    let mut bytes = property_container_bytes(entries);
    for (key, value) in entries.iter() {
        bytes += str_logical_bytes(key);
        bytes += result_value_heap(value, seen);
    }
    bytes
}
