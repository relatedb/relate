//! Logical live-graph byte account (ADR-0042, amended by ADR-0202;
//! string-posting formula ADR-0063).
//!
//! The payload terms stay. On top of them the floor charges the containers
//! that still own the payload: dashmap slots, version lists, property
//! vectors, and adjacency chunks. A vector of length `n` is charged for
//! [`reserved_len`]`(n)` elements, not for whatever the allocator rounded
//! the allocation up to.

use std::collections::{HashMap, HashSet};
use std::mem::size_of;
use std::sync::Arc;

use relate_core::{str_logical_bytes, Node, NodeId, RelId, Value};

use crate::group_key::GroupKey;
use crate::redo::RedoOp;
use crate::store::{Interner, LiveNode, LiveRel, PayloadForm, RelPayload};

pub const OVERHEAD_VERSION: u64 = 24;
pub const OVERHEAD_POSTING: u64 = 8;
pub const OVERHEAD_MAP_ENTRY: u64 = 24;
/// `Vec`'s three-word header. Inline in the parent value, so it is charged
/// once for the vector itself and not again for elements whose `size_of`
/// already includes an inner `Vec`.
pub const VEC_HEADER: u64 = 24;

/// The default per-allocation expression cap (ADR-0463). Lives in the
/// sizing-policy module both the execution view and the store services can
/// consult downward; a configured open option replaces it and `none`/`0`
/// disables it.
pub const EXPRESSION_BUDGET_BYTES: u64 = 256 << 20;

/// Capacity the floor charges for a vector of length `len`.
///
/// `0` reserves nothing. Every other length rounds up to the next power of
/// two. `usize::next_power_of_two` maps `0` to `1`, so zero is special-cased.
pub fn reserved_len(len: usize) -> usize {
    if len == 0 {
        0
    } else {
        len.next_power_of_two()
    }
}

/// Saturating [`reserved_len`]: a length whose next power of two cannot be
/// represented saturates to [`u64::MAX`], which every charge check treats as
/// over any cap. The charge path's one spelling of the reservation model
/// under extreme lengths.
pub fn reserved_len_saturating(len: usize) -> u64 {
    if len == 0 {
        0
    } else {
        len.checked_next_power_of_two()
            .map_or(u64::MAX, |n| n as u64)
    }
}

/// Heap bytes of a `Vec<T>` of length `len`: `reserved_len(len) * elem`.
/// The header is not included; callers add [`VEC_HEADER`] when the header
/// is not already inside a parent `size_of`.
pub fn vec_heap(len: usize, elem: u64) -> u64 {
    reserved_len(len) as u64 * elem
}

/// Heap bytes a `Vec<T>` of length `len` gains from `added` more elements:
/// the reservation model's own difference — `reserved_len(len + added) *
/// elem - reserved_len(len) * elem` — the one spelling of the reserved-vec
/// growth the admission estimates and the sealed tier's cost helpers price
/// (a founding case is `vec_growth(0, added, elem)`). Both the length and
/// the byte product saturate, so the canonical growth helper cannot wrap
/// where the charge path's saturating spelling would not.
pub fn vec_growth(len: usize, added: usize, elem: u64) -> u64 {
    reserved_len_saturating(len.saturating_add(added))
        .saturating_mul(elem)
        .saturating_sub(reserved_len_saturating(len).saturating_mul(elem))
}

/// Where one id lands in a span array (ADR-0086), decided once for both the
/// store's estimator ([`Shapes::note_span`]) and the admission estimate's
/// span walk: an id the span already covers adds nothing, an id past it
/// grows the dense array to cover it, and an id the reservation model
/// refuses to place takes one side-map entry instead.
pub enum SpanPlacement {
    /// Already inside the span: no dense growth, no side-map entry.
    Inside,
    /// The array grew to cover the id: the dense bytes it gained.
    Grew(u64),
    /// The id cannot be placed in the array: it takes a side-map entry.
    SideEntry,
}

/// Place `id` in the span array `span` covers, returning what it cost.
///
/// `None` is an id the span array cannot index at all — negative, the range
/// the store refuses. The estimate runs on a batch's ids *before* the
/// commit validates them (the ordered preflight's `validate_ids`), so this
/// must price nothing rather than refuse: the caller's own validation
/// rejects such a batch with the documented storage error before anything
/// applies, and an estimate that panicked here would bypass it.
pub fn place_in_span(span: &mut usize, id: i64, slot: u64) -> Option<SpanPlacement> {
    let idx = usize::try_from(id).ok()?;
    let need = idx.saturating_add(1);
    if need <= *span {
        return Some(SpanPlacement::Inside);
    }
    let Some(after) = reservable_span_bytes(need, slot) else {
        return Some(SpanPlacement::SideEntry);
    };
    let before = id_vec_bytes(*span, slot);
    *span = need;
    Some(SpanPlacement::Grew(after - before))
}

/// Grow `v` so `capacity == reserved_len(len)`.
pub(crate) fn fit_reserved<T>(v: &mut Vec<T>) {
    let want = reserved_len(v.len());
    if v.capacity() == want {
        return;
    }
    let mut fitted = Vec::with_capacity(want);
    fitted.append(v);
    if fitted.capacity() > want {
        fitted.shrink_to(want);
    }
    *v = fitted;
}

/// The shape of one id's version slot (ADR-0529).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainShape {
    Absent,
    /// The live row and its birth timestamp, no version list.
    Inline,
    /// A version list of this many versions.
    Spilled(usize),
}

impl ChainShape {
    /// The shape after a copy-on-write that keeps the previous version.
    pub fn after_cow(self) -> Self {
        match self {
            Self::Absent | Self::Inline => Self::Spilled(2),
            Self::Spilled(n) => Self::Spilled(n + 1),
        }
    }

    /// The shape after a create installs a version on this id.
    pub fn after_install(self) -> Self {
        match self {
            Self::Absent => Self::Inline,
            other => other.after_cow(),
        }
    }

    /// The shape after a delete tombstones the live version.
    pub fn after_delete(self) -> Self {
        match self {
            Self::Inline => Self::Spilled(1),
            other => other,
        }
    }
}

/// Largest dense reservation a store will allocate. A bigger id is a
/// side-map entry, so one persisted id cannot reserve gigabytes before
/// admission runs (ADR-0086, ADR-0532).
pub const DENSE_SPAN_CAP_BYTES: u64 = 1 << 30;

/// Reserved bytes of a dense id span, or `None` when the power-of-two
/// reservation does not fit in `u64`. That id cannot have a slot array
/// (ADR-0532) and lives in the store's side map instead.
pub fn dense_span_bytes(span: usize, slot: u64) -> Option<u64> {
    if span == 0 {
        return Some(0);
    }
    let reserved = span.checked_next_power_of_two()?;
    (reserved as u64).checked_mul(slot)
}

/// Dense reservation a store will allocate for `span`. `None` when the
/// power-of-two product overflows `u64` or exceeds
/// [`DENSE_SPAN_CAP_BYTES`]: that id is a side-map entry, the same gate
/// the store uses when it grows.
pub fn reservable_span_bytes(span: usize, slot: u64) -> Option<u64> {
    match dense_span_bytes(span, slot) {
        Some(bytes) if bytes <= DENSE_SPAN_CAP_BYTES => Some(bytes),
        _ => None,
    }
}

/// Reserved bytes of an id-indexed slot array (ADR-0532). The row itself
/// sits in the slot; this is the vector, holes included. A span that does
/// not fit charges nothing here — the caller charges a side-map entry.
pub fn id_vec_bytes(span: usize, slot: u64) -> u64 {
    dense_span_bytes(span, slot).unwrap_or(0)
}

/// One side-map entry for an id whose slot array does not fit: the
/// hash-map overhead plus the value. The value's spilled heap is separate.
pub fn overflow_entry_bytes(value: u64) -> u64 {
    OVERHEAD_MAP_ENTRY + value
}

/// Heap of a `HashSet<i64>` that can hold `capacity` entries: hashbrown's
/// 9-byte buckets (8-byte key plus control byte), one slack bucket past
/// `capacity`, and the 16-byte hasher state. The census harness's one
/// spelling of a `HashSet<i64>` allocation (research-only, ADR-0476).
#[cfg(feature = "research")]
pub(crate) fn hashset_i64_bytes(capacity: usize) -> u64 {
    (capacity as u64 + 1) * 9 + 16
}

/// Heap of a spilled version chain. An inline row lives in the id slot, so
/// it adds nothing here, and the spilled vector's header does too.
pub fn chain_bytes(shape: ChainShape, stride: u64) -> u64 {
    match shape {
        ChainShape::Spilled(n) if n > 0 => vec_heap(n, stride),
        _ => 0,
    }
}

pub fn node_chain_bytes(shape: ChainShape) -> u64 {
    chain_bytes(shape, crate::graph::node_version_stride())
}

pub fn rel_chain_bytes(shape: ChainShape) -> u64 {
    chain_bytes(shape, crate::graph::rel_version_stride())
}

/// One id family's version-chain tally — the census harness's count shape
/// (research-only, ADR-0476). Beside [`chain_bytes`] so every
/// [`ChainShape`] arm is matched in one module.
#[cfg(feature = "research")]
#[derive(Debug, Default, Clone, Copy)]
pub struct ChainCensus {
    pub inline: u64,
    pub spilled_slots: u64,
    pub spilled_versions: u64,
}

#[cfg(feature = "research")]
impl ChainCensus {
    pub(crate) fn count(&mut self, shape: ChainShape) {
        match shape {
            ChainShape::Inline => self.inline += 1,
            ChainShape::Spilled(n) => {
                self.spilled_slots += 1;
                self.spilled_versions += n as u64;
            }
            ChainShape::Absent => {}
        }
    }
}

/// `to_id` slot, `to_str` slot, and one `Arc<str>` allocation. A second
/// `Arc` clone of the same string is not charged again.
pub fn interned_string_bytes(len: usize) -> u64 {
    2 * OVERHEAD_MAP_ENTRY + 16 + len as u64
}

/// One stub-label cache entry: the dashmap slot, the label-id key, the
/// `Vec<String>` header and its reserved `String` slots, and the copied
/// string heaps. Charged once per distinct label-id list. The interned
/// originals stay on [`interned_string_bytes`]; these are the cache's own
/// copies, and the entry is not evicted when the nodes go away.
pub fn label_vec_cache_bytes(label_count: usize, string_bytes: u64) -> u64 {
    OVERHEAD_MAP_ENTRY
        + VEC_HEADER
        + vec_heap(label_count, size_of::<u32>() as u64)
        + VEC_HEADER
        + vec_heap(label_count, size_of::<String>() as u64)
        + string_bytes
}

/// Heap of one node's adjacency chunks and hop buffers. The chunk-vector
/// header is the id slot (ADR-0532), so it is not charged again here.
pub fn adjacency_bytes(chunk_hops: &[usize]) -> u64 {
    if chunk_hops.is_empty() {
        return 0;
    }
    let chunk_stride = size_of::<(u32, Vec<crate::adjacency::AdjHop>)>() as u64;
    let hop_stride = size_of::<crate::adjacency::AdjHop>() as u64;
    vec_heap(chunk_hops.len(), chunk_stride)
        + chunk_hops
            .iter()
            .map(|n| vec_heap(*n, hop_stride))
            .sum::<u64>()
}

fn prop_vec_heap(count: usize) -> u64 {
    vec_heap(count, size_of::<u32>() as u64) + vec_heap(count, size_of::<Value>() as u64)
}

/// `(label, property)` pairs that currently have a TEXT, RANGE, or POINT
/// index. Gram, prefix/equality, and point bytes are charged only for these
/// pairs — an unindexed string adds no gram bytes.
///
/// Properties are grouped by label so a membership test borrows `&str`
/// instead of allocating a `(String, String)` key.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IndexCharge {
    pub text: HashMap<String, HashSet<String>>,
    pub range: HashMap<String, HashSet<String>>,
    pub point: HashMap<String, HashSet<String>>,
}

impl IndexCharge {
    pub fn from_details<'a>(
        details: impl IntoIterator<Item = &'a crate::redo::IndexDetailSpec>,
    ) -> Self {
        let mut charge = Self::default();
        for detail in details {
            for (label, property, kind) in detail.covered_pairs() {
                let bucket = match kind {
                    crate::redo::IndexKindSpec::Text => &mut charge.text,
                    crate::redo::IndexKindSpec::Range => &mut charge.range,
                    crate::redo::IndexKindSpec::Point => &mut charge.point,
                    _ => continue,
                };
                bucket
                    .entry(label.to_string())
                    .or_default()
                    .insert(property.to_string());
            }
        }
        charge
    }

    fn charges(bucket: &HashMap<String, HashSet<String>>, label: &str, property: &str) -> bool {
        bucket
            .get(label)
            .is_some_and(|props| props.contains(property))
    }
}

fn classify_prop<'a>(
    key: &'a str,
    v: &'a Value,
    record_props: &mut u64,
    indexable: &mut Vec<(&'a str, u64, u64)>,
    strings: &mut Vec<(&'a str, u64, u64, u64)>,
    points: &mut Vec<(&'a str, u64)>,
) {
    let klen = key.len() as u64;
    *record_props += 4 + v.logical_bytes();
    if let Some(gk) = GroupKey::for_property_index(v) {
        indexable.push((key, klen, gk.logical_bytes()));
    }
    if let Value::String(s) = v {
        strings.push((key, klen, s.len() as u64, s.chars().count() as u64));
    }
    if let Value::Point(_) = v {
        points.push((key, klen));
    }
}

pub fn live_node_account(n: &Node, charge: &IndexCharge) -> u64 {
    let labels: Vec<&str> = n.labels.iter().map(String::as_str).collect();
    let mut record_props = 0u64;
    let mut indexable = Vec::new();
    let mut strings = Vec::new();
    let mut points = Vec::new();
    for (k, v) in &*n.properties {
        classify_prop(
            k.as_str(),
            v,
            &mut record_props,
            &mut indexable,
            &mut strings,
            &mut points,
        );
    }
    node_account(
        &labels,
        n.properties.len(),
        record_props,
        &indexable,
        &strings,
        &points,
        charge,
    )
}

/// The same account computed from the compact live record (ADR-0063): the
/// bulk-create path charges straight from parts without materializing a
/// `Node`. Kept numerically identical to [`live_node_account`] by
/// construction — both delegate to `node_account`.
pub fn live_node_account_live(live: &LiveNode, interner: &Interner, charge: &IndexCharge) -> u64 {
    // Hold the interned arcs so the `&str` slices below stay valid. Cloning
    // an `Arc<str>` bumps a refcount; it does not copy the bytes.
    let label_arcs: Vec<Arc<str>> = live.labels.iter().map(|&l| interner.get(l)).collect();
    let labels: Vec<&str> = label_arcs.iter().map(|s| s.as_ref()).collect();
    let prop_arcs: Vec<(Arc<str>, &Value)> = live
        .props
        .iter()
        .map(|(kid, v)| (interner.get(kid), v))
        .collect();
    let mut record_props = 0u64;
    let mut indexable = Vec::new();
    let mut strings = Vec::new();
    let mut points = Vec::new();
    for (key, v) in &prop_arcs {
        classify_prop(
            key.as_ref(),
            v,
            &mut record_props,
            &mut indexable,
            &mut strings,
            &mut points,
        );
    }
    node_account(
        &labels,
        live.props.len(),
        record_props,
        &indexable,
        &strings,
        &points,
        charge,
    )
}

fn rel_record_account(props: u64, heaps: u64) -> u64 {
    OVERHEAD_VERSION + 16 + 4 + props + heaps
}

/// Bytes of the compact relationship record (ADR-0063).
///
/// A stored `Other` keeps its property vectors even when the keys are
/// `id`, `amount`, and `ts`. [`PayloadForm`] is that same choice without
/// interned key ids, so the redo estimate can follow a `SET` without
/// re-classifying the map. Adjacency is not included; the walk and the
/// estimate charge [`adjacency_bytes`] from the out/in maps.
pub(crate) fn payload_form_account(form: &PayloadForm) -> u64 {
    // Value::logical_bytes: String = 16 + len, Int/Float = 8.
    let (props, heaps) = match form {
        PayloadForm::Triple { id, amount, .. } => {
            (str_logical_bytes(id) + amount.logical_bytes() + 8, 0)
        }
        PayloadForm::Other(props) => (
            props.values().map(Value::logical_bytes).sum(),
            prop_vec_heap(props.len()),
        ),
    };
    rel_record_account(props, heaps)
}

/// [`payload_form_account`] from the compact live record.
pub fn live_rel_account_live(r: &LiveRel) -> u64 {
    let (props, heaps) = match &r.payload {
        RelPayload::Triple { id, amount, .. } => {
            (str_logical_bytes(id) + amount.logical_bytes() + 8, 0)
        }
        RelPayload::Other(p) => (
            p.iter().map(|(_, v)| v.logical_bytes()).sum(),
            prop_vec_heap(p.len()),
        ),
    };
    rel_record_account(props, heaps)
}

/// Single accounting core behind [`live_node_account`] and
/// [`live_node_account_live`]. `indexable` is `(key, key_len, group_key_bytes)`
/// per property-indexed property; `strings` is `(key, key_len, byte_len, char_len)`;
/// `points` is `(key, key_len)`. A RANGE index charges one posting per
/// indexable value (the group-key term below). TEXT, POINT, and that
/// RANGE term are charged only when `charge` names that `(label, key)`.
/// Keys are borrowed so the hot path does not allocate a `String` per label
/// or property.
fn node_account(
    labels: &[&str],
    prop_count: usize,
    record_props: u64,
    indexable: &[(&str, u64, u64)],
    strings: &[(&str, u64, u64, u64)],
    points: &[(&str, u64)],
    charge: &IndexCharge,
) -> u64 {
    let mut cost = OVERHEAD_VERSION + 8 + 4 * labels.len() as u64 + record_props;
    // Label-presence postings (the empty-label bucket keys on "").
    if labels.is_empty() {
        cost += OVERHEAD_MAP_ENTRY + OVERHEAD_POSTING;
    }
    for l in labels {
        cost += OVERHEAD_MAP_ENTRY + l.len() as u64 + OVERHEAD_POSTING;
    }
    for l in labels {
        let llen = l.len() as u64;
        for (k, klen, gk) in indexable {
            if IndexCharge::charges(&charge.range, l, k) {
                cost += OVERHEAD_MAP_ENTRY + llen + klen + gk + OVERHEAD_POSTING;
            }
        }
        for (k, _klen, _byte_len, char_len) in strings {
            if IndexCharge::charges(&charge.text, l, k) {
                // One posting id per width-3 window (ADR-0207). The source
                // string is the property already counted in `record_props`.
                cost += crate::graph::trigram_count(*char_len) * OVERHEAD_POSTING;
            }
        }
        for (k, klen) in points {
            if IndexCharge::charges(&charge.point, l, k) {
                cost += OVERHEAD_MAP_ENTRY + llen + klen + 24 + OVERHEAD_POSTING;
            }
        }
    }
    // Label-id and property vectors. Their headers live inside the version
    // stride; this is only the reserved heap.
    cost += vec_heap(labels.len(), size_of::<u32>() as u64);
    cost += prop_vec_heap(prop_count);
    cost
}

/// Each id's slot shape as the batch moves it: the committed shape until
/// an op of the batch changes it.
struct Shapes<F> {
    committed: F,
    batch: HashMap<i64, ChainShape>,
    bytes: fn(ChainShape) -> u64,
    span: usize,
    slot: u64,
    /// `size_of` of the stored value, for an id that cannot join the vector.
    value: u64,
}

impl<F: Fn(i64) -> ChainShape> Shapes<F> {
    fn new(committed: F, bytes: fn(ChainShape) -> u64, span: usize, slot: u64, value: u64) -> Self {
        Self {
            committed,
            batch: HashMap::new(),
            bytes,
            span,
            slot,
            value,
        }
    }

    fn shape_of(&self, id: i64) -> ChainShape {
        self.batch
            .get(&id)
            .copied()
            .unwrap_or_else(|| (self.committed)(id))
    }

    /// Bytes the slot array gains when `id` extends it. An id the store
    /// will not place in the array adds one side-map entry instead, and
    /// only while the id has no slot yet. The placement itself is
    /// [`place_in_span`]'s one spelling — the store's side-map rule is
    /// the shape read here, not a second copy of the span arithmetic.
    fn note_span(&mut self, id: i64) -> i64 {
        match place_in_span(&mut self.span, id, self.slot) {
            // An id the array cannot index is refused by the commit's own
            // validation before anything applies, so it prices nothing.
            None | Some(SpanPlacement::Inside) => 0,
            Some(SpanPlacement::Grew(bytes)) => bytes as i64,
            Some(SpanPlacement::SideEntry) => {
                if !matches!(self.shape_of(id), ChainShape::Absent) {
                    return 0;
                }
                overflow_entry_bytes(self.value) as i64
            }
        }
    }

    /// Move `id`'s slot by `step`; the slot bytes it adds.
    fn step(&mut self, id: i64, step: impl FnOnce(ChainShape) -> ChainShape) -> i64 {
        let before = self
            .batch
            .get(&id)
            .copied()
            .unwrap_or_else(|| (self.committed)(id));
        let after = step(before);
        self.batch.insert(id, after);
        (self.bytes)(after) as i64 - (self.bytes)(before) as i64
    }
}

/// In-place edits replace the record. The first edit of a committed record
/// copies it: the old version stays, and the slot spills or grows by one.
fn version_edit<F: Fn(i64) -> ChainShape>(
    in_batch: bool,
    before: u64,
    after: u64,
    shapes: &mut Shapes<F>,
    id: i64,
) -> i64 {
    if in_batch {
        after as i64 - before as i64
    } else {
        after as i64 + shapes.step(id, ChainShape::after_cow)
    }
}

/// A redo batch's live-size delta, split by what the checkpoint fold
/// reclaims: `spill` is that part — a delete's tombstones (ADR-0529, the
/// original and largest term) plus the adjacency growth the fold reclaims
/// with them: the on-demand region, its chunk, and a founding delete's
/// side-map entry — and [`refusable`](Self::refusable) is `total`'s
/// remainder. GC returns the spill bytes, so admission does not refuse a
/// batch for them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RedoDelta {
    /// The batch's whole live-size delta.
    pub total: i64,
    /// The part of `total` the checkpoint fold reclaims.
    pub spill: i64,
}

impl RedoDelta {
    /// The share of the batch admission may refuse — `total` less the
    /// fold-reclaimed spill. Derived, never stored: one spelling of the
    /// relation, so the two can never disagree.
    pub fn refusable(&self) -> i64 {
        self.total - self.spill
    }
}

/// Sequential live-size delta of `ops` against `lookup` (committed graph).
///
/// Every node is accounted under `charge`, the index charge in force after
/// `ops` (DDL in the same batch included). When `ops` also change the
/// charge, [`Graph::estimate_redo_delta`](crate::graph::Graph::estimate_redo_delta)
/// re-accounts the committed live set from the old charge to the new one;
/// together the two are exact for created, modified, deleted, and untouched
/// nodes alike. A delete keeps the version until GC: it adds nothing,
/// except the spill of an inline row into a one-version chain, which is
/// also reported as [`RedoDelta::spill`]. A set on a committed
/// node copies the version and adds the new record; a set on a node this
/// batch already created or edited edits that record in place.
#[allow(clippy::too_many_arguments)]
pub fn estimate_redo_delta(
    interner: &Interner,
    ops: &[RedoOp],
    lookup_node: impl Fn(NodeId) -> Option<Node>,
    lookup_rel: impl Fn(RelId) -> Option<PayloadForm>,
    node_shape: impl Fn(NodeId) -> ChainShape,
    rel_shape: impl Fn(RelId) -> ChainShape,
    node_span: usize,
    rel_span: usize,
    charge: &IndexCharge,
) -> RedoDelta {
    let mut nodes: HashMap<NodeId, Option<Node>> = HashMap::new();
    let mut rels: HashMap<RelId, Option<PayloadForm>> = HashMap::new();
    let mut node_shapes = Shapes::new(
        node_shape,
        node_chain_bytes,
        node_span,
        crate::graph::node_slot(),
        crate::graph::node_value(),
    );
    let mut rel_shapes = Shapes::new(
        rel_shape,
        rel_chain_bytes,
        rel_span,
        crate::graph::rel_slot(),
        crate::graph::rel_value(),
    );
    let mut delta = 0i64;
    let mut spill = 0i64;

    let get_node = |nodes: &HashMap<NodeId, Option<Node>>, id: NodeId| -> Option<Node> {
        if let Some(v) = nodes.get(&id) {
            return v.clone();
        }
        lookup_node(id)
    };
    let get_rel = |rels: &HashMap<RelId, Option<PayloadForm>>, id: RelId| -> Option<PayloadForm> {
        if let Some(v) = rels.get(&id) {
            return v.clone();
        }
        lookup_rel(id)
    };

    for op in ops {
        match op {
            RedoOp::CreateNode {
                id,
                labels,
                properties,
            } => {
                let n = Node {
                    id: *id,
                    labels: labels.clone(),
                    properties: properties.clone(),
                };
                delta += live_node_account(&n, charge) as i64
                    + node_shapes.note_span(*id)
                    + node_shapes.step(*id, ChainShape::after_install);
                nodes.insert(*id, Some(n));
            }
            RedoOp::CreateRel {
                id,
                start: _,
                end: _,
                rel_type: _,
                properties,
            } => {
                let form = PayloadForm::from_map(interner, (**properties).clone().into());
                delta += payload_form_account(&form) as i64
                    + rel_shapes.note_span(*id)
                    + rel_shapes.step(*id, ChainShape::after_install);
                rels.insert(*id, Some(form));
            }
            RedoOp::DeleteNode { id } => {
                // Retained until GC. Mark it so a later op in this batch
                // does not edit the dropped record.
                if !matches!(nodes.insert(*id, None), Some(None)) {
                    let step = node_shapes.step(*id, ChainShape::after_delete);
                    delta += step;
                    spill += step;
                }
            }
            RedoOp::DeleteRel { id } => {
                if !matches!(rels.insert(*id, None), Some(None)) {
                    let step = rel_shapes.step(*id, ChainShape::after_delete);
                    delta += step;
                    spill += step;
                }
            }
            RedoOp::SetNodeProp { id, key, value } => {
                let in_batch = nodes.contains_key(id);
                let mut n = match get_node(&nodes, *id) {
                    Some(n) => n,
                    None => continue,
                };
                let before = live_node_account(&n, charge);
                Arc::make_mut(&mut n.properties).insert(key.clone(), value.clone());
                let after = live_node_account(&n, charge);
                delta += version_edit(in_batch, before, after, &mut node_shapes, *id);
                nodes.insert(*id, Some(n));
            }
            RedoOp::RemoveNodeProp { id, key } => {
                let in_batch = nodes.contains_key(id);
                let mut n = match get_node(&nodes, *id) {
                    Some(n) => n,
                    None => continue,
                };
                let before = live_node_account(&n, charge);
                Arc::make_mut(&mut n.properties).remove(key);
                let after = live_node_account(&n, charge);
                delta += version_edit(in_batch, before, after, &mut node_shapes, *id);
                nodes.insert(*id, Some(n));
            }
            RedoOp::SetRelProp { id, key, value } => {
                let in_batch = rels.contains_key(id);
                let mut form = match get_rel(&rels, *id) {
                    Some(form) => form,
                    None => continue,
                };
                let before = payload_form_account(&form);
                form.set(interner, key, value.clone());
                let after = payload_form_account(&form);
                delta += version_edit(in_batch, before, after, &mut rel_shapes, *id);
                rels.insert(*id, Some(form));
            }
            RedoOp::RemoveRelProp { id, key } => {
                let in_batch = rels.contains_key(id);
                let mut form = match get_rel(&rels, *id) {
                    Some(form) => form,
                    None => continue,
                };
                let before = payload_form_account(&form);
                form.remove(interner, key);
                let after = payload_form_account(&form);
                delta += version_edit(in_batch, before, after, &mut rel_shapes, *id);
                rels.insert(*id, Some(form));
            }
            RedoOp::AddLabels { id, labels } => {
                let in_batch = nodes.contains_key(id);
                let mut n = match get_node(&nodes, *id) {
                    Some(n) => n,
                    None => continue,
                };
                let before = live_node_account(&n, charge);
                for l in labels {
                    if !n.labels.iter().any(|x| x == l) {
                        Arc::make_mut(&mut n.labels).push(l.clone());
                    }
                }
                let after = live_node_account(&n, charge);
                delta += version_edit(in_batch, before, after, &mut node_shapes, *id);
                nodes.insert(*id, Some(n));
            }
            RedoOp::RemoveLabels { id, labels } => {
                let in_batch = nodes.contains_key(id);
                let mut n = match get_node(&nodes, *id) {
                    Some(n) => n,
                    None => continue,
                };
                let before = live_node_account(&n, charge);
                Arc::make_mut(&mut n.labels).retain(|l| !labels.contains(l));
                let after = live_node_account(&n, charge);
                delta += version_edit(in_batch, before, after, &mut node_shapes, *id);
                nodes.insert(*id, Some(n));
            }
            RedoOp::CreateIndex { .. }
            | RedoOp::DropIndex { .. }
            | RedoOp::CreateConstraint { .. }
            | RedoOp::DropConstraint { .. }
            | RedoOp::CatalogIndexDetail { .. }
            | RedoOp::CatalogDerivedConstraint { .. }
            | RedoOp::PutDatabase { .. }
            | RedoOp::DropDatabase { .. }
            | RedoOp::PutGraphType { .. }
            | RedoOp::SetCurrentGraphType { .. }
            | RedoOp::PullIdentityPut { .. }
            | RedoOp::OntologySet { .. }
            | RedoOp::OntologyDrop { .. } => {}
        }
    }
    RedoDelta {
        total: delta,
        spill,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn reserved_len_is_zero_or_the_next_power_of_two() {
        assert_eq!(reserved_len(0), 0);
        assert_eq!(reserved_len(1), 1);
        assert_eq!(reserved_len(2), 2);
        assert_eq!(reserved_len(3), 4);
        assert_eq!(reserved_len(20), 32);
    }

    #[test]
    fn reserved_len_saturating_matches_reserved_len_and_saturates() {
        for len in [0, 1, 2, 3, 20, 1_000_000] {
            assert_eq!(reserved_len_saturating(len), reserved_len(len) as u64);
        }
        assert_eq!(reserved_len_saturating(usize::MAX), u64::MAX);
    }

    #[test]
    fn other_that_gains_the_triple_keys_keeps_its_vectors() {
        let interner = Interner::default();
        let partial = BTreeMap::from([
            ("id".into(), Value::String("a".into())),
            ("amount".into(), Value::Int(1)),
        ]);
        let mut form = PayloadForm::from_map(&interner, partial.clone());
        let created = payload_form_account(&form);
        form.set(&interner, "ts", Value::Int(5));
        let grown = payload_form_account(&form);
        assert!(matches!(form, PayloadForm::Other(_)));
        assert_eq!(
            grown,
            created + (prop_vec_heap(3) - prop_vec_heap(2)) + Value::Int(5).logical_bytes()
        );

        let mut triple = BTreeMap::from([
            ("id".into(), Value::String("a".into())),
            ("amount".into(), Value::Int(1)),
        ]);
        triple.insert("ts".into(), Value::Int(5));
        let as_created = PayloadForm::from_map(&interner, triple);
        assert!(matches!(as_created, PayloadForm::Triple { .. }));
        assert_eq!(payload_form_account(&as_created) + prop_vec_heap(3), grown);

        let mut stored = RelPayload::from_map(&interner, partial);
        stored.set(&interner, "ts", Value::Int(5));
        assert_eq!(stored.form(&interner), form);
        assert!(matches!(stored, RelPayload::Other(_)));

        let triple_map = BTreeMap::from([
            ("id".into(), Value::String("b".into())),
            ("amount".into(), Value::Int(2)),
            ("ts".into(), Value::Int(9)),
        ]);
        let mut stored_triple = RelPayload::from_map(&interner, triple_map.clone());
        let mut form_triple = PayloadForm::from_map(&interner, triple_map);
        stored_triple.set(&interner, "note", Value::String("x".into()));
        form_triple.set(&interner, "note", Value::String("x".into()));
        assert_eq!(stored_triple.form(&interner), form_triple);
        assert!(matches!(stored_triple, RelPayload::Other(_)));
        assert!(matches!(form_triple, PayloadForm::Other(_)));
    }

    #[test]
    fn long_string_index_exceeds_payload() {
        let mut n = Node {
            id: 1,
            labels: Arc::new(vec!["Person".into()]),
            properties: relate_core::PropertyMap::share(BTreeMap::new()),
        };
        Arc::make_mut(&mut n.properties).insert("name".into(), Value::String("abcdefghij".into()));
        let plain = live_node_account(&n, &IndexCharge::default());
        let mut indexed = IndexCharge::default();
        indexed
            .text
            .entry("Person".into())
            .or_default()
            .insert("name".into());
        let with_grams = live_node_account(&n, &indexed);
        assert!(
            with_grams > plain,
            "a text index charges gram bytes; an unindexed string does not"
        );
        let char_len = 10u64;
        assert_eq!(
            with_grams - plain,
            crate::graph::trigram_count(char_len) * OVERHEAD_POSTING
        );
    }

    #[test]
    fn parts_account_matches_node_account() {
        // ADR-0063: the bulk-path charge from `LiveNode` parts must equal
        // the materialized-`Node` charge exactly (one shared core).
        let interner = Interner::default();
        // (labels, props) fixtures for one `Node` / `LiveNode` pair each.
        type Fixture = (Vec<&'static str>, Vec<(&'static str, Value)>);
        let cases: Vec<Fixture> = vec![
            (vec![], vec![]),
            (
                vec!["Person"],
                vec![("name", Value::String("Tomas 1".into()))],
            ),
            (
                vec!["Person", "Employee"],
                vec![
                    ("name", Value::String("Åsa 🦀".into())),
                    ("id", Value::Int(7)),
                    ("score", Value::Float(1.5)),
                    ("nick", Value::String("x".into())),
                ],
            ),
            // A literal "*" label collides with the any-label bucket.
            (vec!["*"], vec![("s", Value::String("abc".into()))]),
        ];
        for (labels, props) in cases {
            let n = Node {
                id: 1,
                labels: Arc::new(labels.iter().map(|l| l.to_string()).collect()),
                properties: relate_core::PropertyMap::share(
                    props
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.clone()))
                        .collect::<std::collections::BTreeMap<_, _>>(),
                ),
            };
            let live = LiveNode::from_parts(
                &interner,
                labels.iter().map(|l| l.to_string()).collect(),
                props
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.clone()))
                    .collect(),
            );
            let none = IndexCharge::default();
            assert_eq!(
                live_node_account(&n, &none),
                live_node_account_live(&live, &interner, &none),
                "labels={labels:?} props={props:?}"
            );
        }
    }
}
