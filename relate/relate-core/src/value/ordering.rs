//! `Value` equality and ordering: Cypher's three-valued `cypher_eq`, the
//! strict `eq_non_null`, the three-valued `compare` and the total
//! `order_by_cmp` (ADR-0462 depth guards threaded through every container
//! arm). Extracted from `value/mod.rs` for the file-size rule, following
//! the `json.rs`/`props.rs` precedent; the methods stay inherent, so no
//! caller changes.

use std::cmp::Ordering;

use super::*;

/// Three-valued AND over element-wise `cypher_eq` results: any false pair
/// yields false; otherwise any null pair yields null. Shared by the List
/// and Map arms of [`Value::cypher_eq`]; `depth` is threaded through so the
/// ADR-0462 guard reaches the recursive element comparison.
fn cypher_eq_pairs<'a>(pairs: impl Iterator<Item = (&'a Value, &'a Value)>, depth: u32) -> Value {
    let mut saw_null = false;
    for (x, y) in pairs {
        match x.cypher_eq_at(y, depth) {
            Value::Bool(false) => return Value::Bool(false),
            Value::Null => saw_null = true,
            _ => {}
        }
    }
    if saw_null {
        Value::Null
    } else {
        Value::Bool(true)
    }
}

/// The first elementwise comparison that is not `equal`, or `tie` when every
/// zipped pair ties. Both iterators are zipped, so only the first `min(len)`
/// pairs are compared; `tie` is where a caller states its length rule. A
/// `None` result from `cmp` is terminal: a null operand decides the whole
/// comparison as null. The one spelling of the hand-rolled
/// first-non-equal loops ([`compare_maps`], [`Value::compare`]'s list arm,
/// [`Value::order_by_cmp`]'s list/vector arms).
fn lexicographic<A, B, C>(
    a: impl Iterator<Item = A>,
    b: impl Iterator<Item = B>,
    equal: C,
    tie: C,
    mut cmp: impl FnMut(A, B) -> C,
) -> C
where
    C: PartialEq,
{
    for (x, y) in a.zip(b) {
        let result = cmp(x, y);
        if result != equal {
            return result;
        }
    }
    tie
}

/// Elementwise [`Value::eq_non_null`] over two same-length sequences — the
/// one list/vector element comparison in [`Value::eq_non_null`]. `depth` is
/// the container level of the elements; the ADR-0462 guard caps the walk.
fn elementwise_eq(a: &[Value], b: &[Value], depth: u32) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|(x, y)| x.eq_non_null_at(y, depth))
}

/// The map half of [`Value::compare`]: size first, then the sorted key lists
/// element by element, then the values under those keys. Both operands are
/// `BTreeMap`s, so iteration is already key order and equal lengths plus
/// equal key lists mean the same key set.
fn compare_maps(
    a: &std::collections::BTreeMap<String, Value>,
    b: &std::collections::BTreeMap<String, Value>,
    depth: u32,
) -> Option<Ordering> {
    match a.len().cmp(&b.len()) {
        Ordering::Equal => {}
        other => return Some(other),
    }
    let by_key = lexicographic(
        a.keys(),
        b.keys(),
        Ordering::Equal,
        Ordering::Equal,
        |ka: &String, kb: &String| ka.cmp(kb),
    );
    if by_key != Ordering::Equal {
        return Some(by_key);
    }
    lexicographic(
        a.values(),
        b.values(),
        Some(Ordering::Equal),
        Some(Ordering::Equal),
        |x, y| x.compare_at(y, depth),
    )
}

impl Value {
    /// Cypher equality: numbers compare across int/float; null = x is null.
    /// Lists and maps are three-valued: any `false` pair yields false; otherwise
    /// any `null` pair yields null.
    pub fn cypher_eq(&self, other: &Value) -> Value {
        self.cypher_eq_at(other, 0)
    }

    /// [`Value::cypher_eq`] with the ADR-0462 depth guard threaded through
    /// the container arms: a comparison past [`MAX_VALUE_DEPTH`] levels
    /// answers `false` instead of recursing, consistent with the sibling
    /// comparison walkers. Unreachable behind the checked constructors.
    fn cypher_eq_at(&self, other: &Value, depth: u32) -> Value {
        if self.is_null() || other.is_null() {
            return Value::Null;
        }
        match (self, other) {
            (Value::List(a), Value::List(b)) if descends(depth) => {
                if a.len() != b.len() {
                    return Value::Bool(false);
                }
                cypher_eq_pairs(a.iter().zip(b.iter()), depth + 1)
            }
            (Value::Map(a), Value::Map(b)) if descends(depth) => {
                if a.len() != b.len() || a.keys().any(|k| !b.contains_key(k)) {
                    return Value::Bool(false);
                }
                cypher_eq_pairs(
                    a.iter().map(|(k, v)| (v, b.get(k).expect("key checked"))),
                    depth + 1,
                )
            }
            // Over-depth containers saturate instead of recursing.
            (Value::List(_), Value::List(_)) | (Value::Map(_), Value::Map(_)) => Value::Bool(false),
            // Everything else — including a Vector pair at over-cap depth —
            // inherits the accumulated budget rather than restarting at 0.
            _ => Value::Bool(self.eq_non_null_at(other, depth)),
        }
    }

    pub fn eq_non_null(&self, other: &Value) -> bool {
        self.eq_non_null_at(other, 0)
    }

    /// [`Value::eq_non_null`] with the ADR-0462 depth guard threaded through
    /// the container arms: a comparison past [`MAX_VALUE_DEPTH`] levels
    /// answers `false` instead of recursing. Unreachable behind the checked
    /// constructors — the arms that descend only run at a legal depth.
    fn eq_non_null_at(&self, other: &Value, depth: u32) -> bool {
        match (self, other) {
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            // Exact Int-vs-Float equality: a plain `(a as f64) == b` cast
            // rounds away low bits for `|a| > 2^53`, so `9007199254740993`
            // spuriously equaled `9007199254740992.0` here while the exact
            // equivalence keys (DISTINCT/GROUP BY/property index) did not --
            // an index's presence could change a MATCH/MERGE answer.
            (Value::Int(a), Value::Float(b)) => relate_model::canonical_key::eq_i64_f64(*a, *b),
            (Value::Float(a), Value::Int(b)) => relate_model::canonical_key::eq_i64_f64(*b, *a),
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Bytes(a), Value::Bytes(b)) => a == b,
            (Value::Uuid(a), Value::Uuid(b)) => a == b,
            (Value::List(a), Value::List(b)) if descends(depth) => elementwise_eq(a, b, depth + 1),
            (Value::Map(a), Value::Map(b)) if descends(depth) => {
                a.len() == b.len()
                    && a.iter()
                        .all(|(k, v)| b.get(k).is_some_and(|o| v.eq_non_null_at(o, depth + 1)))
            }
            (Value::Node(a), Value::Node(b)) => a.id == b.id,
            (Value::Relationship(a), Value::Relationship(b)) => a.id == b.id,
            (Value::Path(a), Value::Path(b)) => a == b,
            // The spec's temporal equality is offset/zone-sensitive and is
            // *not* `compare() == Equal`: a DST fall-back overlap is equal
            // (`Temporal::equals`) yet ordered.
            (Value::Temporal(a), Value::Temporal(b)) => a.equals(b),
            (Value::Point(a), Value::Point(b)) => a == b,
            (
                Value::Vector {
                    coord: ac,
                    values: av,
                },
                Value::Vector {
                    coord: bc,
                    values: bv,
                },
            ) if descends(depth) => {
                // Elementwise-exact, like the List arm: a derived `av == bv`
                // is variant-strict (`Int(1)` vs `Float(1.0)` differs), which
                // disagreed with the equivalence keys' numeric normalization
                // of vector elements.
                ac == bc && elementwise_eq(av, bv, depth + 1)
            }
            _ => false,
        }
    }

    /// Cypher's three-valued ordering over one type-pair (`None` = null: the
    /// pair is incomparable, or a `null` operand reached here). Orderable:
    /// numbers, strings, bytes, booleans, lists, maps, and the temporal kinds
    /// except durations. Not orderable despite having `=`: UUIDs, points,
    /// durations, and any cross-type pair — the `<=`/`>=` arms of the
    /// comparison operators still hold for equal operands (`cmp_bool`).
    pub fn compare(&self, other: &Value) -> Option<Ordering> {
        self.compare_at(other, 0)
    }

    /// [`Value::compare`] with the ADR-0462 depth guard threaded through the
    /// container arms: a comparison past [`MAX_VALUE_DEPTH`] levels answers
    /// `None` (incomparable) instead of recursing.
    fn compare_at(&self, other: &Value, depth: u32) -> Option<Ordering> {
        if self.is_null() || other.is_null() {
            return None;
        }
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => Some(a.cmp(b)),
            (Value::Float(a), Value::Float(b)) => a.partial_cmp(b),
            // Exact Int-vs-Float ordering, consistent with the equality above
            // (both route through `relate_model::canonical_key`), so `<`/
            // `ORDER BY` and `=` never disagree past `2^53`.
            (Value::Int(a), Value::Float(b)) => relate_model::canonical_key::cmp_i64_f64(*a, *b),
            (Value::Float(a), Value::Int(b)) => {
                relate_model::canonical_key::cmp_i64_f64(*b, *a).map(Ordering::reverse)
            }
            (Value::String(a), Value::String(b)) => Some(a.cmp(b)),
            // Byte-wise, like the List arm: element by element, then length.
            (Value::Bytes(a), Value::Bytes(b)) => Some(a.cmp(b)),
            (Value::Bool(a), Value::Bool(b)) => Some(a.cmp(b)),
            // CIP-246: UUIDs are NOT ordered by the comparison operators --
            // only equality is defined, so a distinct pair is incomparable
            // (NULL for `<`/`>`), while an equal pair still yields `Equal`
            // and so `>=`/`<=` are true (`UUID(x) >= UUID(x)` is true). The
            // numeric order lives in `order_by_cmp` (UUID ranks between
            // BOOLEAN and TEMPORAL), so ORDER BY / min / max still sort.
            (Value::Uuid(a), Value::Uuid(b)) => (*a == *b).then_some(Ordering::Equal),
            (Value::Temporal(a), Value::Temporal(b)) => a.compare(b),
            (Value::List(a), Value::List(b)) if descends(depth) => lexicographic(
                a.iter(),
                b.iter(),
                Some(Ordering::Equal),
                Some(a.len().cmp(&b.len())),
                |x, y| x.compare_at(y, depth + 1),
            ),
            // Maps have a total order: size, then the sorted keys, then the
            // values under them. A null value makes the comparison null
            // while a differing key decides the order on its own, which is
            // why `{k1: 42} < {k2: null}` is true but `{k: 42} < {k: null}`
            // is null.
            (Value::Map(a), Value::Map(b)) if descends(depth) => compare_maps(a, b, depth + 1),
            // Everything left has no ordering arm: UUIDs and points (only `=`
            // is defined for them — `ORDER BY` still ranks them, see
            // `order_by_cmp`), paths, entities, and every cross-type pair.
            _ => None,
        }
    }

    /// Total order for ORDER BY / min / max. Comparison operators still use
    /// `compare`, which is three-valued and incomparable across types.
    pub fn order_by_cmp(&self, other: &Value) -> Ordering {
        self.order_by_cmp_at(other, 0)
    }

    /// [`Value::order_by_cmp`] with the ADR-0462 depth guard threaded
    /// through the container arms: past [`MAX_VALUE_DEPTH`] levels the walk
    /// answers `Equal` instead of recursing. Unreachable behind the checked
    /// constructors.
    fn order_by_cmp_at(&self, other: &Value, depth: u32) -> Ordering {
        let ra = self.order_type_rank();
        let rb = other.order_type_rank();
        if ra != rb {
            return ra.cmp(&rb);
        }
        match (self, other) {
            (Value::Null, Value::Null) => Ordering::Equal,
            // Not orderable by `compare` (no comparison operators), but
            // ORDER BY / min / max still rank them.
            (Value::Uuid(a), Value::Uuid(b)) => a.cmp(b),
            (Value::Temporal(a), Value::Temporal(b)) => a.total_cmp(b),
            (Value::List(a), Value::List(b)) if descends(depth) => lexicographic(
                a.iter(),
                b.iter(),
                Ordering::Equal,
                a.len().cmp(&b.len()),
                |x, y| x.order_by_cmp_at(y, depth + 1),
            ),
            (
                Value::Vector {
                    coord: ac,
                    values: av,
                },
                Value::Vector {
                    coord: bc,
                    values: bv,
                },
            ) if descends(depth) => {
                let type_order = ac
                    .order_rank()
                    .cmp(&bc.order_rank())
                    .then_with(|| av.len().cmp(&bv.len()));
                if type_order != Ordering::Equal {
                    return type_order;
                }
                lexicographic(
                    av.iter(),
                    bv.iter(),
                    Ordering::Equal,
                    Ordering::Equal,
                    |x, y| x.order_by_cmp_at(y, depth + 1),
                )
            }
            _ => self.compare_at(other, depth).unwrap_or(Ordering::Equal),
        }
    }

    /// OpenCypher ORDER BY type order (ascending):
    /// MAP < NODE < RELATIONSHIP < LIST < BYTES < PATH < VECTOR < POINT <
    /// TEMPORAL < STRING < BOOLEAN < UUID < NUMBER < NaN < NULL. A temporal
    /// sorts after the containers/points and before STRING (CIP-222's
    /// Neo4j-verified mixed-type max/min/sort: `date('2024-06-01')` is the
    /// minimum and `true` the maximum of `[date, true, 'hi']`; UUID sits
    /// between BOOLEAN and NUMBER per the CIP-246 amendment; BYTES, a
    /// sequence value in Neo4j, directly after LIST, ADR-0327).
    ///
    /// The temporal kinds are ordered among themselves — ZONED DATETIME <
    /// LOCAL DATETIME < DATE < ZONED TIME < LOCAL TIME < DURATION — because
    /// `Temporal::total_cmp` answers `Equal` across unlike kinds and this
    /// rank is what keeps them distinct (live Neo4j 2026.10.1: `coll.sort`
    /// and `ORDER BY` agree on that sequence, independent of the values).
    /// [`TemporalKind`] is declared in exactly that order, so its
    /// discriminant is the rank's offset from TEMPORAL's base rank.
    fn order_type_rank(&self) -> u8 {
        match self {
            Value::Map(_) => 0,
            Value::Node(_) => 1,
            Value::Relationship(_) => 2,
            Value::List(_) => 3,
            Value::Bytes(_) => 4,
            Value::Path(_) => 5,
            Value::Vector { .. } => 6,
            Value::Point(_) => 7,
            Value::Temporal(t) => 8 + t.kind() as u8,
            Value::String(_) => 14,
            Value::Bool(_) => 15,
            // CIP-246: UUID orders between BOOLEAN and NUMBER (live
            // Neo4j 2026.09: true < uuid < 1).
            Value::Uuid(_) => 16,
            Value::Int(_) => 17,
            Value::Float(f) if f.is_nan() => 19,
            Value::Float(_) => 17,
            Value::Null => 20,
        }
    }
}
