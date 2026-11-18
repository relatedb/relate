//! The relationship payload — the `Triple` fast shape or a property map
//! (ADR-0033) — with its key-id view, its shape test and its tagged-JSON byte
//! counter, split out of `store.rs` the way `compact_props` split before it.
//! The parent keeps the interner, the name cache and the live records.

use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::sync::Arc;

use relate_core::{PropertyMap, Result, Value};

use super::compact_props::json_len;
use super::{CompactProps, Interner, NameCache};

/// A payload's property key ids with its own interner reads already done:
/// `Triple` carries its three fixed names resolved once, `Other` carries its
/// stored ids (which need no interner). A caller that probes another lock
/// against these ids — the ADR-0439 constraint map's scope, held under its
/// lock — builds this before taking that lock, so the probe never takes an
/// interner shard guard.
pub(crate) enum PayloadKeyIds<'a> {
    Triple([Option<u32>; 3]),
    Other(&'a CompactProps),
}

impl PayloadKeyIds<'_> {
    /// Visit every property key id the payload carries, in payload order.
    /// The one `Triple`/`Other` dispatch — [`any`](Self::any) derives from
    /// it instead of repeating the match.
    pub(crate) fn for_each(&self, mut f: impl FnMut(u32) -> ControlFlow<()>) -> ControlFlow<()> {
        match self {
            Self::Triple(ids) => {
                for id in ids.iter().flatten() {
                    f(*id)?;
                }
            }
            Self::Other(props) => {
                for (kid, _) in props.iter() {
                    f(kid)?;
                }
            }
        }
        ControlFlow::Continue(())
    }

    /// Whether any of the payload's key ids satisfies `f`. Built on
    /// [`Self::for_each`] so the Triple/Other walk stays single-sourced;
    /// `ControlFlow` keeps the first-match short-circuit the create-seam hot
    /// path wants.
    pub(crate) fn any(&self, mut f: impl FnMut(u32) -> bool) -> bool {
        self.for_each(|kid| {
            if f(kid) {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        })
        .is_break()
    }
}

/// One relationship payload: the `Triple` fast shape or a property map. `P`
/// is the props representation — [`CompactProps`] for the live record,
/// `BTreeMap<String, Value>` for the interner-free redo estimate — so the
/// shape choice and every method over it are implemented once.
///
/// `from_map` stores a `Triple` only when the map is created in this shape.
/// `Other` never turns back into a `Triple`, so a later `SET` that happens
/// to produce `{id, amount, ts}` still occupies property vectors.
/// Accounting has to follow this form, not re-read the map.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Payload<P> {
    Triple {
        id: Arc<str>,
        amount: Value,
        ts: i64,
    },
    Other(P),
}

/// The props representation a [`Payload`] is generic over.
pub(crate) trait PayloadProps: Sized {
    fn from_map(interner: &Interner, map: BTreeMap<String, Value>) -> Self;
    fn set(&mut self, interner: &Interner, key: &str, value: Value);
    fn remove(&mut self, interner: &Interner, key: &str);
    fn get(&self, interner: &Interner, key: &str) -> Option<Value>;
    fn to_map(&self, interner: &Interner) -> BTreeMap<String, Value>;
}

impl PayloadProps for BTreeMap<String, Value> {
    fn from_map(_interner: &Interner, map: BTreeMap<String, Value>) -> Self {
        map
    }

    fn set(&mut self, _interner: &Interner, key: &str, value: Value) {
        self.insert(key.to_string(), value);
    }

    fn remove(&mut self, _interner: &Interner, key: &str) {
        BTreeMap::remove(self, key);
    }

    fn get(&self, _interner: &Interner, key: &str) -> Option<Value> {
        BTreeMap::get(self, key).cloned()
    }

    fn to_map(&self, _interner: &Interner) -> BTreeMap<String, Value> {
        self.clone()
    }
}

impl PayloadProps for CompactProps {
    fn from_map(interner: &Interner, map: BTreeMap<String, Value>) -> Self {
        CompactProps::from_map(interner, map)
    }

    fn set(&mut self, interner: &Interner, key: &str, value: Value) {
        self.set(interner.intern(key), value);
    }

    fn remove(&mut self, interner: &Interner, key: &str) {
        // Read-only lookup: a miss means nothing to remove, never a new
        // interned id.
        if let Some(kid) = interner.get_id(key) {
            self.remove(kid);
        }
    }

    fn get(&self, interner: &Interner, key: &str) -> Option<Value> {
        self.get(interner.get_id(key)?).cloned()
    }

    fn to_map(&self, interner: &Interner) -> BTreeMap<String, Value> {
        CompactProps::to_map(self, interner)
    }
}

/// The live counter's payload shape.
pub(crate) type RelPayload = Payload<CompactProps>;
/// The redo estimate's payload shape (no interned ids).
pub(crate) type PayloadForm = Payload<BTreeMap<String, Value>>;

/// An in-place triple write that fits the triple's shape; [`triple_write`]
/// answers `None` for every other pair, which the caller promotes.
enum TripleWrite<'a> {
    /// Set the `id` field to this string.
    Id(&'a str),
    /// Set the `amount` field to this value.
    Amount(&'a Value),
    /// Set the `ts` field to this integer.
    Ts(i64),
}

impl TripleWrite<'_> {
    /// Apply the write to its field. The one field-update spelling both
    /// payload shapes share.
    fn apply_to(self, id: &mut Arc<str>, amount: &mut Value, ts: &mut i64) {
        match self {
            TripleWrite::Id(s) => *id = Arc::from(s),
            TripleWrite::Amount(v) => *amount = v.clone(),
            TripleWrite::Ts(i) => *ts = i,
        }
    }
}

/// The one in-place-vs-promote decision both payload shapes consult: a write
/// updates a triple field only when it names that field with its own type;
/// every other write — and every removal — promotes (`None`).
fn triple_write<'a>(key: &str, value: &'a Value) -> Option<TripleWrite<'a>> {
    Some(match (TripleField::of(key), value) {
        (Some(TripleField::Id), Value::String(s)) => TripleWrite::Id(s),
        (Some(TripleField::Amount), Value::Int(_) | Value::Float(_)) => TripleWrite::Amount(value),
        (Some(TripleField::Ts), Value::Int(i)) => TripleWrite::Ts(*i),
        _ => return None,
    })
}

/// The `Triple` payload's three fields and the property name each is stored
/// under — the one classification `triple_shape`, `triple_prop` and
/// `triple_write` consult, and the source of [`TRIPLE_KEYS`].
#[derive(Clone, Copy)]
enum TripleField {
    Amount,
    Id,
    Ts,
}

impl TripleField {
    /// Every field, for the name lookup; [`TRIPLE_KEYS`] fixes their order.
    const ALL: [TripleField; 3] = [TripleField::Amount, TripleField::Id, TripleField::Ts];

    /// The property name this field is stored under.
    const fn key(self) -> &'static str {
        match self {
            TripleField::Amount => "amount",
            TripleField::Id => "id",
            TripleField::Ts => "ts",
        }
    }

    /// The field `key` names.
    fn of(key: &str) -> Option<TripleField> {
        TripleField::ALL
            .into_iter()
            .find(|field| field.key() == key)
    }
}

/// The `Triple` payload's three fixed property names, in field order — the
/// one spelling `keys`, `key_ids`, `for_each_key_value`, the promotes,
/// `to_map`, and the borrowed serializer all consult.
pub(crate) const TRIPLE_KEYS: [&str; 3] = [
    TripleField::Amount.key(),
    TripleField::Id.key(),
    TripleField::Ts.key(),
];

/// The three fixed properties as a map — the one conversion `to_map` and
/// both promotes share.
fn triple_map(id: Arc<str>, amount: Value, ts: i64) -> BTreeMap<String, Value> {
    let mut map = BTreeMap::new();
    map.insert(TRIPLE_KEYS[0].to_string(), amount);
    map.insert(TRIPLE_KEYS[1].to_string(), Value::String(id));
    map.insert(TRIPLE_KEYS[2].to_string(), Value::Int(ts));
    map
}

/// The triple's three typed fields, taken by [`triple_shape`] so the callers
/// store them without re-reading the source map.
struct TripleParts {
    id: Arc<str>,
    amount: Value,
    ts: i64,
}

/// The `Triple` payload's shape: exactly the three reserved properties, a
/// string id, a numeric amount and an int ts. Returns the typed parts, so
/// both map spellings — the owned map and the shared map read in place —
/// consume them instead of re-getting each field after the test.
fn triple_shape<'a>(
    len: usize,
    mut get: impl FnMut(&str) -> Option<&'a Value>,
) -> Option<TripleParts> {
    if len != 3 {
        return None;
    }
    let id = match get(TripleField::Id.key())? {
        Value::String(s) => Arc::clone(s),
        _ => return None,
    };
    let amount = get(TripleField::Amount.key())?;
    if !matches!(amount, Value::Int(_) | Value::Float(_)) {
        return None;
    }
    let amount = amount.clone();
    let ts = match get(TripleField::Ts.key())? {
        Value::Int(i) => *i,
        _ => return None,
    };
    Some(TripleParts { id, amount, ts })
}

impl<P: PayloadProps> Payload<P> {
    pub(crate) fn from_map(interner: &Interner, map: BTreeMap<String, Value>) -> Self {
        match triple_shape(map.len(), |key| map.get(key)) {
            Some(parts) => Payload::Triple {
                id: parts.id,
                amount: parts.amount,
                ts: parts.ts,
            },
            None => Payload::Other(P::from_map(interner, map)),
        }
    }

    pub(crate) fn set(&mut self, interner: &Interner, key: &str, value: Value) {
        if value.is_null() {
            self.remove(interner, key);
            return;
        }
        if let Payload::Triple { id, amount, ts } = self {
            if let Some(write) = triple_write(key, &value) {
                return write.apply_to(id, amount, ts);
            }
            // A key the triple cannot hold promotes it; the promoted shape
            // is the one the write lands on, as `remove` spells it.
            self.promote(interner);
        }
        if let Payload::Other(props) = self {
            props.set(interner, key, value);
        }
    }

    /// Removal always promotes (the [`triple_write`] policy's non-fitting
    /// case).
    pub(crate) fn remove(&mut self, interner: &Interner, key: &str) {
        if matches!(self, Payload::Triple { .. }) {
            self.promote(interner);
        }
        if let Payload::Other(props) = self {
            props.remove(interner, key);
        }
    }

    fn promote(&mut self, interner: &Interner) {
        let Payload::Triple { id, amount, ts } = self else {
            return;
        };
        *self = Payload::Other(P::from_map(
            interner,
            triple_map(
                std::mem::take(id),
                std::mem::replace(amount, Value::Null),
                *ts,
            ),
        ));
    }

    pub(crate) fn to_map(&self, interner: &Interner) -> BTreeMap<String, Value> {
        match self {
            Payload::Triple { id, amount, ts } => triple_map(id.clone(), amount.clone(), *ts),
            Payload::Other(props) => props.to_map(interner),
        }
    }

    pub(crate) fn ts(&self, interner: &Interner) -> Option<i64> {
        match self {
            Payload::Triple { ts, .. } => Some(*ts),
            Payload::Other(props) => match props.get(interner, TripleField::Ts.key()) {
                Some(Value::Int(i)) => Some(i),
                _ => None,
            },
        }
    }

    pub(crate) fn get(&self, interner: &Interner, key: &str) -> Option<Value> {
        match self {
            Payload::Triple { id, amount, ts } => triple_prop(id, amount, *ts, key),
            Payload::Other(props) => props.get(interner, key),
        }
    }
}

/// The `Triple` payload's property named `key`.
fn triple_prop(id: &Arc<str>, amount: &Value, ts: i64, key: &str) -> Option<Value> {
    match TripleField::of(key)? {
        TripleField::Id => Some(Value::String(id.clone())),
        TripleField::Amount => Some(amount.clone()),
        TripleField::Ts => Some(Value::Int(ts)),
    }
}

impl RelPayload {
    /// [`Self::get`] with the key already interned: `kid` is the id of
    /// `key` (a plan-resolved token), so no interner lookup is paid.
    pub(crate) fn get_by_id(&self, key: &str, kid: u32) -> Option<Value> {
        match self {
            Payload::Triple { id, amount, ts } => triple_prop(id, amount, *ts, key),
            Payload::Other(props) => props.get(kid).cloned(),
        }
    }
}

impl Payload<CompactProps> {
    /// [`Payload::from_map`] over a shared property map read in place — the
    /// restore row's shape — keeping the same triple test. `names` collapses
    /// the `Other` path's interner lookups to one per distinct key in the
    /// slice.
    pub(crate) fn from_map_ref(
        interner: &Interner,
        names: &mut NameCache,
        map: &PropertyMap,
    ) -> Self {
        match triple_shape(map.len(), |key| map.get(key)) {
            Some(parts) => Payload::Triple {
                id: parts.id,
                amount: parts.amount,
                ts: parts.ts,
            },
            None => Payload::Other(CompactProps::from_map_ref(interner, names, map)),
        }
    }

    /// The storage choice the byte account has to follow. `Other` stays
    /// `Other` even when its keys are `id`, `amount`, and `ts`.
    pub(crate) fn form(&self, interner: &Interner) -> PayloadForm {
        match self {
            Payload::Triple { id, amount, ts } => Payload::Triple {
                id: id.clone(),
                amount: amount.clone(),
                ts: *ts,
            },
            Payload::Other(props) => Payload::Other(props.to_map(interner)),
        }
    }

    pub(crate) fn keys(&self, interner: &Interner) -> Vec<String> {
        match self {
            Payload::Triple { .. } => TRIPLE_KEYS.iter().map(|k| (*k).into()).collect(),
            Payload::Other(props) => props.keys_as_strings(interner),
        }
    }

    /// The property key ids only, with this payload's interner reads done
    /// here and once. See [`PayloadKeyIds`] for the callers that must build
    /// it before taking another lock.
    pub(crate) fn key_ids(&self, interner: &Interner) -> PayloadKeyIds<'_> {
        match self {
            Payload::Triple { .. } => {
                PayloadKeyIds::Triple(TRIPLE_KEYS.map(|key| interner.get_id(key)))
            }
            Payload::Other(props) => PayloadKeyIds::Other(props),
        }
    }

    /// One pass over every property as (interned key id, value) pairs
    /// (ADR-0185): `Other` zips its stored ids and values; `Triple`
    /// resolves its three fixed keys once and yields from its fields. No
    /// string de-interning or re-hashing.
    pub(crate) fn for_each_key_value(&self, interner: &Interner, mut f: impl FnMut(u32, &Value)) {
        match self {
            Payload::Triple { id, amount, ts } => {
                // Only the two synthesized fields materialize; `amount` is
                // lent by reference.
                let id_value = Value::String(id.clone());
                let ts_value = Value::Int(*ts);
                let pairs = [
                    (TRIPLE_KEYS[0], amount),
                    (TRIPLE_KEYS[1], &id_value),
                    (TRIPLE_KEYS[2], &ts_value),
                ];
                for (k, v) in pairs {
                    if let Some(kid) = interner.get_id(k) {
                        f(kid, v);
                    }
                }
            }
            Payload::Other(props) => {
                for (k, v) in props.iter() {
                    f(k, v);
                }
            }
        }
    }

    pub(crate) fn tagged_json_len(&self, interner: &Interner) -> Result<u64> {
        match self {
            Payload::Triple { id, amount, ts } => json_len(&BorrowedTriple {
                id,
                amount,
                ts: *ts,
            }),
            Payload::Other(props) => props.tagged_json_len(interner),
        }
    }
}

/// `{amount, id, ts}` in the same tagged JSON as a property map, without
/// cloning the triple fields. Key order does not change the byte length.
pub(super) struct BorrowedTriple<'a> {
    pub(super) id: &'a str,
    pub(super) amount: &'a Value,
    pub(super) ts: i64,
}

impl serde::Serialize for BorrowedTriple<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry(TRIPLE_KEYS[0], self.amount)?;
        map.serialize_entry(TRIPLE_KEYS[1], &relate_core::tagged_str(self.id))?;
        map.serialize_entry(TRIPLE_KEYS[2], &Value::Int(self.ts))?;
        map.end()
    }
}
