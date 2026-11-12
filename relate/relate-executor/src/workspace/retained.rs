//! The retaining containers' per-row allocation walk (LABS-781): the
//! ownership rule that charges each `Arc` allocation a landed row's
//! values reach exactly once when every hold of it is the row's own —
//! split from `workspace.rs` (the stellar round's 1k-ceiling finding)
//! beside the `result_*` walk's parent module.

use std::collections::BTreeMap;
use std::mem::size_of;
use std::sync::Arc;

use relate_core::{Node, PropertyMap, Relationship, Value};

use crate::hash_seed::IdMap;

use super::{
    buffer_bytes, created_label_bytes, property_heap, str_logical_bytes, OVERHEAD_MAP_ENTRY,
};

/// The payload bytes one landed row's values retain. An `Arc`
/// allocation the row's values reach charges exactly once iff every
/// hold of it is the row's own — a direct row-slot reference, or a
/// reference from inside a container the row itself solely holds — so
/// a payload duplicated across the row (a projected alias and its
/// parked ORDER BY clone, two aliases of one value, a list holding the
/// row's payload twice) counts once. An allocation held beyond the row
/// is borrowed and charges nothing **at every depth**: the store's
/// entities with their labels and properties, a parameter list the
/// statement unwinds and every value nested inside it, the group
/// container a projected seed was cloned from — a borrowed container's
/// children are borrowed with it. Entity property maps ride their
/// entity's allocation under [`property_heap`]'s own walk. The figure
/// the retaining output containers' `add_row` charge reads.
///
/// The walk descends **on ownership**, resolved by one worklist pass:
/// an allocation that turns out to be the row's own activates its
/// outgoing reference edges and, if it is a container, expands — so a
/// container that becomes row-owned only through references discovered
/// later still expands (no charge is lost), borrowed structure is
/// never walked at all, and every allocation and edge is touched
/// exactly once: linear in the row's owned structure, whatever its
/// depth or width.
pub(crate) fn row_retained_heap(values: &[Value]) -> u64 {
    let mut walk = RowWalk::default();
    for value in values {
        walk.note_value(value, None);
    }
    walk.resolve();
    walk.charge()
}

// The test seam for the walk's work bound: every note operation and
// every edge touch bumps it, so a pin can assert the walk visited only
// the row-owned structure (an unwound parameter's value graph is never
// walked) and touched each edge once. Thread-local so concurrently
// running tests never contaminate each other's counts — a test's
// statement and its walk share its thread.
#[cfg(test)]
thread_local! {
    static NOTE_OPS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The walk's note-operation count so far on this thread (the test
/// seam's read).
#[cfg(test)]
pub(crate) fn note_ops() -> u64 {
    NOTE_OPS.with(|ops| ops.get())
}

/// Resets the note-operation counter (the test seam's reset, ahead of
/// a measured statement).
#[cfg(test)]
pub(crate) fn reset_note_ops() {
    NOTE_OPS.with(|ops| ops.set(0));
}

/// An allocation the walk can expand once it turns out to be the row's
/// own: the container kinds whose children are separate allocations.
/// Every arm is a shared reference, so the walk copies the handle out
/// of its table before expanding (no borrow rides the mutation).
#[derive(Clone, Copy)]
enum Expandable<'a> {
    Items(&'a Arc<Vec<Value>>),
    Entries(&'a Arc<BTreeMap<String, Value>>),
    Node(&'a Node),
    Rel(&'a Relationship),
    Path(&'a relate_core::Path),
}

/// The walk's state: every allocation reached, the expandable
/// containers among them, and the parent-to-child reference edges.
///
/// The two tables are keyed by `Arc` allocation addresses — live heap
/// addresses the allocator picked, never bytes a client supplies — so
/// they sit on the id policy ([`IdMap`], keyless FxHash; the `hash_seed`
/// seam, ADR-0355), not SipHash: a fresh pair is built per landed row,
/// and SipHash on them was profiled as a top cost of the frontier rung.
/// A client shapes how many allocations a row holds, which one row's
/// tables bound, but cannot steer their addresses (the allocator and
/// ASLR do), so there is no key to pre-collide. Nothing here reads
/// table iteration order: `resolve` seeds an unordered worklist and
/// `charge` sums.
#[derive(Default)]
struct RowWalk<'a> {
    seen: IdMap<usize, RowAllocation>,
    expandable: IdMap<usize, Expandable<'a>>,
    /// One entry per reference from inside a parent allocation to a
    /// child — `[acc, acc]` contributes two — activated by the
    /// ownership worklist when (and only when) the parent turns out to
    /// be the row's own. Each edge activates at most once.
    edges: Vec<(usize, usize)>,
    applied: Vec<bool>,
    /// Allocations that just turned out to be the row's own, pending
    /// edge activation and expansion. Each allocation lands here at
    /// most once (row references only grow, and each edge contributes
    /// once).
    owned_worklist: Vec<usize>,
}

impl<'a> RowWalk<'a> {
    /// Note one `Arc` allocation: first sight records its own figure
    /// and holders count, a row-slot sight adds a direct reference.
    /// Returns the allocation's id and whether this was the first
    /// sight.
    fn note<T: ?Sized>(
        &mut self,
        allocation: &Arc<T>,
        own_bytes: u64,
        from_slot: bool,
    ) -> (usize, bool) {
        #[cfg(test)]
        NOTE_OPS.with(|ops| ops.set(ops.get() + 1));
        let id = Arc::as_ptr(allocation) as *const () as usize;
        match self.seen.get_mut(&id) {
            Some(allocation) => {
                if from_slot {
                    allocation.row_refs += 1;
                }
                (id, false)
            }
            None => {
                self.seen.insert(
                    id,
                    RowAllocation {
                        bytes: own_bytes,
                        holders: Arc::strong_count(allocation) as u64,
                        row_refs: u64::from(from_slot),
                        expanded: false,
                        outgoing: Vec::new(),
                    },
                );
                (id, true)
            }
        }
    }

    /// Record one parent-to-child reference edge, indexed on the
    /// parent's outgoing list.
    fn link_edge(&mut self, parent: usize, child: usize) {
        let edge = self.edges.len();
        self.edges.push((parent, child));
        self.applied.push(false);
        self.seen
            .get_mut(&parent)
            .expect("the parent was noted before its children")
            .outgoing
            .push(edge);
    }

    /// Note a value's own allocation — flat payload or container —
    /// link it to the container it was reached through, and register
    /// the container kinds for ownership-gated expansion.
    fn note_value(&mut self, value: &'a Value, parent: Option<usize>) {
        let link = |walk: &mut RowWalk<'a>, id: usize, parent: Option<usize>| {
            if let Some(parent) = parent {
                walk.link_edge(parent, id);
            }
        };
        match value {
            Value::Null
            | Value::Bool(_)
            | Value::Int(_)
            | Value::Float(_)
            | Value::Temporal(_)
            | Value::Point(_)
            | Value::Uuid(_) => {}
            Value::String(text) => {
                let (id, _) = self.note(text, str_logical_bytes(text), parent.is_none());
                link(self, id, parent);
            }
            Value::Bytes(payload) => {
                let (id, _) = self.note(
                    payload,
                    relate_core::bytes_logical_bytes(payload),
                    parent.is_none(),
                );
                link(self, id, parent);
            }
            Value::List(items) | Value::Vector { values: items, .. } => {
                let (id, first) = self.note(
                    items,
                    buffer_bytes(items.len(), size_of::<Value>() as u64),
                    parent.is_none(),
                );
                link(self, id, parent);
                if first {
                    self.expandable.insert(id, Expandable::Items(items));
                }
            }
            Value::Map(entries) => {
                // The map's key figure is deferred to charge time (see
                // `charge`): a borrowed map parameter is noted once per
                // landed row, and summing its keys eagerly would cost
                // rows x keys for a value that never charges.
                let (id, first) = self.note(entries, 0, parent.is_none());
                link(self, id, parent);
                if first {
                    self.expandable.insert(id, Expandable::Entries(entries));
                }
            }
            Value::Node(node) => {
                let (id, first) = self.note(node, size_of::<Node>() as u64, parent.is_none());
                link(self, id, parent);
                if first {
                    self.expandable.insert(id, Expandable::Node(node));
                }
            }
            Value::Relationship(rel) => {
                let (id, first) = self.note(
                    rel,
                    size_of::<Relationship>() as u64 + str_logical_bytes(&rel.rel_type),
                    parent.is_none(),
                );
                link(self, id, parent);
                if first {
                    self.expandable.insert(id, Expandable::Rel(rel));
                }
            }
            Value::Path(path) => {
                // The rels are inline in the path's allocation, so their
                // rel-type strings are part of its own figure — the term
                // [`owned_heap`](super::owned_heap)'s Path arm charges
                // through `rel_heap`.
                // The path's rel-type strings are part of its own
                // figure (the term [`owned_heap`](super::owned_heap)'s
                // Path arm charges through `rel_heap`) but the figure
                // is deferred to charge time (see `charge`), so a
                // borrowed path parameter is never re-summed per row.
                let own = size_of::<relate_core::Path>() as u64
                    + buffer_bytes(path.nodes.len(), size_of::<Node>() as u64)
                    + buffer_bytes(path.rels.len(), size_of::<Relationship>() as u64);
                let (id, first) = self.note(path, own, parent.is_none());
                link(self, id, parent);
                if first {
                    self.expandable.insert(id, Expandable::Path(path));
                }
            }
        }
    }

    /// The ownership resolution, one worklist pass: an allocation that
    /// turns out to be the row's own activates its outgoing edges
    /// (each contributing one internal hold as a row reference of the
    /// child) and, if it is a container, expands — noting its children.
    /// Ownership propagates down through the row's own containers and
    /// stops at the first borrowed one, whatever holds it (parameters,
    /// the store, an outer container); a child that reaches full row
    /// ownership joins the worklist.
    fn resolve(&mut self) {
        // The flat-row fast path: no container children to expand and
        // no reference edges to activate — every allocation's ownership
        // was decided at its note, and `charge` reads it directly.
        if self.edges.is_empty() && self.expandable.is_empty() {
            return;
        }
        self.owned_worklist = self
            .seen
            .iter()
            .filter(|(_, allocation)| allocation.row_owned())
            .map(|(id, _)| *id)
            .collect();
        while let Some(id) = self.owned_worklist.pop() {
            // A container the row owns expands — its children are
            // reached through it. Borrowed containers never expand:
            // their children are borrowed with them and charge nothing,
            // so the walk never walks them.
            if !self.seen[&id].expanded {
                if let Some(expandable) = self.expandable.get(&id).copied() {
                    self.seen.get_mut(&id).expect("just popped").expanded = true;
                    match expandable {
                        Expandable::Items(items) => {
                            for item in items.iter() {
                                self.note_value(item, Some(id));
                            }
                        }
                        Expandable::Entries(entries) => {
                            for (_, value) in entries.iter() {
                                self.note_value(value, Some(id));
                            }
                        }
                        Expandable::Node(node) => self.note_innards(
                            id,
                            created_label_bytes(&node.labels),
                            &node.labels,
                            &node.properties,
                        ),
                        Expandable::Rel(rel) => self.note_properties(
                            id,
                            property_heap(&rel.properties),
                            &rel.properties,
                        ),
                        Expandable::Path(path) => {
                            for node in &path.nodes {
                                self.note_innards(
                                    id,
                                    created_label_bytes(&node.labels),
                                    &node.labels,
                                    &node.properties,
                                );
                            }
                            for rel in &path.rels {
                                self.note_properties(
                                    id,
                                    property_heap(&rel.properties),
                                    &rel.properties,
                                );
                            }
                        }
                    }
                }
            }
            // Its outgoing references activate, once each; a child that
            // just reached full row ownership joins the worklist.
            let outgoing =
                std::mem::take(&mut self.seen.get_mut(&id).expect("just popped").outgoing);
            for edge in outgoing {
                if self.applied[edge] {
                    continue;
                }
                self.applied[edge] = true;
                let child_id = self.edges[edge].1;
                let child = self
                    .seen
                    .get_mut(&child_id)
                    .expect("every edge's child was noted before its parent expanded");
                child.row_refs += 1;
                if child.row_owned() {
                    self.owned_worklist.push(child_id);
                }
            }
        }
    }

    /// A node's label vector and property map, reached through the
    /// node's allocation: leaf allocations (their inline payload bytes
    /// are their own whole figure), linked to their entity.
    fn note_innards(
        &mut self,
        parent: usize,
        label_bytes: u64,
        labels: &Arc<Vec<String>>,
        properties: &Arc<PropertyMap>,
    ) {
        let (id, _) = self.note(labels, label_bytes, false);
        self.link_edge(parent, id);
        self.note_properties(parent, property_heap(properties), properties);
    }

    /// An entity's property map: its whole [`property_heap`] figure
    /// rides the one allocation (its nested payloads stay under that
    /// walk's own ownership rule, the store-borrowed case).
    fn note_properties(&mut self, parent: usize, own_bytes: u64, properties: &Arc<PropertyMap>) {
        let (id, _) = self.note(properties, own_bytes, false);
        self.link_edge(parent, id);
    }

    /// The row's retained bytes: each row-owned allocation's own
    /// figure, exactly once.
    fn charge(self) -> u64 {
        let RowWalk {
            seen, expandable, ..
        } = self;
        seen.into_iter()
            .map(|(id, allocation)| {
                if !allocation.row_owned() {
                    return 0;
                }
                // The deferred figures (a map's keys, a path's rel-type
                // strings) are computed here, only for the row-owned
                // allocations that actually charge; a borrowed map or
                // path is never summed.
                match expandable.get(&id) {
                    Some(Expandable::Entries(entries)) => {
                        allocation.bytes + map_key_figure(entries)
                    }
                    Some(Expandable::Path(path)) => allocation.bytes + path_rel_type_figure(path),
                    _ => allocation.bytes,
                }
            })
            .sum()
    }
}

/// A map's key figure — one entry slot plus the inline key string per
/// entry. Seam-counted: it runs only at charge time and only for
/// row-owned maps, so a pin can assert a borrowed map's keys are never
/// summed.
fn map_key_figure(entries: &Arc<BTreeMap<String, Value>>) -> u64 {
    #[cfg(test)]
    NOTE_OPS.with(|ops| ops.set(ops.get() + 1));
    entries
        .keys()
        .map(|key| OVERHEAD_MAP_ENTRY + str_logical_bytes(key))
        .sum()
}

/// A path's inline rel-type strings (the term
/// [`owned_heap`](super::owned_heap)'s Path arm charges through
/// `rel_heap`). Seam-counted like [`map_key_figure`]: row-owned paths
/// only.
fn path_rel_type_figure(path: &relate_core::Path) -> u64 {
    #[cfg(test)]
    NOTE_OPS.with(|ops| ops.set(ops.get() + 1));
    path.rels
        .iter()
        .map(|rel| str_logical_bytes(&rel.rel_type))
        .sum()
}

/// One `Arc` allocation the row's walk reached: its own byte figure,
/// the holders count at first sight, the row-reference count the
/// ownership resolution accumulates, and whether its children were
/// expanded.
struct RowAllocation {
    /// This allocation's own figure — buffer slots and inline payload
    /// bytes — never its nested values' (the walk reaches those as
    /// their own allocations).
    bytes: u64,
    holders: u64,
    row_refs: u64,
    expanded: bool,
    /// This allocation's outgoing edge indices — the references from
    /// inside it to its children.
    outgoing: Vec<usize>,
}

impl RowAllocation {
    /// Whether every hold of this allocation is the row's own.
    fn row_owned(&self) -> bool {
        self.row_refs == self.holders
    }
}
