//! Compact live records and version-chain structures (ADR-0033); the typed
//! adjacency chunks live in [`crate::adjacency`].
//! Public `Node` / `Relationship` maps are materialized at the API boundary.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, OnceLock};

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
/// The empty property map every node and relationship stub shares.
pub(crate) fn empty_properties() -> Arc<relate_core::PropertyMap> {
    static EMPTY: OnceLock<Arc<relate_core::PropertyMap>> = OnceLock::new();
    Arc::clone(EMPTY.get_or_init(|| Arc::new(relate_core::PropertyMap::new())))
}
mod compact_props;
mod interner;
mod payload;
mod rel_types;

#[cfg(test)]
pub(crate) use self::interner::take_get_id_calls;
pub(crate) use self::interner::Interner;

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
        interner.label_strings(&self.labels)
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
