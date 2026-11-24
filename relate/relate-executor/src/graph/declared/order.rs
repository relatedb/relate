//! RANGE index order keys and postings.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::{Bound, ControlFlow};
use std::sync::Arc;

use chrono::{Datelike, NaiveDateTime, NaiveTime, Timelike};
use relate_core::{NodeId, Temporal, Value};
use relate_ir::BinaryOp;
use relate_model::canonical_key::f64_exact_i64;

use crate::eval;
use crate::group_key::GroupKey;

use super::DrainState;
use crate::graph::unposted::UnpostedMark;

/// Node ids posted under one key. A unique value stays a single id; a
/// second distinct id promotes the arm to a set.
#[derive(Clone, Debug)]
pub(super) enum Postings {
    One(NodeId),
    Many(HashSet<NodeId>),
}

impl Postings {
    fn insert(&mut self, id: NodeId) {
        match self {
            Postings::One(existing) if *existing == id => {}
            Postings::One(existing) => {
                let mut set = HashSet::with_capacity(2);
                set.insert(*existing);
                set.insert(id);
                *self = Postings::Many(set);
            }
            Postings::Many(set) => {
                set.insert(id);
            }
        }
    }

    /// `true` when the posting is now empty and the caller should drop the key.
    fn remove(&mut self, id: NodeId) -> bool {
        let demote = match self {
            Postings::One(existing) if *existing == id => return true,
            Postings::One(_) => return false,
            Postings::Many(set) => {
                set.remove(&id);
                match set.len() {
                    0 => return true,
                    1 => set.iter().copied().next(),
                    _ => None,
                }
            }
        };
        if let Some(only) = demote {
            *self = Postings::One(only);
        }
        false
    }

    #[cfg(test)]
    pub(super) fn contains(&self, id: NodeId) -> bool {
        match self {
            Postings::One(existing) => *existing == id,
            Postings::Many(set) => set.contains(&id),
        }
    }

    fn len(&self) -> usize {
        match self {
            Postings::One(_) => 1,
            Postings::Many(set) => set.len(),
        }
    }

    /// The bucket's ids copied out in ascending id order — the one way a
    /// walker takes the postings without the pair's guard: the copy is
    /// made under the guard and the walk runs outside it, so no
    /// `Postings` clone and no guard across node reads. The order is the
    /// walk's contract (ascending ids within a key), not an artifact of
    /// the set's hash layout.
    pub(super) fn copy_ids(&self) -> Vec<NodeId> {
        match self {
            Postings::One(id) => vec![*id],
            Postings::Many(set) => {
                let mut ids: Vec<NodeId> = set.iter().copied().collect();
                ids.sort_unstable();
                ids
            }
        }
    }

    /// The one posting walk: each id in turn until `visit` breaks. A
    /// consumer that needs only some of the postings stops pulling and the
    /// rest are never read. A walker that must carry an answer out without
    /// holding the pair's guard copies the ids ([`Self::copy_ids`]) and
    /// walks the copy itself.
    pub(super) fn each(&self, mut visit: impl FnMut(NodeId) -> ControlFlow<()>) {
        match self {
            Postings::One(id) => {
                let _ = visit(*id);
            }
            Postings::Many(set) => {
                for id in set {
                    if visit(*id).is_break() {
                        break;
                    }
                }
            }
        }
    }

    fn extend_into(&self, out: &mut HashSet<NodeId>) {
        self.each(|id| {
            out.insert(id);
            ControlFlow::Continue(())
        });
    }
}

/// Keys one node was posted under. One key stays inline; a second version
/// of the property promotes the arm to a vec.
#[derive(Clone, Debug)]
pub(super) enum RevKeys<T> {
    One(T),
    Many(Vec<T>),
}

impl<T: PartialEq + Clone> RevKeys<T> {
    fn insert(&mut self, key: T) {
        match self {
            RevKeys::One(existing) if existing == &key => {}
            RevKeys::One(existing) => {
                let first = existing.clone();
                *self = RevKeys::Many(vec![first, key]);
            }
            RevKeys::Many(keys) => {
                if !keys.contains(&key) {
                    keys.push(key);
                }
            }
        }
    }

    fn into_vec(self) -> Vec<T> {
        match self {
            RevKeys::One(key) => vec![key],
            RevKeys::Many(keys) => keys,
        }
    }
}

/// Which end of an ordered index a read walks toward — the one
/// vocabulary for the extremum direction, from the aggregate's
/// `min`/`max` ask to the step's end choice; the bool it used to be is
/// gone, not derived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Extremum {
    Min,
    Max,
}

#[derive(Debug, Default)]
pub(crate) struct RangeIndex {
    pub(super) order: BTreeMap<OrderKey, Postings>,
    /// Order keys this node was posted under, so a drain removes it without
    /// scanning every key in the index.
    pub(super) by_node: HashMap<NodeId, RevKeys<OrderKey>>,
    /// Equality postings for values whose [`OrderKey`] cannot answer `=`
    /// (lists, maps, points, uuids, temporals outside the order window, and
    /// every zoned datetime — see [`order_key_finer_than_equality`]).
    eq_only: HashMap<GroupKey, Postings>,
    eq_by_node: HashMap<NodeId, RevKeys<GroupKey>>,
    /// Nodes whose temporal value does not fit an `OrderKey`. A comparison
    /// seek must scan while this is non-empty: serving the index would miss
    /// them.
    pub(super) unbounded: HashSet<NodeId>,
    /// Nodes whose value reaches neither `order` nor `eq_only` (a NaN, alone
    /// or inside a list or map): they are posted nowhere, so a walk of the
    /// postings does not see them. Comparison seeks are unaffected (a NaN
    /// satisfies no comparison); only the aggregate reads consult it.
    pub(super) unposted: UnpostedMark,
    pub(super) dirty: HashSet<NodeId>,
}

impl RangeIndex {
    /// Every posting sits in the order map: no equality-only side map, no
    /// unbounded temporals and no unposted NaN values, so each posted value's
    /// order key is its own `=` identity and a walk of the map sees every
    /// node holding a value.
    pub(super) fn order_is_complete(&self) -> bool {
        self.eq_only.is_empty() && self.unbounded.is_empty() && self.unposted.is_empty()
    }

    /// Whether the first and last keys can answer a min/max read: every
    /// posted value sorts by an order key that is its own `=` identity (no
    /// side-map or unbounded postings), and both ends are the same Int or
    /// String variant, so no other type's keys sit between them. An empty
    /// index qualifies (its answer is no value).
    pub(super) fn answers_extremum(&self) -> bool {
        let one_variant = match (self.order.first_key_value(), self.order.last_key_value()) {
            (Some((lo, _)), Some((hi, _))) => matches!(
                (lo, hi),
                (OrderKey::Int(_), OrderKey::Int(_)) | (OrderKey::String(_), OrderKey::String(_))
            ),
            // Both ends come from the one map, so an index has either
            // both or neither; the mixed arms are unreachable but named,
            // not silently permissive.
            (None, None) => true,
            (None, Some(_)) | (Some(_), None) => {
                debug_assert!(false, "one end of a one-map index cannot exist alone");
                false
            }
        };
        self.order_is_complete() && one_variant
    }

    /// The posting just past `from` toward `extremum`; the first (last)
    /// one when `from` is `None`. Borrowed: the caller walks the bucket
    /// under the same guard it stepped with — no bucket clone per step.
    pub(super) fn step_from(
        &self,
        from: Option<&OrderKey>,
        extremum: Extremum,
    ) -> Option<(&OrderKey, &Postings)> {
        let entry = match (from, extremum) {
            (None, Extremum::Max) => self.order.last_key_value(),
            (None, Extremum::Min) => self.order.first_key_value(),
            (Some(k), Extremum::Max) => self.order.range(..k.clone()).next_back(),
            (Some(k), Extremum::Min) => self
                .order
                .range((Bound::Excluded(k.clone()), Bound::Unbounded))
                .next(),
        };
        entry
    }
}

impl DrainState for RangeIndex {
    fn dirty_mut(&mut self) -> &mut HashSet<NodeId> {
        &mut self.dirty
    }
    fn insert(&mut self, id: NodeId, value: &Value) {
        insert_order(self, value, id);
    }
    fn remove_node(&mut self, id: NodeId) {
        order_remove_node(self, id);
    }
    fn reconcile_live(&mut self, id: NodeId, live: Option<&Value>) {
        // `unposted` is the aggregate reads' trust mark, and those run
        // in the exactness window — the latest committed state — so it
        // reflects the LIVE value alone: a superseded NaN must not
        // over-decline, a superseded posting must not under-decline, and
        // a node whose live version carries no value for the pair is
        // marked by nothing. `unbounded` is NOT reconciled: it is the
        // comparison seeks' any-snapshot gate (a seek whose statement
        // snapshot still sees a superseded out-of-window temporal must
        // scan, or it would miss a node the order map never holds), so
        // it keeps the per-version superset `insert_order` posts — and
        // the aggregate decline that stale mark costs is the safe
        // direction.
        let unpostable = live.is_some_and(|v| !Destinations::of(v).reaches_a_map());
        self.unposted.restate(id, unpostable);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum OrderKey {
    Bool(bool),
    Int(i64),
    Float(u64),
    String(Arc<str>),
    Date(i32),
    LocalTime(i64),
    Time(i64),
    LocalDateTime(i64),
    DateTime(i64),
    Duration(i64, i64, i128),
}

#[cfg(test)]
impl RevKeys<OrderKey> {
    pub(super) fn first(&self) -> Option<&OrderKey> {
        match self {
            RevKeys::One(key) => Some(key),
            RevKeys::Many(keys) => keys.first(),
        }
    }
}

/// Ordered comparison, equality, or `IN` against one property.
#[derive(Clone, Debug)]
pub(crate) enum RangeSeek {
    Cmp { op: BinaryOp, bound: Value },
    Eq(Value),
    In(Arc<Vec<Value>>),
}

/// Distance or bbox probe against one point property.
#[derive(Clone, Debug)]
pub(crate) enum SpatialSeek {
    Distance {
        origin: Value,
        max: f64,
        inclusive: bool,
    },
    BBox {
        lo: Value,
        hi: Value,
    },
}

pub(super) fn ord_f64(f: f64) -> u64 {
    let bits = f.to_bits();
    if bits & (1u64 << 63) == 0 {
        bits | (1u64 << 63)
    } else {
        !bits
    }
}

pub(super) fn time_nanos(t: NaiveTime) -> i64 {
    t.num_seconds_from_midnight() as i64 * 1_000_000_000 + t.nanosecond() as i64
}

pub(super) fn dt_nanos(dt: NaiveDateTime) -> Option<i64> {
    dt.and_utc().timestamp_nanos_opt()
}

/// A value's order key, or `None` when it has none — a NaN, an entity, and
/// a temporal outside the order window ([`Temporal::CivilDate`] /
/// [`Temporal::CivilDateTime`], whose calendar lies outside chrono's range).
/// A civil date could be keyed by its calendar parts, but that widens every
/// DATE key in the index for a value class no store can hold: the storage
/// codecs refuse to persist a civil date, so only an in-memory store has
/// one. Declining the key instead makes its presence mark the node
/// `unbounded`, and a comparison seek scans rather than answering the Date
/// variant alone.
pub(super) fn order_key(v: &Value) -> Option<OrderKey> {
    match v {
        Value::Bool(b) => Some(OrderKey::Bool(*b)),
        Value::Int(i) => Some(OrderKey::Int(*i)),
        // An exact integral float shares the integer key, so `1` and `1.0`
        // (and `0.0` / `-0.0`) are one point lookup. Non-integral floats
        // stay in the float variant.
        Value::Float(f) if !f.is_nan() => Some(match f64_exact_i64(*f) {
            Some(i) => OrderKey::Int(i),
            None => OrderKey::Float(ord_f64(*f)),
        }),
        Value::String(s) => Some(OrderKey::String(Arc::clone(s))),
        Value::Temporal(Temporal::Date(d)) => Some(OrderKey::Date(d.num_days_from_ce())),
        Value::Temporal(Temporal::LocalTime(t)) => Some(OrderKey::LocalTime(time_nanos(*t))),
        Value::Temporal(Temporal::Time { time, offset }) => Some(OrderKey::Time(
            time_nanos(*time) - i64::from(*offset) * 1_000_000_000,
        )),
        Value::Temporal(Temporal::LocalDateTime(dt)) => dt_nanos(*dt).map(OrderKey::LocalDateTime),
        Value::Temporal(Temporal::DateTime { utc, .. }) => dt_nanos(*utc).map(OrderKey::DateTime),
        Value::Temporal(Temporal::Duration {
            months,
            days,
            nanos,
        }) => Some(OrderKey::Duration(*months, *days, *nanos)),
        _ => None,
    }
}

// Payload-free lower-bound constructor per `OrderKey` variant: the single
// definition of each variant boundary sentinel. `variant_lower`,
// `variant_upper` and the range scans below all build on these, so no call
// site re-states a sentinel inline.
fn bool_lower() -> OrderKey {
    OrderKey::Bool(false)
}

fn int_lower() -> OrderKey {
    OrderKey::Int(i64::MIN)
}

fn float_lower() -> OrderKey {
    OrderKey::Float(ord_f64(f64::NEG_INFINITY))
}

fn string_lower() -> OrderKey {
    OrderKey::String(Arc::from(""))
}

fn date_lower() -> OrderKey {
    OrderKey::Date(i32::MIN)
}

fn local_time_lower() -> OrderKey {
    OrderKey::LocalTime(i64::MIN)
}

fn time_lower() -> OrderKey {
    OrderKey::Time(i64::MIN)
}

fn local_date_time_lower() -> OrderKey {
    OrderKey::LocalDateTime(i64::MIN)
}

fn date_time_lower() -> OrderKey {
    OrderKey::DateTime(i64::MIN)
}

fn duration_lower() -> OrderKey {
    OrderKey::Duration(i64::MIN, i64::MIN, i128::MIN)
}

/// `k`'s variant lower bound: the payload-free sentinel of its own variant.
pub(super) fn variant_lower(k: &OrderKey) -> OrderKey {
    match k {
        OrderKey::Bool(_) => bool_lower(),
        OrderKey::Int(_) => int_lower(),
        OrderKey::Float(_) => float_lower(),
        OrderKey::String(_) => string_lower(),
        OrderKey::Date(_) => date_lower(),
        OrderKey::LocalTime(_) => local_time_lower(),
        OrderKey::Time(_) => time_lower(),
        OrderKey::LocalDateTime(_) => local_date_time_lower(),
        OrderKey::DateTime(_) => date_time_lower(),
        OrderKey::Duration(_, _, _) => duration_lower(),
    }
}

/// The variant ladder, stated once: each variant's successor in the
/// cross-type order, or `None` past the last one (a duration). Every arm
/// returns the successor variant's **lower-bound sentinel** — the first key
/// past `k`'s variant. The `OrderKey` declaration order is the same
/// sequence (the `Ord` derive is the contract), and [`variant_upper`] reads
/// this ladder, so a variant inserted or reordered needs its arm here and
/// its sentinel in [`variant_lower`], never a hand-shifted second copy.
fn successor(k: &OrderKey) -> Option<OrderKey> {
    Some(match k {
        OrderKey::Bool(_) => int_lower(),
        OrderKey::Int(_) => float_lower(),
        OrderKey::Float(_) => string_lower(),
        OrderKey::String(_) => date_lower(),
        OrderKey::Date(_) => local_time_lower(),
        OrderKey::LocalTime(_) => time_lower(),
        OrderKey::Time(_) => local_date_time_lower(),
        OrderKey::LocalDateTime(_) => date_time_lower(),
        OrderKey::DateTime(_) => duration_lower(),
        OrderKey::Duration(_, _, _) => return None,
    })
}

/// The exclusive upper bound of `k`'s variant: the successor's lower bound,
/// or unbounded past the last variant.
pub(super) fn variant_upper(k: &OrderKey) -> Bound<OrderKey> {
    successor(k).map_or(Bound::Unbounded, Bound::Excluded)
}

pub(super) fn prefix_successor(prefix: &str) -> Option<String> {
    let mut chars: Vec<char> = prefix.chars().collect();
    while let Some(c) = chars.pop() {
        if let Some(next) = char::from_u32(u32::from(c).saturating_add(1)) {
            chars.push(next);
            return Some(chars.into_iter().collect());
        }
    }
    None
}

fn remember<T: PartialEq + Clone>(map: &mut HashMap<NodeId, RevKeys<T>>, id: NodeId, key: T) {
    match map.entry(id) {
        std::collections::hash_map::Entry::Occupied(mut entry) => entry.get_mut().insert(key),
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(RevKeys::One(key));
        }
    }
}

trait ForwardPostings {
    type Key;
    fn unpost(&mut self, key: &Self::Key, id: NodeId);
    /// Post `id` under `key` — the forward half of the shared [`post`],
    /// the fresh bucket seeded with the posting itself (no sentinel).
    fn post(&mut self, key: Self::Key, id: NodeId);
}

impl<K: Ord> ForwardPostings for BTreeMap<K, Postings> {
    type Key = K;
    fn unpost(&mut self, key: &K, id: NodeId) {
        let empty = self
            .get_mut(key)
            .is_some_and(|postings| postings.remove(id));
        if empty {
            self.remove(key);
        }
    }
    fn post(&mut self, key: K, id: NodeId) {
        self.entry(key).or_insert(Postings::One(id)).insert(id);
    }
}

impl<K: Eq + std::hash::Hash> ForwardPostings for HashMap<K, Postings> {
    type Key = K;
    fn unpost(&mut self, key: &K, id: NodeId) {
        let empty = self
            .get_mut(key)
            .is_some_and(|postings| postings.remove(id));
        if empty {
            self.remove(key);
        }
    }
    fn post(&mut self, key: K, id: NodeId) {
        self.entry(key).or_insert(Postings::One(id)).insert(id);
    }
}

fn forget<K, M>(reverse: &mut HashMap<NodeId, RevKeys<K>>, forward: &mut M, id: NodeId)
where
    K: PartialEq + Clone,
    M: ForwardPostings<Key = K>,
{
    if let Some(keys) = reverse.remove(&id) {
        for key in keys.into_vec() {
            forward.unpost(&key, id);
        }
    }
}

/// [`forget`]'s posting counterpart — the one forward-and-reverse pair
/// write every [`insert_order`] arm takes: remember the key on the node's
/// reverse side, then post the id under it forward.
fn post<K, M>(reverse: &mut HashMap<NodeId, RevKeys<K>>, forward: &mut M, id: NodeId, key: K)
where
    K: PartialEq + Clone,
    M: ForwardPostings<Key = K>,
{
    remember(reverse, id, key.clone());
    forward.post(key, id);
}

pub(super) fn insert_order(st: &mut RangeIndex, value: &Value, id: NodeId) {
    // The one posting-destination classification, consulted by the
    // posting arms and the live reconciliation alike. Two contracts
    // follow: the forward maps and `unbounded` are the per-version
    // superset (ADR-0018; `unbounded` gates comparison seeks at ANY
    // statement snapshot, so a superseded out-of-window temporal must
    // keep its mark), while `unposted` — the aggregate reads'
    // exactness-window mark — is written only by the drain's
    // `reconcile_live`, from the live value.
    let d = Destinations::of(value);
    if let Some(k) = d.order {
        post(&mut st.by_node, &mut st.order, id, k);
    }
    if d.unbounded {
        st.unbounded.insert(id);
    }
    if let Some(gk) = d.eq {
        post(&mut st.eq_by_node, &mut st.eq_only, id, gk);
    }
}

/// Where one value lands in the index's maps — the one classification,
/// computed once per value and consumed by [`insert_order`]'s posting
/// arms and [`RangeIndex::reconcile_live`]'s reach question alike. The
/// destinations are independent, not exclusive: a zoned DateTime posts
/// `order` AND `eq_only`, an out-of-window temporal posts `unbounded`
/// AND `eq_only`. A NaN (alone or inside a list or map) is the one value
/// with no destination at all.
struct Destinations {
    /// Its own order key, when the value sorts.
    order: Option<OrderKey>,
    /// A temporal outside the order window.
    unbounded: bool,
    /// Its index identity, when `=` needs the side map.
    eq: Option<GroupKey>,
}

impl Destinations {
    /// The admission is decided here, in the direction the predicate
    /// reads: the `eq_only` copy is admitted when there is no order key
    /// or the key is finer than the `=` identity
    /// ([`order_key_finer_than_equality`] — a zoned `DateTime` orders by
    /// instant, so its key cannot answer `=` alone). Each field is then a
    /// real destination and `reaches_a_map` stays exact.
    fn of(value: &Value) -> Self {
        let order = order_key(value);
        Destinations {
            eq: if order.is_none() || order_key_finer_than_equality(value) {
                GroupKey::for_property_index(value)
            } else {
                None
            },
            // The order key is derived once: an out-of-window temporal is
            // one that has no key, read off the `order` already bound.
            unbounded: matches!(value, Value::Temporal(_)) && order.is_none(),
            order,
        }
    }

    /// Whether the value reaches any of the index's maps.
    fn reaches_a_map(&self) -> bool {
        self.order.is_some() || self.unbounded || self.eq.is_some()
    }
}

pub(super) fn order_remove_node(st: &mut RangeIndex, id: NodeId) {
    st.unbounded.remove(&id);
    st.unposted.clear(id);
    forget(&mut st.by_node, &mut st.order, id);
    forget(&mut st.eq_by_node, &mut st.eq_only, id);
}

/// Ids equal to `value` under the range index: one order-key probe for
/// values that sort, otherwise the side map. Exact integral floats share
/// the integer key, so this is a point lookup for `1` and `1.0`.
pub(super) fn equality_ids(st: &RangeIndex, value: &Value) -> HashSet<NodeId> {
    let mut out = HashSet::new();
    equality_each(st, value, |id| {
        out.insert(id);
        ControlFlow::Continue(())
    });
    out
}

/// The equality postings of `value` as a stream: `visit` is called per id
/// until it breaks, so a consumer that needs only the first few (the capped
/// funnel) stops without the id set ever being built. [`equality_ids`] is
/// this stream collected — the one equality read.
pub(super) fn equality_each(
    st: &RangeIndex,
    value: &Value,
    visit: impl FnMut(NodeId) -> ControlFlow<()>,
) {
    if let Some(postings) = equality_postings(st, value) {
        postings.each(visit);
    }
}

/// Posting length for one equality value. Zero when the value is not posted.
/// This does not allocate the id set.
pub(super) fn equality_count(st: &RangeIndex, value: &Value) -> usize {
    equality_postings(st, value).map(Postings::len).unwrap_or(0)
}

/// Whether an equality probe cannot read the `order` map for this value. A
/// zoned `DateTime` orders by its UTC instant while its index identity also
/// carries the zone ([`GroupKey::for_property_index`]): an instant-keyed
/// bucket is coarser than that identity (two zones can share an instant), so
/// it cannot answer an indexed seek or a uniqueness probe, and the value
/// gets an `eq_only` entry keyed by the identity instead.
fn order_key_finer_than_equality(value: &Value) -> bool {
    matches!(value, Value::Temporal(Temporal::DateTime { .. }))
}

fn equality_postings<'a>(st: &'a RangeIndex, value: &Value) -> Option<&'a Postings> {
    if !order_key_finer_than_equality(value) {
        if let Some(k) = order_key(value) {
            return st.order.get(&k);
        }
    }
    st.eq_only.get(&GroupKey::for_property_index(value)?)
}

pub(super) fn collect_range(
    order: &BTreeMap<OrderKey, Postings>,
    start: Bound<OrderKey>,
    end: Bound<OrderKey>,
    out: &mut HashSet<NodeId>,
) {
    scan::visit_range(order, start, end, |ids| ids.extend_into(out));
}

/// Superset of values on the far side of a bound, covering both ints and
/// floats and, for a zoned temporal, its key-tie group. Residual comparison
/// drops the extra ids.
pub(super) fn range_ids(
    order: &BTreeMap<OrderKey, Postings>,
    op: BinaryOp,
    bound: &Value,
) -> HashSet<NodeId> {
    let mut out = HashSet::new();
    scan::visit_range_postings(order, op, bound, |ids| ids.extend_into(&mut out));
    out
}

/// Upper bound on [`range_ids`], summed from posting lengths. A node posted
/// under more than one visited key is counted once per key. No id set is built.
pub(super) fn range_count(
    order: &BTreeMap<OrderKey, Postings>,
    op: BinaryOp,
    bound: &Value,
) -> usize {
    let mut count = 0usize;
    scan::visit_range_postings(order, op, bound, |ids| {
        count = count.saturating_add(ids.len());
    });
    count
}

pub(super) fn prefix_ids(order: &BTreeMap<OrderKey, Postings>, prefix: &str) -> HashSet<NodeId> {
    let mut out = HashSet::new();
    let start = OrderKey::String(Arc::from(prefix));
    let end = match prefix_successor(prefix) {
        Some(s) => Bound::Excluded(OrderKey::String(Arc::from(s))),
        // No successor prefix: scan to the end of the String variant.
        None => variant_upper(&start),
    };
    collect_range(order, Bound::Included(start), end, &mut out);
    out
}

pub(super) fn cmp_holds(v: &Value, op: BinaryOp, bound: &Value) -> bool {
    // The predicate side comes from the ordering-operator table the expression
    // evaluator uses, and the value side from its `cmp_bool`, so an indexed
    // range predicate can never answer differently from a residual one
    // (`duration(...) <= duration(...)` holds without being orderable).
    let Some(ordering) = eval::ordering_operator(op) else {
        return false;
    };
    matches!(
        eval::cmp_bool(v, bound, ordering.equality_allowed, ordering.pred),
        Value::Bool(true)
    )
}

#[cfg(test)]
mod tests;

mod scan;
