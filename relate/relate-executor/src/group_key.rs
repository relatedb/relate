//! Canonical equivalence key for `DISTINCT`, `GROUP BY`, `UNION`
//! deduplication, aggregate `DISTINCT` (`count(DISTINCT x)`, ...),
//! `coll.distinct`, and (in its equality reading) the property index, the
//! uniqueness probe and `MERGE`/`MATCH` equality buckets.
//!
//! Cypher's *equivalence* relation is deliberately **not** the same relation
//! as `=` equality (see `Value::eq_non_null` / `Value::cypher_eq`): `null` is
//! reflexive here (`null ≡ null`) while `null = null` is `null`, and every
//! NaN is its own equivalence class while `NaN = NaN` is false. This module is
//! the executor's single source of truth for both relations: one typed
//! [`GroupKey`] keys the five dedup/grouping sites, and its equality reading
//! keys every equality bucket, so `RETURN DISTINCT`, `GROUP BY`, `UNION`,
//! `count(DISTINCT ...)`, `coll.distinct(...)`, an indexed `{prop: value}`
//! seek, a uniqueness constraint and `MERGE` derive their verdicts from one
//! normalization -- the divergence the old `Value::to_cypher_literal()` string
//! keys had (they rendered `Int(1)` as `"1"` and `Float(1.0)` as `"1.0"`,
//! splitting groups and missing index seeks, while rendering every NaN as
//! `"NaN"`, merging groups).
//!
//! The Int/Float half of the numeric normalization is the one exact rule
//! owned by [`relate_model::canonical_key`] (ground-truthed against live
//! Neo4j) — `norm_f64` normalizes through its [`f64_exact_i64`], the same
//! primitive `=`/`<` evaluation (`Value::eq_non_null`/`compare`) routes
//! through, so an indexed `{prop: value}` seek, a `WHERE` residual, and a
//! `DISTINCT` bucket can never disagree:
//!
//! * numbers compare by **mathematical value**, ignoring the Int/Float
//!   distinction, recursively inside lists, maps, and vectors:
//!   `1 ≡ 1.0`, `0.0 ≡ -0.0`, `{a:[1]} ≡ {a:[1.0]}`.
//! * **every NaN is its own distinct key** -- never equivalent, even to
//!   itself (`UNWIND [0.0/0.0, 0.0/0.0] AS x RETURN DISTINCT x` yields two
//!   rows). This holds for a NaN nested arbitrarily deep inside a
//!   list/map/point too.
//! * strings are byte-identical only; a string never collides with a list
//!   (`'a,b' ≢ ['a','b']`).
//! * nodes/relationships compare by element id; two structurally identical
//!   nodes with different ids stay distinct. Paths key on their node/rel ids.
//! * temporals key on their value identity, not on a bare instant:
//!   `time('12:00+01:00') ≢ time('13:00+02:00')` (same instant, different
//!   local time/offset) while `time('12:00Z') ≡ time('12:00+00:00')`, and a
//!   zoned DateTime keys on its UTC instant **plus** its [`ZoneId`], so the
//!   two halves of a DST fall-back overlap stay distinct even though `=`
//!   holds between them.
//! * the **first-seen** occurrence's representation is what a dedup caller
//!   emits -- the key only decides set membership (`[1, 1.0]` → `1`;
//!   `[1.0, 1]` → `1.0`; `RETURN 1 UNION RETURN 1.0` → a single row `1`).
//!
//! The same tree keys both grouping and the property index. An index bucket
//! is the value's identity, not its `=` class: Neo4j's range index and
//! uniqueness constraint keep the two halves of a zoned-DateTime DST
//! fall-back overlap apart even though `=` holds between them (and an
//! indexed equality seek therefore does not find the `=`-equal partner,
//! exactly like a scan-free Neo4j plan). ADR-0446 originally coarsened the
//! index key to `=`, which made an indexed seek disagree with Neo4j's; the
//! measured suite corrected it.
//!
//! The key is a fully-owned, normalized tree (`Norm`) whose `Hash`/`Eq` are
//! derived, so the two can never fall out of sync -- the classic hazard of a
//! hand-written `Hash` paired with a hand-written `Eq`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use chrono::{NaiveDate, NaiveDateTime, NaiveTime, Timelike};

use relate_core::temporal::ZoneId;
use relate_core::{Temporal, Value, VectorCoordKind};
use relate_model::canonical_key::f64_exact_i64;

/// Process-global monotonically increasing nonce source for NaN keys. Every
/// NaN encountered while a key is built claims a fresh nonce, so two NaNs
/// never collide -- including NaNs nested inside lists/maps/points. The
/// counter's actual values are irrelevant (equal values never reach this
/// path); only the guaranteed uniqueness matters, so `Relaxed` ordering is
/// sufficient.
static NAN_NONCE: AtomicU64 = AtomicU64::new(0);

fn next_nan_nonce() -> u64 {
    NAN_NONCE.fetch_add(1, Ordering::Relaxed)
}

/// A number normalized so that Int/Float and `+0.0`/`-0.0` collapse while
/// every NaN stays unique. The Int/Float collapse is the one exact rule from
/// [`relate_model::canonical_key`]; the NaN nonce is GroupKey-specific.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum NormNum {
    /// An exact integer value -- integral floats within `i64` range land here
    /// too, so `Int(1)` and `Float(1.0)` share one key.
    Int(i64),
    /// A non-integral (or out-of-`i64`-range) finite float, or an infinity,
    /// keyed by its IEEE-754 bit pattern. `-0.0`/`+0.0` never reach this arm
    /// (they normalize to `Int(0)`), so distinct bit patterns here always mean
    /// distinct mathematical values.
    Bits(u64),
    /// A NaN, tagged with a per-occurrence nonce so it is equivalent to
    /// nothing, not even another NaN.
    Nan(u64),
}

fn norm_f64(f: f64) -> NormNum {
    if f.is_nan() {
        return NormNum::Nan(next_nan_nonce());
    }
    // The exact Int/Float rule, shared with `=`/`<` evaluation: `-0.0` and
    // `+0.0` are both integral and in range, so both land in `Int(0)` --
    // giving `0.0 ≡ -0.0` for free. Anything without an exact `i64`
    // (non-integral, or at/beyond ±2^63) keys by bit pattern and therefore
    // never spuriously collides with the genuine integer
    // `i64::MAX`/`i64::MIN`.
    match f64_exact_i64(f) {
        Some(i) => NormNum::Int(i),
        None => NormNum::Bits(f.to_bits()),
    }
}

/// Normalized temporal. `Time` keys on its (UTC nanos-of-day, offset) pair —
/// `Temporal::equals`'s local-time-and-offset identity — and a zoned
/// `DateTime` on its UTC instant plus [`ZoneId`], so a named zone stays
/// apart from the offset it denotes while `Z` shares `+00:00`'s key, and
/// the two halves of a DST fall-back overlap (same wall clock and zone,
/// different instants) stay apart.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum NormTemporal {
    Date(NaiveDate),
    CivilDate {
        year: i64,
        month: u32,
        day: u32,
    },
    LocalTime(NaiveTime),
    Time {
        /// Nanoseconds since midnight UTC (`local time - offset`).
        utc_nanos: i64,
        offset: i32,
    },
    LocalDateTime(NaiveDateTime),
    CivilDateTime {
        year: i64,
        month: u32,
        day: u32,
        time: NaiveTime,
    },
    /// A zoned datetime: UTC instant plus zone.
    DateTime {
        utc: NaiveDateTime,
        zone: ZoneId,
    },
    Duration {
        months: i64,
        days: i64,
        nanos: i128,
    },
}

/// Nanoseconds since midnight, mirroring `Temporal::compare`'s private
/// `time_to_nanos`.
fn time_to_nanos(t: NaiveTime) -> i64 {
    t.num_seconds_from_midnight() as i64 * 1_000_000_000 + t.nanosecond() as i64
}

fn norm_temporal(t: &Temporal) -> NormTemporal {
    match t {
        Temporal::Date(d) => NormTemporal::Date(*d),
        Temporal::CivilDate { year, month, day } => NormTemporal::CivilDate {
            year: *year,
            month: *month,
            day: *day,
        },
        Temporal::LocalTime(tm) => NormTemporal::LocalTime(*tm),
        Temporal::Time { time, offset } => NormTemporal::Time {
            utc_nanos: time_to_nanos(*time) - i64::from(*offset) * 1_000_000_000,
            offset: *offset,
        },
        Temporal::LocalDateTime(dt) => NormTemporal::LocalDateTime(*dt),
        Temporal::CivilDateTime {
            year,
            month,
            day,
            time,
        } => NormTemporal::CivilDateTime {
            year: *year,
            month: *month,
            day: *day,
            time: *time,
        },
        Temporal::DateTime { utc, offset, zone } => NormTemporal::DateTime {
            utc: *utc,
            zone: ZoneId::of(zone.as_deref(), *offset),
        },
        Temporal::Duration {
            months,
            days,
            nanos,
        } => NormTemporal::Duration {
            months: *months,
            days: *days,
            nanos: *nanos,
        },
    }
}

/// The normalized equivalence tree. Distinct enum variants carry an implicit
/// type tag, so cross-type collisions (a string vs a list, a bool vs an int)
/// are impossible.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Norm {
    /// All nulls share this one key (reflexive), the sole divergence from `=`.
    Null,
    Bool(bool),
    Num(NormNum),
    Str(Arc<str>),
    /// A byte array keys byte-wise (ADR-0327): its own variant, so bytes
    /// never collide with a list of the same integers.
    Bytes(Arc<[u8]>),
    List(Vec<Norm>),
    /// Map entries sorted by key so insertion order does not affect the key;
    /// values keyed recursively, so `{a:1} ≡ {a:1.0}`.
    Map(Vec<(String, Norm)>),
    Node(i64),
    Rel(i64),
    Path(Vec<Norm>, Vec<Norm>),
    Temporal(NormTemporal),
    Point {
        srid: i32,
        coords: Vec<NormNum>,
    },
    Uuid(u128),
    Vector {
        coord: VectorCoordKind,
        values: Vec<Norm>,
    },
}

fn norm(v: &Value) -> Norm {
    match v {
        Value::Null => Norm::Null,
        Value::Bool(b) => Norm::Bool(*b),
        Value::Int(i) => Norm::Num(NormNum::Int(*i)),
        Value::Float(f) => Norm::Num(norm_f64(*f)),
        Value::String(s) => Norm::Str(s.clone()),
        Value::Bytes(b) => Norm::Bytes(b.clone()),
        Value::Uuid(u) => Norm::Uuid(*u),
        Value::List(items) => Norm::List(items.iter().map(norm).collect()),
        Value::Map(m) => {
            let mut entries: Vec<(String, Norm)> =
                m.iter().map(|(k, val)| (k.clone(), norm(val))).collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Norm::Map(entries)
        }
        Value::Node(n) => Norm::Node(n.id),
        Value::Relationship(r) => Norm::Rel(r.id),
        Value::Path(p) => Norm::Path(
            p.nodes.iter().map(|n| Norm::Node(n.id)).collect(),
            p.rels.iter().map(|r| Norm::Rel(r.id)).collect(),
        ),
        Value::Temporal(t) => Norm::Temporal(norm_temporal(t)),
        Value::Point(p) => {
            let mut coords = vec![norm_f64(p.x), norm_f64(p.y)];
            if let Some(z) = p.z {
                coords.push(norm_f64(z));
            }
            Norm::Point {
                srid: p.srid,
                coords,
            }
        }
        Value::Vector { coord, values } => Norm::Vector {
            // The coordinate kind tag keeps an INT8 vector distinct from a
            // FLOAT32 vector even when their element values coincide
            // numerically (matching `Value::eq_non_null`'s vector arm, which
            // compares `coord`). Elements are normalized numerically, so
            // `vector([1], ...)` ≡ `vector([1.0], ...)` — the same
            // elementwise-exact rule `eq_non_null` applies.
            coord: *coord,
            values: values.iter().map(norm).collect(),
        },
    }
}

/// A canonical equivalence key for a single `Value`, per the module-level
/// contract. `Hash + Eq` implement Cypher's *equivalence* relation, the one
/// both [`GroupKey::new`] and [`GroupKey::for_property_index`] encode.
///
/// The encoding is **injective** over the equivalence classes: two values map
/// to equal keys **iff** they are Cypher-equivalent (with the deliberate NaN
/// exception, where every occurrence is its own class). This is precisely the
/// property the old `to_cypher_literal`-string key lacked -- there
/// `Int(1)`/`Float(1.0)` collided-wrong-way while `"1,2"`/`[1,2]` and
/// quote-bearing strings aliased.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct GroupKey(Norm);

impl GroupKey {
    /// Build the equivalence key for a value. A NaN anywhere inside `value`
    /// claims a fresh nonce at this call, so the resulting key is unique and
    /// two separately-built keys over NaN-containing values never compare
    /// equal.
    pub(crate) fn new(value: &Value) -> Self {
        GroupKey(norm(value))
    }

    /// Key for the property index — the buckets of an indexed seek, the
    /// uniqueness probe and the scan re-check: the value's equivalence
    /// identity, except that a value containing a NaN anywhere is unindexable
    /// (`None`). A NaN never satisfies an equality seek (`NaN = NaN` is false),
    /// so indexing it could only create a bucket no seek key can ever address
    /// -- and, because every NaN key claims a fresh nonce, one the GC drop path
    /// could never recompute to remove.
    pub(crate) fn for_property_index(value: &Value) -> Option<Self> {
        if value_has_nan(value) {
            return None;
        }
        Some(GroupKey(norm(value)))
    }

    /// Estimated heap footprint of the key, for the ADR-0042 logical index
    /// accounting in `memory.rs` (which charges per index map entry).
    pub(crate) fn logical_bytes(&self) -> u64 {
        norm_bytes(&self.0)
    }

    /// The class of a value whose word needs no tree: an integer or a node
    /// (the dominant keys), by the same encodings [`Self::class`] gives
    /// them. `None` for every other value, which goes through
    /// [`Self::new`] and [`Self::class`].
    pub(crate) fn class_of(value: &Value) -> Option<KeyClass> {
        match value {
            Value::Int(i) => Some(KeyClass::int(*i)),
            Value::Node(node) => Some(KeyClass::node(node.id)),
            _ => None,
        }
    }

    /// The key's cheap projection for the distinct set's tiers: the one
    /// canonical derivation, so no site re-derives it. One exhaustive
    /// `match` over `Norm` with every variant spelled — a new variant
    /// breaks the build until it is classified.
    ///
    /// Exact by construction: each word payload is a bijection of its
    /// variant's field (`i64` reinterpreted, IEEE bits as-is, `bool` as
    /// 0/1), `Norm` has already applied every equivalence rule (`1 ≡ 1.0`,
    /// `±0.0 → Int(0)`), and the tag keeps variants apart, so
    /// `a.class() == b.class()` exactly when `a == b` for word keys. A NaN
    /// stays [`KeyClass::General`]: its nonce lives only in `GroupKey`
    /// equality. There is no inverse (word → key).
    pub(crate) fn class(&self) -> KeyClass {
        match &self.0 {
            Norm::Null => KeyClass::Null,
            Norm::Bool(b) => KeyClass::Word(WordTag::Bool, u64::from(*b)),
            Norm::Num(NormNum::Int(i)) => KeyClass::int(*i),
            Norm::Num(NormNum::Bits(bits)) => KeyClass::Word(WordTag::FloatBits, *bits),
            Norm::Num(NormNum::Nan(_)) => KeyClass::General,
            Norm::Node(id) => KeyClass::node(*id),
            Norm::Rel(id) => KeyClass::Word(WordTag::Rel, *id as u64),
            Norm::Str(_)
            | Norm::Bytes(_)
            | Norm::List(_)
            | Norm::Map(_)
            | Norm::Path(_, _)
            | Norm::Temporal(_)
            | Norm::Point { .. }
            | Norm::Uuid(_)
            | Norm::Vector { .. } => KeyClass::General,
        }
    }
}

/// [`GroupKey::class`]'s result: the reflexive null, a 64-bit word under its
/// type tag, or a key only the full [`GroupKey`] can represent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyClass {
    Null,
    Word(WordTag, u64),
    General,
}

impl KeyClass {
    /// The one encoding of an integer word.
    fn int(i: i64) -> Self {
        Self::Word(WordTag::Int, i as u64)
    }

    /// The one encoding of a node-id word.
    fn node(id: i64) -> Self {
        Self::Word(WordTag::Node, id as u64)
    }
}

/// The type tag of a word key. Words of different tags never meet: each
/// tag holds a disjoint set of `Norm` variants. The tag→table map is
/// `Spill::word_table` in `distinct_set` — one exhaustive `match`, so a
/// new tag is a compile error there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum WordTag {
    Bool,
    Int,
    FloatBits,
    Node,
    Rel,
}

impl WordTag {
    /// This tag's position in [`Self::ALL`]: an exhaustive match, so a new
    /// variant is a compile error here (a unit test holds the list to it).
    pub(crate) const fn index(self) -> usize {
        match self {
            WordTag::Bool => 0,
            WordTag::Int => 1,
            WordTag::FloatBits => 2,
            WordTag::Node => 3,
            WordTag::Rel => 4,
        }
    }

    /// Every tag: the one vocabulary the distinct set sizes its parked
    /// tiers from.
    pub(crate) const ALL: [WordTag; 5] = [
        WordTag::Bool,
        WordTag::Int,
        WordTag::FloatBits,
        WordTag::Node,
        WordTag::Rel,
    ];
}

/// Whether `leaf` holds anywhere in `value`'s tree — at the value
/// itself or inside any list, map or vector — the one recursion the
/// NaN and zoned-datetime predicates share. The leaves answer only for
/// scalar variants and the recursion covers only the containers, so
/// each policy keeps its leaf and the walk lives once; every other
/// variant is spelled, so a new `Value` variant is a compile error
/// here, exactly as `norm`'s match makes it one there.
fn value_any(value: &Value, leaf: &impl Fn(&Value) -> bool) -> bool {
    leaf(value)
        || match value {
            Value::List(items) => items.iter().any(|item| value_any(item, leaf)),
            Value::Map(m) => m.values().any(|item| value_any(item, leaf)),
            Value::Vector { values, .. } => values.iter().any(|item| value_any(item, leaf)),
            Value::Null
            | Value::Bool(_)
            | Value::Int(_)
            | Value::Float(_)
            | Value::String(_)
            | Value::Node(_)
            | Value::Relationship(_)
            | Value::Path(_)
            | Value::Temporal(_)
            | Value::Point(_)
            | Value::Uuid(_)
            | Value::Bytes(_) => false,
        }
}

/// True if a NaN occurs anywhere inside `v` (recursively, mirroring `norm`).
fn value_has_nan(v: &Value) -> bool {
    value_any(v, &|v| match v {
        Value::Float(f) => f.is_nan(),
        Value::Point(p) => p.x.is_nan() || p.y.is_nan() || p.z.is_some_and(|z| z.is_nan()),
        _ => false,
    })
}

/// True when `value`'s equivalence identity is narrower than its `=`
/// class: a **named-zone** `DateTime` (an IANA region) occurs anywhere
/// in it — bare, or nested inside a list, map or vector. The two halves
/// of a DST fall-back overlap are `=`-equal (one wall clock and zone)
/// but key apart ([`NormTemporal::DateTime`] carries the instant plus
/// the zone), so an identity-keyed bucket — the property index's —
/// cannot answer a `=` lookup for such a value: it would miss the
/// partner a scan finds (the declared range index answers identity for
/// the same reason, Neo4j's recorded behaviour). An offset-only
/// datetime — `Z`, a bare offset, no zone — resolves to
/// [`ZoneId::Offset`], whose (instant, offset) identity is a bijection
/// of its `=` class (wall clock, offset): it serves exactly and never
/// declines. Every other value's identity is exactly its `=` class —
/// the numeric and structural normalizations are the same rules `=`
/// evaluation applies. The lazy equality postings' MATCH serve
/// (ADR-0614) and the overlay's scratch-mode decision read this as
/// their decline.
pub(crate) fn identity_narrower_than_eq(value: &Value) -> bool {
    value_any(value, &|v| {
        matches!(
            v,
            Value::Temporal(Temporal::DateTime { zone, offset, .. })
                if matches!(ZoneId::of(zone.as_deref(), *offset), ZoneId::Named(_))
        )
    })
}

/// Rough heap size of a normalized key tree: an 8-byte enum tag plus payload.
fn norm_bytes(n: &Norm) -> u64 {
    const TAG: u64 = 8;
    const HDR: u64 = 16; // String/Vec header.
    match n {
        Norm::Null | Norm::Bool(_) => TAG + 1,
        Norm::Num(_) => TAG + 16,
        Norm::Str(s) => TAG + HDR + s.len() as u64,
        Norm::Bytes(b) => TAG + HDR + b.len() as u64,
        Norm::List(items) => TAG + HDR + items.iter().map(norm_bytes).sum::<u64>(),
        Norm::Map(entries) => {
            TAG + HDR
                + entries
                    .iter()
                    .map(|(k, v)| HDR + k.len() as u64 + norm_bytes(v))
                    .sum::<u64>()
        }
        Norm::Node(_) | Norm::Rel(_) => TAG + 8,
        Norm::Path(ns, rs) => TAG + 2 * HDR + (ns.len() + rs.len()) as u64 * (TAG + 8),
        Norm::Temporal(_) => TAG + 24,
        Norm::Uuid(_) => TAG + 16,
        Norm::Point { coords, .. } => TAG + 4 + (TAG + 8) * coords.len() as u64,
        Norm::Vector { values, .. } => TAG + 1 + HDR + values.iter().map(norm_bytes).sum::<u64>(),
    }
}

/// Build the equivalence key for a whole row: its column values in column
/// order. Used by RETURN/WITH `DISTINCT` and `UNION` deduplication, where two
/// rows are the same iff every corresponding column value is equivalent.
pub(crate) fn row_key(vals: &[Value]) -> Vec<GroupKey> {
    vals.iter().map(GroupKey::new).collect()
}

#[cfg(test)]
mod tests;
