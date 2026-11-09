//! The property-policy module at the crate root (F-1-1, LABS-668): the
//! pure `relate_core`-only verdict walk — can this value be stored,
//! and why not (ADR-0327, ADR-0339) — plus the one canonical
//! property-admission predicate. Both top-level trees import it
//! downward: the CREATE/MERGE materialization and SET/REMOVE clauses
//! (`exec/`), and the pattern-property policy the eval walker and every
//! other engine reads. The expression adapter that produces a pattern's
//! wanted map lives beside its evaluation deps in the eval tree
//! (`pattern_walk.rs::map_from_expr`).

use std::collections::BTreeMap;

use relate_core::{Error, Result, Value};
use relate_names::NamedToken;

/// The `TypeError` detail for a byte array stored as a property (ADR-0327).
const BYTES_PROPERTY_DETAIL: &str = "byte array values cannot be stored as node or \
     relationship properties; they live only inside a statement (parameter to result)";

/// Why a value cannot be stored as a property.
pub(crate) enum PropertyInvalidity {
    /// A byte array (ADR-0327), directly or nested in a list at any depth.
    HoldsBytes,
    /// Any other non-storable shape.
    Other,
}

/// Check that `v` can be stored as a property value: `Ok`, the typed
/// `TypeError` for a byte array (or a list holding one at any depth;
/// ADR-0327), or the `InvalidPropertyType` type error for every other
/// non-storable value. Null is storable and transparent: each caller
/// applies its own [`NullPolicy`] at the one map walk, [`admit_props`].
pub(crate) fn check_property_value(v: &Value) -> Result<()> {
    match property_value_invalidity(v) {
        None => Ok(()),
        Some(PropertyInvalidity::HoldsBytes) => Err(Error::type_error(BYTES_PROPERTY_DETAIL)),
        Some(PropertyInvalidity::Other) => Err(Error::ty("InvalidPropertyType")),
    }
}

/// Validate and normalize a value for storage: the reference engine stores
/// a list mixing integers and floats as an all-float list (`[1.0, 2]` reads
/// back `[1.0, 2.0]` with `valueType` `LIST<FLOAT NOT NULL>`), which is why
/// the two numeric kinds share one storable list kind below. Every other
/// value passes through unchanged.
pub(crate) fn prepare_property_value(v: Value) -> Result<Value> {
    check_property_value(&v)?;
    Ok(coerce_mixed_numeric_list(v))
}

fn coerce_mixed_numeric_list(v: Value) -> Value {
    let Value::List(xs) = v else {
        return v;
    };
    let has_int = xs.iter().any(|x| matches!(x, Value::Int(_)));
    let has_float = xs.iter().any(|x| matches!(x, Value::Float(_)));
    if !(has_int && has_float) {
        return Value::List(xs);
    }
    Value::List(
        xs.iter()
            .map(|x| match x {
                Value::Int(i) => Value::Float(*i as f64),
                other => other.clone(),
            })
            .collect::<Vec<_>>()
            .into(),
    )
}

/// The one property-value walk: `None` when `v` can be stored,
/// `Some(HoldsBytes)` when a byte array appears anywhere inside it (a list
/// nested at any depth included), `Some(Other)` for every other
/// non-storable shape. A list is storable when its items are one level of
/// homogeneous scalar values; a null item (`[1, null]`), a nested list of
/// scalars, or a mixed list is `Other` (Neo4j 22N39: a list holding null
/// cannot be stored in properties).
pub(crate) fn property_value_invalidity(v: &Value) -> Option<PropertyInvalidity> {
    match v {
        Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::String(_) => None,
        Value::Temporal(_) | Value::Point(_) | Value::Uuid(_) => None,
        // Statement-scoped (ADR-0327): a byte array cannot be stored.
        Value::Bytes(_) => Some(PropertyInvalidity::HoldsBytes),
        // Any vector value stores; the coordinate kind does not narrow the
        // property-value vocabulary (ADR-0339).
        Value::Vector { .. } => None,
        Value::List(xs) => {
            let mut kind = None;
            for item in xs.iter() {
                // A null item makes the whole list unstorable (Neo4j 22N39),
                // not transparent: `[1, null]` is not a property value.
                if item.is_null() {
                    return Some(PropertyInvalidity::Other);
                }
                // A byte array anywhere inside the item is the byte-array
                // verdict; otherwise the item must be a scalar of the
                // list's one kind (integers and floats are one kind: the
                // engine stores the list as all floats).
                if bytes_within(item) {
                    return Some(PropertyInvalidity::HoldsBytes);
                }
                let Some(item_kind) = list_item_kind(item) else {
                    return Some(PropertyInvalidity::Other);
                };
                if kind.is_some_and(|expected| expected != item_kind) {
                    return Some(PropertyInvalidity::Other);
                }
                kind = Some(item_kind);
            }
            None
        }
        _ => Some(PropertyInvalidity::Other),
    }
}

/// Whether a byte array appears inside `v` — the value itself or a list
/// holding one at any depth. The byte-array detail wins over the generic
/// one when a list is otherwise non-storable.
///
/// `depth` is the number of containers enclosing `v`; the walk descends only
/// while [`relate_core::next_depth`] allows it (ADR-0462), so a deeper
/// value — impossible behind the checked constructors — cannot recurse.
fn bytes_within(v: &Value) -> bool {
    bytes_within_at(v, 0)
}

fn bytes_within_at(v: &Value, depth: u32) -> bool {
    match v {
        Value::Bytes(_) => true,
        Value::List(xs) => relate_core::next_depth(depth)
            .is_some_and(|next| xs.iter().any(|x| bytes_within_at(x, next))),
        _ => false,
    }
}

/// The scalar kind of a storable list item; `None` for every non-scalar
/// (a nested list, map, entity or byte array). `Int` and `Float` are one
/// kind because storage coerces a mixed numeric list to floats.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ListItemKind {
    Numeric,
    Scalar(std::mem::Discriminant<Value>),
}

fn list_item_kind(v: &Value) -> Option<ListItemKind> {
    match v {
        Value::Null => None,
        Value::Int(_) | Value::Float(_) => Some(ListItemKind::Numeric),
        Value::Bool(_)
        | Value::String(_)
        | Value::Temporal(_)
        | Value::Point(_)
        | Value::Uuid(_) => Some(ListItemKind::Scalar(std::mem::discriminant(v))),
        _ => None,
    }
}

/// How the one property-map admission walk ([`admit_props`]) treats a null
/// entry — the single policy on which the admitting clauses and the
/// pattern-map readers differ (null itself is storable and transparent,
/// [`check_property_value`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NullPolicy {
    /// Drop the entry: a null is not stored. The policy of the
    /// CREATE/MERGE materialization maps and `SET n = <map>` (full
    /// replacement).
    Drop,
    /// Keep the entry: `SET n += <map>` feeds the map to
    /// `merge_properties_*`, which runs each entry through the set verb —
    /// a kept null records the key's *removal* (`SET n += {k: null}`
    /// removes `k`), so the entry must survive admission.
    KeepAsRemoval,
    /// Keep the entry: a pattern's *match* map (MATCH, pattern predicates
    /// and comprehensions, shortest paths, a WHERE equality pushed into
    /// the map) is a conjunction of equalities, and `n.k = null` is never
    /// true — a kept null equals nothing under [`props_all_match`]'s
    /// `eq_non_null`, so the element matches nothing (LABS-1086).
    /// Dropping it, as storage must, silently widened `{k: null}` into
    /// "any value of k".
    KeepAsUnmatchable,
}

impl NullPolicy {
    /// The null-*source* half of the policy: the map a null source coerces
    /// to, or `None` when a null source is not admissible (the `+=` arm —
    /// a null source fails like every other non-map source). Exhaustive
    /// over the variants, so a future policy decides this here, not at a
    /// call site's re-derived negative match.
    pub(crate) fn null_source_map(&self) -> Option<BTreeMap<String, Value>> {
        match self {
            NullPolicy::Drop => Some(BTreeMap::new()),
            NullPolicy::KeepAsRemoval => None,
            // A null *map* constrains nothing — the absent-map meaning;
            // only a null *entry* is unsatisfiable.
            NullPolicy::KeepAsUnmatchable => Some(BTreeMap::new()),
        }
    }

    /// Whether the admission walk drops a null entry — the entry half of
    /// the policy, decided here once.
    fn drops_null_entries(&self) -> bool {
        match self {
            NullPolicy::Drop => true,
            NullPolicy::KeepAsRemoval | NullPolicy::KeepAsUnmatchable => false,
        }
    }
}

/// The one property-map admission walk (LABS-818): every entry's value is
/// validated and normalized by [`prepare_property_value`], and a null
/// entry is dropped or kept per the caller's [`NullPolicy`].
pub(crate) fn admit_props(
    map: BTreeMap<String, Value>,
    nulls: NullPolicy,
) -> Result<BTreeMap<String, Value>> {
    let drop_nulls = nulls.drops_null_entries();
    let mut out = BTreeMap::new();
    for (k, v) in map {
        if drop_nulls && v.is_null() {
            continue;
        }
        out.insert(k, prepare_property_value(v)?);
    }
    Ok(out)
}

/// The one canonical property-admission predicate (LABS-668) — the match
/// half of the pattern-property policy
/// (`pattern_walk.rs::map_from_expr` produces the wanted entries): every
/// wanted entry must be present on the element and equal under
/// `eq_non_null`. The element is read key by key through `read` — the
/// store's per-key reads (`ExecGraph::node_prop_resolved` /
/// `rel_prop_resolved`), so an admission compares the wanted entries
/// without cloning the element's whole property map (LABS-853) and
/// without an interner lookup when the plan resolved the key.
pub(crate) fn props_all_match(
    want: &[(NamedToken, Value)],
    mut read: impl FnMut(&NamedToken) -> Option<Value>,
) -> bool {
    want.iter()
        .all(|(key, v)| read(key).is_some_and(|stored| stored.eq_non_null(v)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use relate_core::{PointValue, Temporal, VectorCoordKind};

    fn bytes() -> Value {
        Value::Bytes(vec![0, 1, 255].into())
    }

    /// ADR-0339: one walk decides validity and the reason; the byte-array
    /// detail is reported for a byte array anywhere inside the value.
    #[test]
    fn property_value_invalidity_classifies_nested_bytes() {
        let scalar = Value::Int(1);
        assert!(check_property_value(&scalar).is_ok());
        assert!(check_property_value(&Value::Null).is_ok());
        assert!(check_property_value(&Value::Temporal(Temporal::Duration {
            months: 1,
            days: 0,
            nanos: 0
        }))
        .is_ok());
        assert!(check_property_value(&Value::Point(PointValue::cartesian_2d(1.0, 2.0))).is_ok());
        assert!(check_property_value(&Value::Vector {
            coord: VectorCoordKind::Int8,
            values: vec![Value::Int(1)].into(),
        })
        .is_ok());

        // Homogeneous scalar lists store; a list holding null, a mixed
        // non-numeric list and a nested scalar list do not.
        assert!(
            check_property_value(&Value::List(vec![Value::Int(1), Value::Int(2)].into())).is_ok()
        );
        // Integers and floats are one storable kind: storage coerces the
        // list to floats (Neo4j `[1.0, 2]` reads back `[1.0, 2.0]`).
        assert!(
            check_property_value(&Value::List(vec![Value::Int(1), Value::Float(2.5)].into()))
                .is_ok()
        );
        assert_eq!(
            check_property_value(&Value::List(vec![Value::Null, Value::Int(1)].into()))
                .unwrap_err(),
            Error::ty("InvalidPropertyType")
        );
        assert_eq!(
            check_property_value(&Value::List(
                vec![Value::Int(1), Value::String("a".into())].into()
            ))
            .unwrap_err(),
            Error::ty("InvalidPropertyType")
        );
        assert_eq!(
            check_property_value(&Value::List(
                vec![Value::List(vec![Value::Int(1)].into())].into()
            ))
            .unwrap_err(),
            Error::ty("InvalidPropertyType")
        );

        // A byte array names itself at any depth; every other non-scalar
        // keeps the generic detail.
        let bytes_detail = Error::type_error(BYTES_PROPERTY_DETAIL);
        assert_eq!(check_property_value(&bytes()).unwrap_err(), bytes_detail);
        assert_eq!(
            check_property_value(&Value::List(vec![bytes()].into())).unwrap_err(),
            bytes_detail
        );
        assert_eq!(
            check_property_value(&Value::List(
                vec![Value::List(vec![Value::Int(1), bytes()].into())].into()
            ))
            .unwrap_err(),
            bytes_detail
        );
        assert_eq!(
            check_property_value(&Value::Map(BTreeMap::new().into())).unwrap_err(),
            Error::ty("InvalidPropertyType")
        );
    }

    /// A list mixing integers and floats is stored as an all-float list,
    /// the reference engine's storage coercion.
    #[test]
    fn prepare_property_value_coerces_mixed_numeric_lists_to_floats() {
        let prepared =
            prepare_property_value(Value::List(vec![Value::Float(1.0), Value::Int(2)].into()))
                .unwrap();
        assert_eq!(
            prepared,
            Value::List(vec![Value::Float(1.0), Value::Float(2.0)].into())
        );
        // A homogeneous integer list stays integers.
        let ints = Value::List(vec![Value::Int(1), Value::Int(2)].into());
        assert_eq!(prepare_property_value(ints.clone()).unwrap(), ints);
        // A non-numeric mix is still rejected.
        assert_eq!(
            prepare_property_value(Value::List(
                vec![Value::Int(1), Value::String("a".into())].into()
            ))
            .unwrap_err(),
            Error::ty("InvalidPropertyType")
        );
    }

    /// LABS-818: the one admission walk's two null policies — `Drop`
    /// removes the entry, `KeepAsRemoval` keeps it (a `SET n += {k: null}`
    /// records `k`'s removal downstream); both validate every non-null
    /// value the same way.
    #[test]
    fn admit_props_null_policy_is_the_only_difference() {
        let map = || {
            BTreeMap::from([
                ("a".to_string(), Value::Int(1)),
                ("b".to_string(), Value::Null),
            ])
        };
        assert_eq!(
            admit_props(map(), NullPolicy::Drop).unwrap(),
            BTreeMap::from([("a".into(), Value::Int(1))])
        );
        assert_eq!(
            admit_props(map(), NullPolicy::KeepAsRemoval).unwrap(),
            BTreeMap::from([("a".into(), Value::Int(1)), ("b".into(), Value::Null)])
        );
        let bad = BTreeMap::from([("a".to_string(), Value::Null), ("b".to_string(), bytes())]);
        for nulls in [NullPolicy::Drop, NullPolicy::KeepAsRemoval] {
            assert_eq!(
                admit_props(bad.clone(), nulls).unwrap_err(),
                Error::type_error(BYTES_PROPERTY_DETAIL)
            );
        }
    }
}
