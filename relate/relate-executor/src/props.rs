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

use relate_core::temporal::TemporalKind;
use relate_names::NamedToken;

/// The `TypeError` detail for a byte array stored as a property (ADR-0327).
const BYTES_PROPERTY_DETAIL: &str = "byte array values cannot be stored as node or \
     relationship properties; they live only inside a statement (parameter to result)";

/// The `TypeError` detail for a point list holding more than one CRS (live
/// Neo4j 2026.10, Query API probe 2026-11-27: any differing SRID pair —
/// geographic vs cartesian, 2D vs 3D within a family — rejects).
const MIXED_POINT_CRS_DETAIL: &str =
    "Collections containing point values with different CRS can not be stored in properties.";

/// Why a value cannot be stored as a property.
pub(crate) enum PropertyInvalidity {
    /// A byte array (ADR-0327), directly or nested in a list at any depth.
    HoldsBytes,
    /// A list of points holding more than one SRID.
    MixedPointCrs,
    /// Any other non-storable shape.
    Other,
}

/// Check that `v` can be stored as a property value: `Ok`, the typed
/// `TypeError` for a byte array (or a list holding one at any depth;
/// ADR-0327) or for a point list holding more than one CRS, or the
/// `InvalidPropertyType` type error for every other non-storable value.
/// Null is storable and transparent: each caller applies its own
/// [`NullPolicy`] at the one map walk, [`admit_props`].
pub(crate) fn check_property_value(v: &Value) -> Result<()> {
    match property_value_invalidity(v) {
        None => Ok(()),
        Some(PropertyInvalidity::HoldsBytes) => Err(Error::type_error(BYTES_PROPERTY_DETAIL)),
        Some(PropertyInvalidity::MixedPointCrs) => Err(Error::type_error(MIXED_POINT_CRS_DETAIL)),
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
/// nested at any depth included), `Some(MixedPointCrs)` when a list of
/// points holds more than one SRID, `Some(Other)` for every other
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
            // The verdict is a property of the whole list, not of the
            // first tripping item: a point-pair SRID clash only records,
            // and the decision runs once after the scan (live 2026.10
            // answers the mixed-CRS detail only for an all-points list —
            // a list whose points clash and also holds another scalar is
            // the generic rejection, whichever position each item sits
            // in).
            let mut kind = None;
            let mut mixed_crs = false;
            for item in xs.iter() {
                // A null item makes the whole list unstorable (Neo4j 22N39),
                // not transparent: `[1, null]` is not a property value.
                if item.is_null() {
                    return Some(PropertyInvalidity::Other);
                }
                // A byte array anywhere inside the item is the byte-array
                // verdict; otherwise the item must be a scalar of the
                // list's one kind.
                if bytes_within(item) {
                    return Some(PropertyInvalidity::HoldsBytes);
                }
                let Some(item_kind) = list_item_kind(item) else {
                    return Some(PropertyInvalidity::Other);
                };
                let Some(expected) = kind else {
                    kind = Some(item_kind);
                    continue;
                };
                if expected == item_kind {
                    continue;
                }
                if expected.differing_point_pair(item_kind) {
                    mixed_crs = true;
                } else {
                    return Some(PropertyInvalidity::Other);
                }
            }
            mixed_crs.then_some(PropertyInvalidity::MixedPointCrs)
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
/// (a null item, a nested list, map, entity or byte array — the null
/// verdict is the walk's, [`property_value_invalidity`]). The vocabulary
/// is spelled one variant per `Value` scalar so a new scalar variant is a
/// compile error here, and the two scalars whose Cypher type splits carry
/// their partition in their kind: a point's SRID (a point list stores
/// under one CRS — the group-key normalization's `Norm::Point { srid, .. }`
/// shape) and a temporal's [`TemporalKind`] (one temporal type per list;
/// live 2026.10 stores `[date(..), date(..)]` and rejects
/// `[date(..), duration(..)]`). `Int` and `Float` are one kind because
/// storage coerces a mixed numeric list to floats.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ListItemKind {
    Numeric,
    Point(i32),
    Temporal(TemporalKind),
    Bool,
    String,
    Uuid,
}

impl ListItemKind {
    /// Whether this and `other` are two points of differing SRIDs — the
    /// one clash the walk records and decides once over the whole list
    /// (across CRS families or 2D/3D within one — live 2026.10). Equal
    /// kinds never reach the question: the caller's fast path takes them.
    fn differing_point_pair(self, other: Self) -> bool {
        matches!((self, other), (Self::Point(a), Self::Point(b)) if a != b)
    }
}

/// The exhaustive classifier: a new `Value` variant fails to compile here
/// until it is classified (null keeps its single home in the walk — the
/// `Value::Null` arm below is a non-scalar, not a verdict).
fn list_item_kind(v: &Value) -> Option<ListItemKind> {
    match v {
        Value::Int(_) | Value::Float(_) => Some(ListItemKind::Numeric),
        Value::Point(p) => Some(ListItemKind::Point(p.srid)),
        Value::Temporal(t) => Some(ListItemKind::Temporal(t.kind())),
        Value::Bool(_) => Some(ListItemKind::Bool),
        Value::String(_) => Some(ListItemKind::String),
        Value::Uuid(_) => Some(ListItemKind::Uuid),
        Value::Null
        | Value::List(_)
        | Value::Map(_)
        | Value::Node(_)
        | Value::Relationship(_)
        | Value::Path(_)
        | Value::Vector { .. }
        | Value::Bytes(_) => None,
    }
}

/// How the one property-map admission walk ([`admit_props`]) treats a null
/// entry — the entry half of the policy only (null itself is storable and
/// transparent, [`check_property_value`]). What a null *source* means and
/// which error text a non-map earns belong to the reader, not here
/// (`SetMapForm` in the write module; the pattern evaluator).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NullPolicy {
    /// Drop the entry: a null is not stored. The policy of the
    /// CREATE/MERGE materialization maps and `SET n = <map>` (full
    /// replacement).
    Drop,
    /// Keep the entry. For `SET n += <map>` the kept null feeds
    /// `merge_properties_*`, which runs each entry through the set verb
    /// and so records the key's *removal*. For a pattern's *match* map
    /// (MATCH, pattern predicates and comprehensions, shortest paths, a
    /// WHERE equality pushed into the map) it is a conjunction of
    /// equalities, and `n.k = null` is never true — a kept null equals
    /// nothing under [`props_all_match`]'s `eq_non_null`, so the element
    /// matches nothing (LABS-1086); dropping it, as storage must,
    /// silently widened `{k: null}` into "any value of k".
    Keep,
}

/// The one property-map admission walk (LABS-818): every entry's value is
/// validated and normalized by [`prepare_property_value`], and a null
/// entry is dropped or kept per the caller's [`NullPolicy`].
pub(crate) fn admit_props(
    map: BTreeMap<String, Value>,
    nulls: NullPolicy,
) -> Result<BTreeMap<String, Value>> {
    let drop_nulls = nulls == NullPolicy::Drop;
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

    /// A point list stores under one SRID; any differing pair — across CRS
    /// families or 2D/3D within one — is the typed rejection (live 2026.10:
    /// "Collections containing point values with different CRS can not be
    /// stored in properties."). A point beside a non-point is a mixed list,
    /// not a mixed CRS, and the verdict is order-independent: a list whose
    /// points clash and also holds another scalar is the generic rejection
    /// whichever position each item sits in.
    #[test]
    fn property_value_invalidity_classifies_mixed_point_crs() {
        let point = || Value::Point(PointValue::cartesian_2d(1.0, 2.0));
        let other_point = || Value::Point(PointValue::wgs84_2d(1.0, 2.0));
        let list = |xs: Vec<Value>| Value::List(xs.into());
        let stores = || Ok(());
        let mixed_crs = || Err(Error::type_error(MIXED_POINT_CRS_DETAIL));
        let mixed_list = || Err(Error::ty("InvalidPropertyType"));
        for (input, expected) in [
            (
                list(vec![
                    point(),
                    Value::Point(PointValue::cartesian_2d(3.0, 4.0)),
                ]),
                stores(),
            ),
            (list(vec![point(), other_point()]), mixed_crs()),
            (
                list(vec![
                    point(),
                    Value::Point(PointValue::cartesian_3d(1.0, 2.0, 3.0)),
                ]),
                mixed_crs(),
            ),
            (
                list(vec![
                    Value::Point(PointValue::wgs84_2d(1.0, 2.0)),
                    Value::Point(PointValue::wgs84_3d(1.0, 2.0, 3.0)),
                ]),
                mixed_crs(),
            ),
            (list(vec![point(), Value::Int(1)]), mixed_list()),
            (
                list(vec![point(), other_point(), Value::Int(1)]),
                mixed_list(),
            ),
            (
                list(vec![Value::Int(1), point(), other_point()]),
                mixed_list(),
            ),
            (
                list(vec![point(), Value::Int(1), other_point()]),
                mixed_list(),
            ),
        ] {
            assert_eq!(check_property_value(&input), expected, "{input:?}");
        }
    }

    /// One temporal type per list: the kind carries the [`TemporalKind`]
    /// partition, so `[date(..), date(..)]` stores and
    /// `[date(..), duration(..)]` is the generic mixed-list rejection
    /// (live 2026.10: same-kind temporal lists store, every cross-kind
    /// mix is the storage TypeError).
    #[test]
    fn property_value_temporal_lists_partition_by_kind() {
        let date = || {
            Value::Temporal(Temporal::CivilDate {
                year: 2020,
                month: 1,
                day: 1,
            })
        };
        let duration = || {
            Value::Temporal(Temporal::Duration {
                months: 1,
                days: 0,
                nanos: 0,
            })
        };
        assert!(check_property_value(&Value::List(vec![date(), date()].into())).is_ok());
        assert!(check_property_value(&Value::List(vec![duration(), duration()].into())).is_ok());
        assert_eq!(
            check_property_value(&Value::List(vec![date(), duration()].into())).unwrap_err(),
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
    /// removes the entry, `Keep` keeps it (a `SET n += {k: null}`
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
            admit_props(map(), NullPolicy::Keep).unwrap(),
            BTreeMap::from([("a".into(), Value::Int(1)), ("b".into(), Value::Null)])
        );
        let bad = BTreeMap::from([("a".to_string(), Value::Null), ("b".to_string(), bytes())]);
        for nulls in [NullPolicy::Drop, NullPolicy::Keep] {
            assert_eq!(
                admit_props(bad.clone(), nulls).unwrap_err(),
                Error::type_error(BYTES_PROPERTY_DETAIL)
            );
        }
    }
}
