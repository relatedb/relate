//! Owned resident workspace of one statement.
//!
//! Query peak bytes are the high-water mark of this workspace. The formula
//! charges containers the statement allocates: vector headers and
//! [`reserved_len`] capacity, one shared row schema per distinct schema,
//! inline values at their slot size, a shallow node or relationship
//! wrapper, and deep payload only for an allocation this statement created.
//! A cloned graph `Arc` contributes its slot, not the payload again.
//! [`Value::logical_bytes`](relate_core::Value::logical_bytes) stays the
//! deep payload function and is not applied to borrowed values.
//!
//! The identity tables here (distinct schemas, allocations already
//! charged) are keyed by allocation addresses, which no client chooses,
//! so they use the id policy ([`IdMap`] / [`IdSet`], `hash_seed`); the
//! meters read them for membership and counts, never for order.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::mem::size_of;
use std::sync::Arc;

#[cfg(test)]
use indexmap::IndexMap;
use relate_core::{str_logical_bytes, Node, NodeId, RelId, Relationship, Value};

use crate::UsedSet;

use crate::group_key::GroupKey;
use crate::hash_seed::{IdMap, IdSet};
use crate::memory::{reserved_len, OVERHEAD_MAP_ENTRY, VEC_HEADER};
use crate::row::Row;

/// Header plus reserved capacity of a vector of `len` elements. An empty
/// vector reserves nothing.
pub(crate) fn buffer_bytes(len: usize, stride: u64) -> u64 {
    if len == 0 {
        0
    } else {
        VEC_HEADER + reserved_len(len) as u64 * stride
    }
}

/// [`buffer_bytes`] with saturating arithmetic: a length whose reserved
/// capacity or slot product cannot be represented saturates to [`u64::MAX`]
/// instead of wrapping. The charge path uses this, so an extreme
/// `range`/comprehension length is refused as a typed capacity error rather
/// than wrapping the byte figure to a small value and letting
/// `Vec::with_capacity` abort the process.
pub(crate) fn buffer_bytes_saturating(len: usize, stride: u64) -> u64 {
    if len == 0 {
        return 0;
    }
    let reserved = crate::memory::reserved_len_saturating(len);
    VEC_HEADER.saturating_add(reserved.saturating_mul(stride))
}

/// The slot-buffer figure of one `Vec<Value>`: the one spelling beside
/// [`buffer_bytes_saturating`], used by the charge seam and by callers that
/// compose several buffer figures into one combined charge.
pub(crate) fn value_buffer_bytes(len: usize) -> u64 {
    buffer_bytes_saturating(len, size_of::<Value>() as u64)
}

/// The retained bytes of `count` materialized rows of `slots_per_row` value
/// slots. The row's value buffer is charged through the same reservation
/// model as [`value_buffer_bytes`] (header + power-of-two reserved slots,
/// [`buffer_bytes_saturating`]), plus its slot in the row vector
/// (`size_of::<Row>()`). Saturating; the one formula both UNWIND arms read.
pub(crate) fn retained_rows_bytes(count: usize, slots_per_row: usize) -> u64 {
    (count as u64).saturating_mul(
        buffer_bytes_saturating(slots_per_row, size_of::<Value>() as u64)
            .saturating_add(size_of::<Row>() as u64),
    )
}

/// The entry storage of a map of `len` entries: one
/// [`OVERHEAD_MAP_ENTRY`] per entry. The map container's own figure — a
/// `BTreeMap` is not a `Vec`, so it must not be seeded with the value-slot
/// model. Saturating.
pub(crate) fn map_entries_bytes(len: usize) -> u64 {
    (len as u64).saturating_mul(OVERHEAD_MAP_ENTRY)
}

/// Heap this value owns. Inline values sit in their enum slot. A shared
/// `Arc` is borrowed from the graph or from another row and adds nothing.
///
/// `#[inline]` here and on the helpers below is ADR-0499's fix, not
/// decoration: the aggregate fold charges this walk per row, and when the
/// chain was a real call graph (this fn recursing through `unique_list`
/// and the entity walks, plus the cross-crate string formulas) its
/// overhead measured as 46.5% of a plain `count` — a shared payload's
/// single `Arc` count read cost a call chain. Flattened, the common arms
/// inline into the charging loop and only a genuinely deep walk pays
/// depth.
#[inline]
pub(crate) fn owned_heap(value: &Value) -> u64 {
    match value {
        Value::Null
        | Value::Bool(_)
        | Value::Int(_)
        | Value::Float(_)
        | Value::Temporal(_)
        | Value::Point(_)
        | Value::Uuid(_) => 0,
        Value::String(text) => {
            if Arc::strong_count(text) == 1 {
                str_logical_bytes(text)
            } else {
                0
            }
        }
        Value::Bytes(bytes) => {
            if Arc::strong_count(bytes) == 1 {
                relate_core::bytes_logical_bytes(bytes)
            } else {
                0
            }
        }
        Value::List(items) => unique_list(items),
        Value::Vector { values, .. } => unique_list(values),
        Value::Map(entries) => {
            if Arc::strong_count(entries) == 1 {
                map_value_heap(entries)
            } else {
                0
            }
        }
        Value::Node(node) => {
            if Arc::strong_count(node) == 1 {
                size_of::<Node>() as u64 + node_heap(node)
            } else {
                0
            }
        }
        Value::Relationship(rel) => {
            if Arc::strong_count(rel) == 1 {
                size_of::<Relationship>() as u64 + rel_heap(rel)
            } else {
                0
            }
        }
        Value::Path(path) => {
            if Arc::strong_count(path) != 1 {
                return 0;
            }
            let mut bytes = size_of::<relate_core::Path>() as u64;
            bytes += buffer_bytes(path.nodes.len(), size_of::<Node>() as u64);
            bytes += buffer_bytes(path.rels.len(), size_of::<Relationship>() as u64);
            for node in &path.nodes {
                bytes += node_heap(node);
            }
            for rel in &path.rels {
                bytes += rel_heap(rel);
            }
            bytes
        }
    }
}

#[inline]
fn unique_list(items: &Arc<Vec<Value>>) -> u64 {
    if Arc::strong_count(items) != 1 {
        return 0;
    }
    buffer_bytes(items.len(), size_of::<Value>() as u64) + items.iter().map(owned_heap).sum::<u64>()
}

#[inline]
fn map_value_heap(entries: &BTreeMap<String, Value>) -> u64 {
    let mut bytes = 0;
    for (key, value) in entries {
        bytes += OVERHEAD_MAP_ENTRY;
        bytes += str_logical_bytes(key);
        bytes += owned_heap(value);
    }
    bytes
}

#[inline]
fn property_heap(entries: &relate_core::PropertyMap) -> u64 {
    created_property_bytes(entries)
}

#[inline]
fn node_heap(node: &Node) -> u64 {
    let mut bytes = 0;
    if Arc::strong_count(&node.labels) == 1 {
        bytes += buffer_bytes(node.labels.len(), size_of::<String>() as u64);
        for label in node.labels.iter() {
            bytes += str_logical_bytes(label);
        }
    }
    if Arc::strong_count(&node.properties) == 1 {
        bytes += property_heap(&node.properties);
    }
    bytes
}

#[inline]
fn rel_heap(rel: &Relationship) -> u64 {
    let mut bytes = str_logical_bytes(&rel.rel_type);
    if Arc::strong_count(&rel.properties) == 1 {
        bytes += property_heap(&rel.properties);
    }
    bytes
}

/// The row's accounted contents, and whether this call was the set's
/// first sight of the row's schema — the one derivation of schema
/// freshness (F-1-19): the meter's rollback flag and its byte charge
/// come from the same `insert`, so they cannot disagree.
fn row_contents(row: &Row, schemas: &mut IdSet<usize>) -> (u64, bool) {
    let mut bytes = 0;
    let fresh_schema = schemas.insert(row.schema_ptr() as usize);
    if fresh_schema {
        bytes += row.schema_bytes();
    }
    bytes += buffer_bytes(row.values().len(), size_of::<Value>() as u64);
    for value in row.values() {
        bytes += owned_heap(value);
    }
    (bytes, fresh_schema)
}

fn rows_accounted<'a>(len: usize, rows: impl Iterator<Item = &'a Row>) -> u64 {
    let mut bytes = buffer_bytes(len, size_of::<Row>() as u64);
    let mut schemas = IdSet::default();
    for row in rows {
        bytes += row_contents(row, &mut schemas).0;
    }
    bytes
}

/// The row vector, each distinct schema once, each value vector, and the
/// heap those values own.
pub(crate) fn row_buffer_bytes(rows: &[Row]) -> u64 {
    rows_accounted(rows.len(), rows.iter())
}

mod result;
mod retained;

pub use result::result_rows_bytes;
pub(crate) use retained::row_retained_heap;
#[cfg(test)]
pub(crate) use retained::{note_ops, reset_note_ops};

/// Input rows an aggregation still owns. Releasing a row drops that row's
/// payload; the vector's reserved slots stay until the charge is dropped,
/// because draining the vector does not free its buffer.
pub(crate) struct RetainedRows {
    bytes: u64,
    schemas: IdMap<usize, SchemaHold>,
}

struct SchemaHold {
    bytes: u64,
    rows: usize,
}

impl RetainedRows {
    pub(crate) fn measure(rows: &[Row]) -> Self {
        let mut schemas = IdMap::default();
        for row in rows {
            let hold = schemas
                .entry(row.schema_ptr() as usize)
                .or_insert(SchemaHold {
                    bytes: row.schema_bytes(),
                    rows: 0,
                });
            hold.rows += 1;
        }
        Self {
            bytes: row_buffer_bytes(rows),
            schemas,
        }
    }

    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }

    /// `row` has moved out of the input vector.
    pub(crate) fn release_row(&mut self, row: &Row) {
        let ptr = row.schema_ptr() as usize;
        if let Some(hold) = self.schemas.get_mut(&ptr) {
            hold.rows = hold.rows.saturating_sub(1);
            if hold.rows == 0 {
                self.bytes = self.bytes.saturating_sub(hold.bytes);
            }
        }
        self.bytes = self.bytes.saturating_sub(row_payload(row));
    }
}

fn row_payload(row: &Row) -> u64 {
    buffer_bytes(row.values().len(), size_of::<Value>() as u64)
        + row.values().iter().map(owned_heap).sum::<u64>()
}

/// Same walk as [`row_buffer_bytes`] for rows that are not a contiguous slice.
pub(crate) fn row_refs_bytes<'a>(rows: impl IntoIterator<Item = &'a Row>) -> u64 {
    let rows: Vec<&Row> = rows.into_iter().collect();
    rows_accounted(rows.len(), rows.into_iter())
}

/// Sort-key auxiliary: the keyed vector plus each key's value buffer.
pub(crate) fn sort_key_columns<'a>(keys: impl IntoIterator<Item = &'a [Value]>) -> u64 {
    let keys: Vec<&[Value]> = keys.into_iter().collect();
    let stride = (size_of::<Vec<Value>>() + size_of::<usize>()) as u64;
    let mut bytes = buffer_bytes(keys.len(), stride);
    for key in keys {
        bytes += buffer_bytes(key.len(), size_of::<Value>() as u64);
        for value in key {
            bytes += owned_heap(value);
        }
    }
    bytes
}

/// One bound row's slot in an expand batch: the row plus its used-edge
/// set. The one spelling of the layout both the batch accounting
/// ([`binding_bytes`]) and the incremental meter ([`BindingMeter::push`])
/// charge — they must move together with the pair's types (LABS-1116).
fn binding_stride() -> u64 {
    size_of::<(Row, UsedSet)>() as u64
}

/// One expand batch: the pair vector, the rows, and each used-edge set.
pub(crate) fn binding_bytes(rows: &[(Row, UsedSet)]) -> u64 {
    let stride = binding_stride();
    let mut bytes = buffer_bytes(rows.len(), stride);
    let mut schemas = IdSet::default();
    for (row, used) in rows {
        bytes += row_contents(row, &mut schemas).0;
        bytes += buffer_bytes(used.len(), size_of::<RelId>() as u64);
    }
    bytes
}

fn group_key_heap(key: &GroupKey) -> u64 {
    key.logical_bytes()
}

/// A set of `GroupKey`s under any hasher: its table plus each key's
/// accounted heap.
pub(crate) fn group_key_set_bytes<S>(keys: &HashSet<GroupKey, S>) -> u64 {
    let mut bytes = buffer_bytes(keys.len(), size_of::<GroupKey>() as u64);
    for key in keys {
        bytes += group_key_heap(key);
    }
    bytes
}

/// The word tier of a distinct set holding `len` words of one tag: the
/// stride-8 figure T1 and a T2 tag table share, so promotion is free.
pub(crate) fn word_set_bytes(len: usize) -> u64 {
    buffer_bytes(len, size_of::<u64>() as u64)
}

/// A distinct set's row-key table. The hot path adds
/// [`distinct_row_insert_delta`]; this walk is the oracle and the figure a
/// cloned set retains.
pub(crate) fn distinct_row_set_bytes<S>(keys: &HashSet<Vec<GroupKey>, S>) -> u64 {
    let mut bytes = buffer_bytes(keys.len(), size_of::<Vec<GroupKey>>() as u64);
    for key in keys {
        bytes += buffer_bytes(key.len(), size_of::<GroupKey>() as u64);
        for part in key {
            bytes += group_key_heap(part);
        }
    }
    bytes
}

/// Live grouped aggregation: the map, the group keys, and the retained rows.
/// The hot path updates [`GroupMapMeter`]; this walk is the oracle.
#[cfg(test)]
pub(crate) fn group_map_bytes(groups: &IndexMap<Vec<GroupKey>, Vec<Row>>) -> u64 {
    let stride = (size_of::<Vec<GroupKey>>() + size_of::<Vec<Row>>()) as u64;
    let mut bytes = buffer_bytes(groups.len(), stride);
    for (key, rows) in groups {
        bytes += buffer_bytes(key.len(), size_of::<GroupKey>() as u64);
        for part in key {
            bytes += group_key_heap(part);
        }
        bytes += row_buffer_bytes(rows);
    }
    bytes
}

/// Scratch the statement created. Property values use [`owned_heap`], so a
/// string still owned by the graph is not billed again. The property map
/// and the redo log are charged for the slots they reserve, not for a
/// `BTreeMap` leaf or a catalog-sized record. Redo shares the map `Arc`;
/// the container is still this statement's, so it is charged without a
/// strong-count check.
pub(crate) fn scratch_created_bytes(
    nodes: &HashMap<NodeId, Node>,
    rels: &HashMap<RelId, Relationship>,
    created_nodes: &HashSet<NodeId>,
    created_rels: &HashSet<RelId>,
    out_adj: &HashMap<NodeId, Vec<RelId>>,
    in_adj: &HashMap<NodeId, Vec<RelId>>,
    redo_bytes: u64,
) -> u64 {
    let mut bytes = buffer_bytes(created_nodes.len(), size_of::<NodeId>() as u64);
    bytes += buffer_bytes(created_rels.len(), size_of::<RelId>() as u64);
    for id in created_nodes {
        if let Some(node) = nodes.get(id) {
            bytes += created_node_bytes(node);
        }
    }
    for id in created_rels {
        if let Some(rel) = rels.get(id) {
            bytes += created_rel_bytes(rel);
        }
    }
    bytes += adj_map_bytes(out_adj);
    bytes += adj_map_bytes(in_adj);
    bytes += redo_bytes;
    bytes
}

fn created_node_bytes(node: &Node) -> u64 {
    let mut bytes = OVERHEAD_MAP_ENTRY + size_of::<Node>() as u64;
    bytes += buffer_bytes(node.labels.len(), size_of::<String>() as u64);
    for label in node.labels.iter() {
        bytes += str_logical_bytes(label);
    }
    bytes += created_property_bytes(&node.properties);
    bytes
}

fn created_rel_bytes(rel: &Relationship) -> u64 {
    OVERHEAD_MAP_ENTRY
        + size_of::<Relationship>() as u64
        + str_logical_bytes(&rel.rel_type)
        + created_property_bytes(&rel.properties)
}

fn created_property_bytes(entries: &relate_core::PropertyMap) -> u64 {
    let stride = size_of::<(String, Value)>() as u64;
    let mut bytes = if entries.slot_capacity() == 0 {
        0
    } else {
        VEC_HEADER + entries.slot_capacity() as u64 * stride
    };
    for (key, value) in entries {
        bytes += str_logical_bytes(key);
        bytes += owned_heap(value);
    }
    bytes
}

/// Byte change of one buffer when its length moves from `old_len` to `new_len`.
pub(crate) fn buffer_growth(old_len: usize, new_len: usize, stride: u64) -> u64 {
    buffer_bytes(new_len, stride).saturating_sub(buffer_bytes(old_len, stride))
}

/// Running total for a binding batch. `push` adds one row; the total equals
/// [`binding_bytes`] of the same rows.
#[derive(Debug, Default)]
pub(crate) struct BindingMeter {
    pub bytes: u64,
    len: usize,
    schemas: IdSet<usize>,
}

/// Snapshot taken before a [`BindingMeter::push`], so a refused resize can
/// put the meter back.
pub(crate) struct BindingMark {
    bytes: u64,
    len: usize,
    schema: Option<usize>,
}

impl BindingMeter {
    pub(crate) fn push(&mut self, row: &Row, used_len: usize) -> BindingMark {
        let mark = BindingMark {
            bytes: self.bytes,
            len: self.len,
            schema: None,
        };
        let ptr = row.schema_ptr() as usize;
        let stride = binding_stride();
        let old = self.len;
        self.len += 1;
        self.bytes += buffer_growth(old, self.len, stride);
        let (contents, fresh_schema) = row_contents(row, &mut self.schemas);
        self.bytes += contents;
        self.bytes += buffer_bytes(used_len, size_of::<RelId>() as u64);
        BindingMark {
            schema: fresh_schema.then_some(ptr),
            ..mark
        }
    }

    pub(crate) fn unpush(&mut self, mark: BindingMark) {
        self.bytes = mark.bytes;
        self.len = mark.len;
        if let Some(ptr) = mark.schema {
            self.schemas.remove(&ptr);
        }
    }
}

#[derive(Debug, Default)]
struct GroupSlot {
    rows: usize,
    schemas: IdSet<usize>,
}

impl GroupSlot {
    fn add_row(&mut self, row: &Row) -> u64 {
        let old = self.rows;
        self.rows += 1;
        let mut delta = buffer_growth(old, self.rows, size_of::<Row>() as u64);
        if self.schemas.insert(row.schema_ptr() as usize) {
            delta += row.schema_bytes();
        }
        delta += buffer_bytes(row.values().len(), size_of::<Value>() as u64);
        for value in row.values() {
            delta += owned_heap(value);
        }
        delta
    }
}

/// Running total for an aggregation map. Adding a group or a row matches
/// [`group_map_bytes`] of the same map.
#[derive(Debug, Default)]
pub(crate) struct GroupMapMeter {
    pub bytes: u64,
    groups: Vec<GroupSlot>,
    payload: u64,
}

impl GroupMapMeter {
    fn stride() -> u64 {
        (size_of::<Vec<GroupKey>>() + size_of::<Vec<Row>>()) as u64
    }

    fn recompute(&mut self) {
        self.bytes = buffer_bytes(self.groups.len(), Self::stride()) + self.payload;
    }

    pub(crate) fn add_group(&mut self, key: &[GroupKey], row: &Row) {
        let mut slot = GroupSlot::default();
        let row_bytes = slot.add_row(row);
        self.groups.push(slot);
        self.payload += key_account(key) + row_bytes;
        self.recompute();
    }

    pub(crate) fn add_row(&mut self, index: usize, row: &Row) {
        self.payload += self.groups[index].add_row(row);
        self.recompute();
    }

    /// The keyless group aggregation inserts before any input row arrives.
    pub(crate) fn add_empty_group(&mut self) {
        self.groups.push(GroupSlot::default());
        self.recompute();
    }
}

fn key_account(key: &[GroupKey]) -> u64 {
    buffer_bytes(key.len(), size_of::<GroupKey>() as u64)
        + key.iter().map(GroupKey::logical_bytes).sum::<u64>()
}

/// The one group-map entry-landing charge: the map slot's buffer growth
/// (the slot holds the key vector's and the row vector's headers) plus
/// the key's slot buffer and owned payload — the same figures
/// [`GroupMapMeter`]'s `key_account` counts for the workspace ledger,
/// spelled once for the expression-budget running total a new group
/// grows it by.
pub(crate) fn group_entry_insert_delta(old_len: usize, key: &[GroupKey]) -> u64 {
    buffer_growth(old_len, old_len + 1, GroupMapMeter::stride()) + key_account(key)
}

pub(crate) fn distinct_row_insert_delta(old_len: usize, key: &[GroupKey]) -> u64 {
    buffer_growth(old_len, old_len + 1, size_of::<Vec<GroupKey>>() as u64) + key_account(key)
}

/// The one distinct-entry charge policy, applied by
/// [`DistinctSet`](crate::distinct_set::DistinctSet) to each new key: the
/// delta is computed only while a ledger is installed, so an admission-off
/// run never walks a key for bytes (ADR-0507 §5); a zero delta charges
/// nothing; otherwise grow the meter and hold the peak. `delta` comes from
/// the representation that took the key ([`word_insert_delta`],
/// [`group_key_insert_delta`] or [`distinct_row_insert_delta`]).
pub(crate) fn charge_distinct_delta(
    charge: &mut relate_core::tokens::WorkspaceCharge,
    delta: impl FnOnce() -> u64,
) -> relate_core::Result<()> {
    if !relate_core::tokens::workspace_accounting() {
        return Ok(());
    }
    let delta = delta();
    if delta == 0 {
        return Ok(());
    }
    charge.resize(charge.bytes().saturating_add(delta))?;
    relate_core::tokens::peak_hold("distinct");
    Ok(())
}

pub(crate) fn group_key_insert_delta(old_len: usize, key: &GroupKey) -> u64 {
    buffer_growth(old_len, old_len + 1, size_of::<GroupKey>() as u64) + key.logical_bytes()
}

/// One new word in a distinct set's word tier ([`word_set_bytes`]).
pub(crate) fn word_insert_delta(old_len: usize) -> u64 {
    buffer_growth(old_len, old_len + 1, size_of::<u64>() as u64)
}

pub(crate) fn created_node_insert_delta(existing: usize, node: &Node) -> u64 {
    buffer_growth(existing, existing + 1, size_of::<NodeId>() as u64) + created_node_bytes(node)
}

pub(crate) fn created_node_remove_delta(remaining: usize, node: &Node) -> u64 {
    buffer_growth(remaining, remaining + 1, size_of::<NodeId>() as u64) + created_node_bytes(node)
}

pub(crate) fn created_rel_insert_delta(
    existing: usize,
    rel: &Relationship,
    out_len: Option<usize>,
    in_len: Option<usize>,
) -> u64 {
    buffer_growth(existing, existing + 1, size_of::<RelId>() as u64)
        + created_rel_bytes(rel)
        + adj_len_delta(out_len)
        + adj_len_delta(in_len)
}

/// Deleting a created relationship drops its record and id-set slot. The
/// adjacency lists keep the id, matching [`delete_rel`].
pub(crate) fn created_rel_remove_delta(remaining: usize, rel: &Relationship) -> u64 {
    buffer_growth(remaining, remaining + 1, size_of::<RelId>() as u64) + created_rel_bytes(rel)
}

fn adj_len_delta(existing: Option<usize>) -> u64 {
    match existing {
        None => OVERHEAD_MAP_ENTRY + buffer_bytes(1, size_of::<RelId>() as u64),
        Some(len) => buffer_growth(len, len + 1, size_of::<RelId>() as u64),
    }
}

pub(crate) fn created_property_delta(entries: &relate_core::PropertyMap) -> u64 {
    created_property_bytes(entries)
}

pub(crate) fn created_label_bytes(labels: &[String]) -> u64 {
    buffer_bytes(labels.len(), size_of::<String>() as u64)
        + labels
            .iter()
            .map(|label| str_logical_bytes(label))
            .sum::<u64>()
}

fn adj_map_bytes(adj: &HashMap<NodeId, Vec<RelId>>) -> u64 {
    let mut bytes = adj.len() as u64 * OVERHEAD_MAP_ENTRY;
    for rels in adj.values() {
        bytes += buffer_bytes(rels.len(), size_of::<RelId>() as u64);
    }
    bytes
}

#[cfg(test)]
mod tests;
