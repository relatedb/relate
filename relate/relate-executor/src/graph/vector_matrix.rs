//! Packed `f32` rows for vector SEARCH (ADR-0705): the follow-up ADR-0256
//! deferred. A node vector index's candidates are kept as one contiguous
//! `f32` row per node, so the scan walks packed coordinates instead of
//! resolving every candidate's property map and walking its `Value` list.
//!
//! The matrix is a cache over the committed graph, never a second source of
//! truth:
//!
//! - **Built lazily** by the first vector SEARCH over a `(label, property)`
//!   pair, at the reader's snapshot, from the label's carriers.
//! - **Kept current incrementally.** The node write hooks
//!   ([`Graph::index_add`], [`Graph::suspect_postings`]) record each written
//!   carrier as dirty, stamped with the write's commit timestamp. A reader
//!   refreshes only the dirty rows whose write its snapshot can see — a
//!   property write elsewhere on the node (an access counter, say) costs one
//!   row refresh, not a rebuild.
//! - **Served only when exact.** A reader may use the matrix when its
//!   snapshot is at or after the matrix's and every pending dirty write is
//!   newer than that snapshot. Anything else — an older pinned snapshot, a
//!   view with uncommitted writes of its own, a relationship index, a
//!   multi-label index, a query of another dimension, or a carrier whose
//!   coordinates do not pack into `f32` exactly — takes the `Value` scan.
//!   A first build also declines while the store's head is past the
//!   reader's snapshot: a write that landed before the slot was registered
//!   was never dirty-marked, so a build behind the head could publish rows
//!   stale for every later reader.
//!
//! Scores are bit-identical to the `Value` scan: rows hold only `f32`-exact
//! coordinates, and [`crate::vector_similarity::similarity_row`] widens each
//! to the same `f64` and accumulates in the same lanes and order.
//!
//! Lock order: the matrix lock, then (briefly) the dirty lock. The write
//! hooks take only the dirty lock, while a `nodes` guard may be held. No
//! path holds the dirty lock while it takes any other lock, so the two
//! orders cannot cycle. The write half of the matrix lock covers the
//! build or refresh only — the caller's scan runs under the read half,
//! so concurrent searches of one pair never serialize behind a builder.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};

use relate_core::{NodeId, Value};

use super::access::visible_version;
#[cfg(feature = "test-hooks")]
use super::build_counter::SharedCounter;
use super::Graph;
use crate::vector_similarity::{value_coord, value_coords};

#[cfg(feature = "test-hooks")]
static SERVED: SharedCounter = SharedCounter::new();

/// Take the number of vector searches over `label` the matrix served, in
/// this process, resetting it. A test seam behind the off-by-default
/// `test-hooks` feature: whether the matrix or the `Value` scan answered
/// has no other observable surface (the answers are identical by design),
/// and a differential test must prove the matrix side actually ran.
/// Process-wide and name-keyed (the canonical [`SharedCounter`]), not
/// per-thread: the facade runs queries on a spawned `QueryWorker` thread,
/// so the bump and the read never share a thread. Each test indexes its
/// own label, which keeps concurrently running tests' counts separate.
#[cfg(feature = "test-hooks")]
#[doc(hidden)]
pub fn take_vector_matrix_serves(label: &str) -> u64 {
    SERVED.take(label)
}

#[cfg(feature = "test-hooks")]
fn note_served(label: &str) {
    SERVED.bump(label);
}

#[cfg(not(feature = "test-hooks"))]
fn note_served(_label: &str) {}

/// One `(label, property)` pair's matrix and the writes it has not seen.
#[derive(Debug, Default)]
pub(super) struct VectorMatrixSlot {
    /// Carrier id → `(oldest, newest)` commit timestamps of the writes to
    /// it that its row does not reflect yet. Writes arrive in commit order
    /// (one writer), so a row refreshed at `ts` reflects every write
    /// `<= ts`; a newer write keeps the entry, conservatively narrowed to
    /// `ts + 1`.
    dirty: Mutex<HashMap<NodeId, (u64, u64)>>,
    matrix: RwLock<Option<VectorMatrix>>,
}

impl VectorMatrixSlot {
    /// Whether a write the snapshot `ts` can see is still unapplied.
    fn has_pending_at(&self, ts: u64) -> bool {
        self.dirty
            .lock()
            .expect("vector matrix dirty lock poisoned")
            .values()
            .any(|&(oldest, _)| oldest <= ts)
    }

    /// Whether a reader at `ts` may use `m` for a `dim`-length query: the
    /// matrix's own exactness (dimension, snapshot, no inexact carrier)
    /// and no write the reader can see still unapplied. The one spelling
    /// of the module's serve gate — the read-lock fast path and the
    /// post-refresh check both consult it, so a new decline reason lands
    /// in one place.
    fn serves_reader(&self, m: &VectorMatrix, dim: usize, ts: u64) -> bool {
        m.serves(dim, ts) && !self.has_pending_at(ts)
    }

    /// Remove and return the ids with a write `<= ts`; an id that also has
    /// a newer write stays, narrowed to start at `ts + 1`.
    fn take_pending_at(&self, ts: u64) -> Vec<NodeId> {
        let mut dirty = self
            .dirty
            .lock()
            .expect("vector matrix dirty lock poisoned");
        let mut taken = Vec::new();
        dirty.retain(|&id, (oldest, newest)| {
            if *oldest > ts {
                return true;
            }
            taken.push(id);
            if *newest > ts {
                *oldest = ts + 1;
                return true;
            }
            false
        });
        taken
    }

    fn note_write(&self, id: NodeId, ts: u64) {
        let mut dirty = self
            .dirty
            .lock()
            .expect("vector matrix dirty lock poisoned");
        let entry = dirty.entry(id).or_insert((ts, ts));
        entry.0 = entry.0.min(ts);
        entry.1 = entry.1.max(ts);
    }
}

/// What one carrier contributes to the matrix.
enum Packed {
    /// A row of `f32`-exact coordinates.
    Row(Vec<f32>),
    /// Not a candidate: not visible, no label, no property, the wrong
    /// length, or a coordinate the scan would skip on (a non-number or a
    /// non-finite number). The `Value` scan skips exactly these.
    Skip,
    /// A candidate whose coordinates do not all fit `f32` exactly (a
    /// FLOAT64 or large INT vector). The matrix cannot score it
    /// bit-identically, so the scan falls back while one exists.
    Inexact,
}

/// The packed rows of one `(label, property)` pair at `built_ts`.
#[derive(Debug)]
pub(crate) struct VectorMatrix {
    dim: usize,
    built_ts: u64,
    ids: Vec<NodeId>,
    rows: Vec<f32>,
    pos: HashMap<NodeId, usize>,
    inexact: HashSet<NodeId>,
}

impl VectorMatrix {
    fn new(dim: usize, built_ts: u64) -> Self {
        Self {
            dim,
            built_ts,
            ids: Vec::new(),
            rows: Vec::new(),
            pos: HashMap::new(),
            inexact: HashSet::new(),
        }
    }

    /// Every packed row with its node id, in storage order (the scan's
    /// ranking does not depend on it: ties order by id).
    pub(crate) fn rows(&self) -> impl Iterator<Item = (NodeId, &[f32])> + '_ {
        self.ids
            .iter()
            .copied()
            .zip(self.rows.chunks_exact(self.dim))
    }

    fn apply(&mut self, id: NodeId, packed: Packed) {
        match packed {
            Packed::Row(row) => {
                self.inexact.remove(&id);
                match self.pos.get(&id) {
                    Some(&at) => {
                        self.rows[at * self.dim..(at + 1) * self.dim].copy_from_slice(&row)
                    }
                    None => {
                        self.pos.insert(id, self.ids.len());
                        self.ids.push(id);
                        self.rows.extend_from_slice(&row);
                    }
                }
            }
            Packed::Skip => {
                self.inexact.remove(&id);
                self.remove(id);
            }
            Packed::Inexact => {
                self.remove(id);
                self.inexact.insert(id);
            }
        }
    }

    /// Swap-remove `id`'s row, keeping the rows dense.
    fn remove(&mut self, id: NodeId) {
        let Some(at) = self.pos.remove(&id) else {
            return;
        };
        let last = self.ids.len() - 1;
        if at != last {
            let moved = self.ids[last];
            self.ids[at] = moved;
            self.rows
                .copy_within(last * self.dim..(last + 1) * self.dim, at * self.dim);
            self.pos.insert(moved, at);
        }
        self.ids.pop();
        self.rows.truncate(last * self.dim);
    }

    /// Whether this matrix can be brought current for a `dim`-length
    /// query read at snapshot `ts`: the dimensions match and the rows
    /// were built at or before the reader's snapshot. [`serves`] adds the
    /// no-inexact-carrier conjunct on top; the refresh decline arm is
    /// this predicate's negation — one spelling for both.
    fn can_refresh(&self, dim: usize, ts: u64) -> bool {
        self.dim == dim && self.built_ts <= ts
    }

    /// Whether a reader at `ts` sees exactly these rows, given no pending
    /// write at or before `ts` (the slot's half of the check).
    fn serves(&self, dim: usize, ts: u64) -> bool {
        self.can_refresh(dim, ts) && self.inexact.is_empty()
    }
}

/// Pack one property value as the `Value` scan would read it.
fn pack_value(value: &Value, dim: usize) -> Packed {
    // The scan's own coordinate-list read (a VECTOR or a list, else skip).
    let Some(coords) = value_coords(value) else {
        return Packed::Skip;
    };
    if coords.len() != dim {
        return Packed::Skip;
    }
    let mut row = Vec::with_capacity(dim);
    let mut exact = true;
    for coord in coords.iter() {
        // `value_coord` is the scan's own coordinate read: a non-number or
        // non-finite coordinate skips the whole candidate there, so here.
        let Some(c) = value_coord(coord) else {
            return Packed::Skip;
        };
        let narrowed = c as f32;
        exact &= f64::from(narrowed) == c;
        row.push(narrowed);
    }
    if exact {
        Packed::Row(row)
    } else {
        Packed::Inexact
    }
}

impl Graph {
    /// Record a node write for every vector matrix keyed on one of
    /// `labels` (the written version's labels, or the labels it is losing).
    /// Free when no matrix has been built.
    pub(super) fn vector_matrix_note_write(&self, labels: &[u32], id: NodeId) {
        if self.indexes.vector_matrices.is_empty() {
            return;
        }
        let ts = self.read_ts();
        for entry in self.indexes.vector_matrices.iter() {
            if labels.contains(&entry.key().0) {
                entry.value().note_write(id, ts);
            }
        }
    }

    /// The carrier's contribution at snapshot `ts`.
    fn pack_node(&self, id: NodeId, lid: u32, kid: u32, ts: u64, dim: usize) -> Packed {
        let Some(versions) = self.nodes.get(&id) else {
            return Packed::Skip;
        };
        let Some(version) = visible_version(&versions, ts) else {
            return Packed::Skip;
        };
        let live = &version.live;
        if !live.labels.contains(&lid) {
            return Packed::Skip;
        }
        match live.props.get(kid) {
            Some(value) => pack_value(value, dim),
            None => Packed::Skip,
        }
    }

    /// Run `scan` over the packed rows of `(label, key)` as a reader at
    /// snapshot `ts` with a `dim`-length query sees them, building or
    /// refreshing the matrix first when needed. `None` when the matrix
    /// cannot serve this reader exactly; the caller then takes the `Value`
    /// scan. The caller guarantees `ts` is a committed snapshot with no
    /// uncommitted writes of its own. The build or refresh holds the write
    /// lock for the mutation only — the scan runs under the read lock,
    /// re-checked, so concurrent searches of the same pair never
    /// serialize behind a builder.
    pub(crate) fn with_vector_matrix<R>(
        &self,
        label: &str,
        key: &str,
        ts: u64,
        dim: usize,
        scan: impl FnOnce(&VectorMatrix) -> R,
    ) -> Option<R> {
        let lid = self.label_id(label)?;
        let kid = self.prop_key_id(key)?;
        // Register the slot before any build reads the graph, so a write
        // that lands during the build is recorded as dirty.
        let slot: Arc<VectorMatrixSlot> =
            Arc::clone(&self.indexes.vector_matrices.entry((lid, kid)).or_default());

        {
            let matrix = slot.matrix.read().expect("vector matrix lock poisoned");
            if let Some(m) = matrix.as_ref() {
                if slot.serves_reader(m, dim, ts) {
                    note_served(label);
                    return Some(scan(m));
                }
            }
        }

        // The mutation only: build or refresh under the write lock.
        {
            let mut matrix = slot.matrix.write().expect("vector matrix lock poisoned");
            match matrix.as_mut() {
                // A matrix this reader cannot bring current — one built
                // past its snapshot, or for another dimension. Never
                // rewind, never evict a valid matrix — this reader scans
                // `Value`s.
                Some(m) if !m.can_refresh(dim, ts) => return None,
                Some(m) => {
                    for id in slot.take_pending_at(ts) {
                        let packed = self.pack_node(id, lid, kid, ts, dim);
                        m.apply(id, packed);
                    }
                    m.built_ts = ts;
                }
                None => {
                    // A build at `ts` is only exact when `ts` covers every
                    // write whose hook could have run: a write that landed
                    // before the slot was registered was never
                    // dirty-marked, so a first build behind the store's
                    // head could publish rows stale for every later
                    // reader. Decline; a reader at the head builds fresh.
                    if self.read_ts() > ts {
                        return None;
                    }
                    let mut m = VectorMatrix::new(dim, ts);
                    for id in self.node_ids_with_label_at(label, ts) {
                        let packed = self.pack_node(id, lid, kid, ts, dim);
                        m.apply(id, packed);
                    }
                    // The build read every carrier at `ts`: writes up to
                    // `ts` are reflected, newer ones stay pending.
                    slot.take_pending_at(ts);
                    *matrix = Some(m);
                }
            }
        }

        // Serve under the read lock, re-checked: a concurrent refresh past
        // `ts` in the gap declines to the `Value` scan.
        let matrix = slot.matrix.read().expect("vector matrix lock poisoned");
        let m = matrix.as_ref()?;
        if !slot.serves_reader(m, dim, ts) {
            return None;
        }
        note_served(label);
        Some(scan(m))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn floats(xs: &[f64]) -> Value {
        Value::List(Arc::new(xs.iter().map(|&x| Value::Float(x)).collect()))
    }

    #[test]
    fn pack_value_mirrors_the_value_scans_skip_and_exactness_rules() {
        assert!(
            matches!(pack_value(&floats(&[1.0, 0.5]), 2), Packed::Row(r) if r == vec![1.0, 0.5])
        );
        assert!(
            matches!(pack_value(&floats(&[1.0]), 2), Packed::Skip),
            "wrong length"
        );
        assert!(
            matches!(pack_value(&floats(&[f64::NAN, 1.0]), 2), Packed::Skip),
            "non-finite"
        );
        assert!(
            matches!(pack_value(&Value::Int(3), 1), Packed::Skip),
            "scalar"
        );
        assert!(
            matches!(pack_value(&floats(&[0.1, 1.0]), 2), Packed::Inexact),
            "0.1 is not f32-exact"
        );
        let ints = Value::List(Arc::new(vec![Value::Int(3), Value::Int(1 << 30)]));
        assert!(
            matches!(pack_value(&ints, 2), Packed::Row(_)),
            "2^30 is f32-exact"
        );
        let big = Value::List(Arc::new(vec![Value::Int((1 << 30) + 1)]));
        assert!(
            matches!(pack_value(&big, 1), Packed::Inexact),
            "2^30+1 is not"
        );
    }

    #[test]
    fn apply_keeps_rows_dense_across_updates_and_removals() {
        let mut m = VectorMatrix::new(2, 1);
        for id in 1..=4 {
            m.apply(id, Packed::Row(vec![id as f32, 0.0]));
        }
        m.apply(2, Packed::Skip);
        m.apply(3, Packed::Row(vec![30.0, 1.0]));
        m.apply(1, Packed::Inexact);
        let mut rows: Vec<(NodeId, Vec<f32>)> = m.rows().map(|(id, r)| (id, r.to_vec())).collect();
        rows.sort_by_key(|(id, _)| *id);
        assert_eq!(rows, vec![(3, vec![30.0, 1.0]), (4, vec![4.0, 0.0])]);
        assert!(!m.serves(2, 1), "an inexact carrier blocks the matrix");
        m.apply(1, Packed::Skip);
        assert!(m.serves(2, 1));
        assert!(!m.serves(3, 1), "another dimension");
        assert!(!m.serves(2, 0), "an older snapshot");
    }

    #[test]
    fn dirty_entries_drain_only_up_to_the_readers_snapshot() {
        let slot = VectorMatrixSlot::default();
        slot.note_write(7, 5);
        slot.note_write(7, 9);
        slot.note_write(8, 12);
        assert!(!slot.has_pending_at(4));
        assert!(slot.has_pending_at(5));
        assert_eq!(slot.take_pending_at(6), vec![7]);
        // 7's write at 9 is still pending, narrowed to start after 6.
        assert!(slot.has_pending_at(7));
        let mut later = slot.take_pending_at(12);
        later.sort_unstable();
        assert_eq!(later, vec![7, 8]);
        assert!(!slot.has_pending_at(u64::MAX));
    }
}
