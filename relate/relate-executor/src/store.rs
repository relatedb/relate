//! Compact live records and version-chain structures (ADR-0033); the typed
//! adjacency chunks live in [`crate::adjacency`].
//! Public `Node` / `Relationship` maps are materialized at the API boundary.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};

use dashmap::DashMap;
use relate_core::{Node, NodeId, PropertyMap, RelId, Relationship, Result, Value};
use relate_ir::Direction;

/// Other endpoint of `rid` given `current` and direction.
pub(crate) fn hop_other(
    current: NodeId,
    start: NodeId,
    end: NodeId,
    dir: Direction,
) -> Option<NodeId> {
    match dir {
        Direction::Outgoing => (start == current).then_some(end),
        Direction::Incoming => (end == current).then_some(start),
        Direction::Both => {
            if start == current {
                Some(end)
            } else if end == current {
                Some(start)
            } else {
                None
            }
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct Interner {
    to_id: DashMap<Arc<str>, u32>,
    /// Dense, append-only id→string table. Entries are never removed or
    /// reassigned, so a read for a committed id is a plain shard lookup; only
    /// `intern` mutates (on first sight of a string).
    to_str: DashMap<u32, Arc<str>>,
    /// Monotonic id source. Separate from `to_str.len()` so two concurrent
    /// first-time interns of distinct strings cannot observe the same len.
    next_id: AtomicU32,
    /// Bytes of distinct strings. `Arc` clones are not added again.
    bytes: std::sync::atomic::AtomicU64,
    /// One label vector per distinct label-id list. Walk stubs clone the
    /// `Arc` instead of rebuilding the strings.
    label_vecs: DashMap<Vec<u32>, Arc<Vec<String>>>,
}

/// One restore slice's names: one interner lookup per distinct name
/// instead of one per row, so concurrent load workers do not convoy on the
/// interner's shard locks. Interned ids never change, so a slice-lived
/// cache cannot go stale.
#[derive(Default)]
pub(crate) struct NameCache {
    names: HashMap<Box<str>, (u32, Arc<str>)>,
}

impl NameCache {
    /// `name`'s id and shared spelling, interning it on first sight.
    pub(crate) fn shared(&mut self, interner: &Interner, name: &str) -> (u32, Arc<str>) {
        if let Some((id, spelling)) = self.names.get(name) {
            return (*id, Arc::clone(spelling));
        }
        let id = interner.intern(name);
        let spelling = interner.get(id);
        self.names.insert(name.into(), (id, Arc::clone(&spelling)));
        (id, spelling)
    }

    /// `name`'s id, interning it on first sight.
    pub(crate) fn id(&mut self, interner: &Interner, name: &str) -> u32 {
        if let Some((id, _)) = self.names.get(name) {
            return *id;
        }
        self.shared(interner, name).0
    }
}

#[cfg(test)]
thread_local! {
    /// `Interner::get_id` calls on this thread — test-only, read through
    /// [`take_get_id_calls`].
    static GET_ID_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Returns and resets this thread's `Interner::get_id` call count.
#[cfg(test)]
pub(crate) fn take_get_id_calls() -> u64 {
    GET_ID_CALLS.with(|c| c.replace(0))
}

impl relate_names::TokenResolver for Interner {
    fn resolve(&self, name: &str) -> relate_names::Token {
        match self.get_id(name) {
            Some(id) => relate_names::Token::Id(relate_names::TokenId(id)),
            None => relate_names::Token::Absent,
        }
    }
}

/// The empty property map every node and relationship stub shares.
pub(crate) fn empty_properties() -> Arc<relate_core::PropertyMap> {
    static EMPTY: OnceLock<Arc<relate_core::PropertyMap>> = OnceLock::new();
    Arc::clone(EMPTY.get_or_init(|| Arc::new(relate_core::PropertyMap::new())))
}

impl Interner {
    pub(crate) fn intern(&self, s: &str) -> u32 {
        if let Some(id) = self.to_id.get(s) {
            return *id;
        }
        // `entry` holds the shard write lock, so a concurrent `intern` of the
        // same string takes the occupied arm and both callers get one id.
        match self.to_id.entry(Arc::from(s)) {
            dashmap::mapref::entry::Entry::Occupied(e) => *e.get(),
            dashmap::mapref::entry::Entry::Vacant(e) => {
                let arc = e.key().clone();
                let id = self.next_id.fetch_add(1, Ordering::Relaxed);
                // Publish id→str before the id is reachable via `to_id`, so a
                // `get(id)` that observes the id always finds the string.
                self.to_str.insert(id, arc);
                e.insert(id);
                self.bytes.fetch_add(
                    crate::memory::interned_string_bytes(s.len()),
                    Ordering::Relaxed,
                );
                id
            }
        }
    }

    pub(crate) fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }

    /// Distinct interned strings — the census harness's key count.
    #[cfg(feature = "research")]
    pub(crate) fn key_count(&self) -> usize {
        self.to_id.len()
    }

    pub(crate) fn get_id(&self, s: &str) -> Option<u32> {
        #[cfg(test)]
        GET_ID_CALLS.with(|c| c.set(c.get() + 1));
        self.to_id.get(s).map(|v| *v)
    }

    /// Reverse lookup of an interned id. Every reachable id was published
    /// by [`Interner::intern`] before use, so a miss is a bug, not data:
    /// panic instead of silently returning the empty string.
    pub(crate) fn get(&self, id: u32) -> Arc<str> {
        self.to_str
            .get(&id)
            .map(|v| Arc::clone(&*v))
            .expect("interned id is published before use")
    }

    /// The label strings for `ids`, shared with every other stub of the
    /// same label list.
    pub(crate) fn cached_labels(&self, ids: &[u32]) -> Arc<Vec<String>> {
        if let Some(hit) = self.label_vecs.get(ids) {
            return Arc::clone(hit.value());
        }
        let labels = Arc::new(
            ids.iter()
                .map(|id| String::from(&*self.get(*id)))
                .collect::<Vec<_>>(),
        );
        match self.label_vecs.entry(ids.to_vec()) {
            dashmap::mapref::entry::Entry::Occupied(entry) => Arc::clone(entry.get()),
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                let string_bytes = labels.iter().map(|label| label.len() as u64).sum();
                self.bytes.fetch_add(
                    crate::memory::label_vec_cache_bytes(ids.len(), string_bytes),
                    Ordering::Relaxed,
                );
                entry.insert(Arc::clone(&labels));
                labels
            }
        }
    }
}

mod compact_props;
mod payload;
mod rel_types;

pub(crate) use self::payload::{PayloadForm, PayloadKeyIds, RelPayload};
pub(crate) use self::rel_types::RelTypes;

use self::compact_props::add_bytes;
pub(crate) use self::compact_props::CompactProps;

#[derive(Clone, Debug, Default)]
pub(crate) struct LiveNode {
    pub labels: Vec<u32>,
    pub props: CompactProps,
}

impl LiveNode {
    /// The one constructor both spellings feed: the interned label ids and
    /// the compact property record, with the label buffer fitted.
    fn with(mut labels: Vec<u32>, props: CompactProps) -> Self {
        crate::memory::fit_reserved(&mut labels);
        Self { labels, props }
    }

    pub(crate) fn from_parts(
        interner: &Interner,
        labels: Vec<String>,
        properties: BTreeMap<String, Value>,
    ) -> Self {
        let label_ids = labels.iter().map(|l| interner.intern(l)).collect();
        Self::with(label_ids, CompactProps::from_map(interner, properties))
    }

    /// [`Self::from_parts`] over a restore slice's [`NameCache`] and a row
    /// read in place: the labels' interner lookups collapse to one per
    /// distinct label, and the row's shared properties are read, not
    /// cloned out of their map.
    pub(crate) fn from_parts_cached(
        interner: &Interner,
        names: &mut NameCache,
        labels: &[String],
        properties: &PropertyMap,
    ) -> Self {
        let label_ids = labels.iter().map(|l| names.id(interner, l)).collect();
        Self::with(
            label_ids,
            CompactProps::from_map_ref(interner, names, properties),
        )
    }

    pub(crate) fn to_node(&self, id: NodeId, interner: &Interner) -> Node {
        Node {
            id,
            labels: Arc::new(self.label_strings(interner)),
            properties: relate_core::PropertyMap::share(self.props.to_map(interner)),
        }
    }

    pub(crate) fn has_label(&self, label: u32) -> bool {
        self.labels.contains(&label)
    }

    /// Every id in `lids` present — the containment half of
    /// `Graph::node_matches` at the id level, so a raw-version re-check
    /// (the index seek's `finish_ids`) rides the same predicate the
    /// materialized check spells, never a second `iter().all` copy.
    pub(crate) fn has_labels(&self, lids: &[u32]) -> bool {
        lids.iter().all(|lid| self.has_label(*lid))
    }

    pub(crate) fn label_strings(&self, interner: &Interner) -> Vec<String> {
        self.labels
            .iter()
            .map(|l| String::from(&*interner.get(*l)))
            .collect()
    }

    pub(crate) fn prop(&self, interner: &Interner, key: &str) -> Option<Value> {
        let kid = interner.get_id(key)?;
        self.prop_by_id(kid)
    }

    /// The property under interned key id `kid` — [`Self::prop`] with the
    /// interning already paid (a plan-resolved token).
    pub(crate) fn prop_by_id(&self, kid: u32) -> Option<Value> {
        self.props.get(kid).cloned()
    }

    /// Label bytes plus tagged property JSON. The Parquet codec adds the
    /// fixed width of its Int64 columns.
    pub(crate) fn checkpoint_bytes(&self, interner: &Interner) -> Result<u64> {
        let mut bytes = 0u64;
        for label in &self.labels {
            bytes = add_bytes(bytes, interner.get(*label).len() as u64)?;
        }
        add_bytes(bytes, self.props.tagged_json_len(interner)?)
    }

    pub(crate) fn stub(&self, id: NodeId, interner: &Interner) -> Node {
        Node {
            id,
            labels: interner.cached_labels(&self.labels),
            properties: empty_properties(),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct LiveRel {
    pub start: NodeId,
    pub end: NodeId,
    pub type_id: u32,
    type_name: Arc<str>,
    pub payload: RelPayload,
}

impl LiveRel {
    /// The one constructor both spellings feed: the interned type id and
    /// its shared spelling beside the payload.
    fn with(
        start: NodeId,
        end: NodeId,
        type_id: u32,
        type_name: Arc<str>,
        payload: RelPayload,
    ) -> Self {
        Self {
            start,
            end,
            type_id,
            type_name,
            payload,
        }
    }

    pub(crate) fn from_parts(
        interner: &Interner,
        start: NodeId,
        end: NodeId,
        rel_type: &str,
        properties: BTreeMap<String, Value>,
    ) -> Self {
        let type_id = interner.intern(rel_type);
        Self::with(
            start,
            end,
            type_id,
            interner.get(type_id),
            RelPayload::from_map(interner, properties),
        )
    }

    /// [`Self::from_parts`] over a restore slice's [`NameCache`] and a row
    /// read in place: the type's interner lookups collapse to one per
    /// distinct type, and the row's shared properties are read, not cloned
    /// out of their map.
    pub(crate) fn from_parts_cached(
        interner: &Interner,
        names: &mut NameCache,
        start: NodeId,
        end: NodeId,
        rel_type: &str,
        properties: &PropertyMap,
    ) -> Self {
        let (type_id, type_name) = names.shared(interner, rel_type);
        Self::with(
            start,
            end,
            type_id,
            type_name,
            RelPayload::from_map_ref(interner, names, properties),
        )
    }

    pub(crate) fn to_rel(&self, id: RelId, interner: &Interner) -> Relationship {
        Relationship {
            id,
            start: self.start,
            end: self.end,
            rel_type: String::from(&*self.type_name),
            properties: relate_core::PropertyMap::share(self.payload.to_map(interner)),
        }
    }

    /// The type's interned name, shared — one refcount bump (the
    /// overlay's shared type read's base half).
    pub(crate) fn type_name_shared(&self) -> Arc<str> {
        Arc::clone(&self.type_name)
    }

    pub(crate) fn stub(&self, id: RelId) -> Relationship {
        Relationship {
            id,
            start: self.start,
            end: self.end,
            rel_type: String::from(&*self.type_name),
            properties: empty_properties(),
        }
    }

    /// Type-name bytes plus tagged property JSON. The Parquet codec adds the
    /// fixed width of its Int64 columns.
    pub(crate) fn checkpoint_bytes(&self, interner: &Interner) -> Result<u64> {
        add_bytes(
            self.type_name.len() as u64,
            self.payload.tagged_json_len(interner)?,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_roundtrip() {
        let i = Interner::default();
        let a = i.intern("TRANSFER");
        let b = i.intern("TRANSFER");
        assert_eq!(a, b);
        assert_eq!(&*i.get(a), "TRANSFER");
        assert_eq!(i.get_id("OTHER"), None);
    }

    #[test]
    fn resolver_matches_interner_ids() {
        use relate_names::{Token, TokenId, TokenResolver};
        let i = Interner::default();
        // label, relationship type, property key share the interner's id space
        for name in ["Person", "KNOWS", "since"] {
            let id = i.intern(name);
            assert_eq!(i.resolve(name), Token::Id(TokenId(id)));
        }
        for name in ["Absent", "MISSING", "nokey"] {
            assert_eq!(i.resolve(name), Token::Absent);
        }
    }

    #[test]
    #[should_panic(expected = "interned id is published before use")]
    fn get_unknown_id_panics() {
        let i = Interner::default();
        let _ = i.get(0);
    }

    /// The id table is append-only and concurrent `intern` calls race to mint
    /// ids. Every distinct string must get exactly one id, repeated interns of
    /// the same string must agree, and no id may ever resolve to the wrong
    /// string (which the old `len()`-based counter could cause).
    #[test]
    fn intern_concurrent_ids_are_stable_and_unique() {
        use std::collections::{HashMap, HashSet};
        use std::sync::Arc as StdArc;

        let interner = StdArc::new(Interner::default());
        let words: Vec<String> = (0..64).map(|n| format!("label-{n}")).collect();

        let handles: Vec<_> = (0..8)
            .map(|t| {
                let interner = StdArc::clone(&interner);
                let words = words.clone();
                std::thread::spawn(move || {
                    let mut seen = HashMap::new();
                    for round in 0..32 {
                        for n in 0..words.len() {
                            // Rotate start so threads interleave differently.
                            let w = &words[(n + t + round) % words.len()];
                            let id = interner.intern(w);
                            // Idempotent: the same string always yields the
                            // same id, even under concurrent first-sight.
                            if let Some(prev) = seen.insert(w.clone(), id) {
                                assert_eq!(prev, id, "id changed for {w}");
                            }
                            // The reverse direction must agree immediately.
                            assert_eq!(&*interner.get(id), w.as_str());
                        }
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        // Exactly one id per distinct string, and the id space is dense from 0.
        let ids: HashSet<u32> = words.iter().map(|w| interner.intern(w)).collect();
        assert_eq!(ids.len(), words.len(), "duplicate ids for distinct strings");
        assert_eq!(*ids.iter().min().unwrap(), 0);
        assert_eq!(*ids.iter().max().unwrap(), (words.len() - 1) as u32);
    }

    #[test]
    fn triple_payload_preserves_int_amount() {
        let i = Interner::default();
        let mut m = BTreeMap::new();
        m.insert("id".into(), Value::String("tx-1".into()));
        m.insert("amount".into(), Value::Int(10));
        m.insert("ts".into(), Value::Int(99));
        let p = RelPayload::from_map(&i, m);
        assert!(matches!(
            p,
            RelPayload::Triple {
                amount: Value::Int(10),
                ts: 99,
                ..
            }
        ));
        let out = p.to_map(&i);
        assert_eq!(out.get("amount"), Some(&Value::Int(10)));
    }

    #[test]
    fn triple_promotes_on_extra_key() {
        let i = Interner::default();
        let mut m = BTreeMap::new();
        m.insert("id".into(), Value::String("tx-1".into()));
        m.insert("amount".into(), Value::Float(1.5));
        m.insert("ts".into(), Value::Int(1));
        let mut p = RelPayload::from_map(&i, m);
        p.set(&i, "note", Value::String("x".into()));
        assert!(matches!(p, RelPayload::Other(_)));
        assert_eq!(p.to_map(&i).get("note"), Some(&Value::String("x".into())));
    }
}
