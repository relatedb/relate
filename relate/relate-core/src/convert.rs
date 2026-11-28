//! Relate `Value` <-> CypherValue bridge (relate-model).
//!
//! The forward crossing (engine `Value` -> `CypherValue`) is the one
//! iterative walk in [`crate::convert_walk`], shared by both policies —
//! ADR-0462's data-value cap and ADR-0628's plan-wire budget; this module
//! holds the walk's two public front ends, the reverse crossing, and the
//! temporal arm of the mapping.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{FixedOffset, TimeZone};
use relate_model::value::IsoDuration;
use relate_model::wire::ColumnTypeKind;
use relate_model::{CypherValue, IndexMap};

use crate::convert_walk::{value_to_cypher_walk, DepthPolicy};
// The budget const's one public path runs through this module (the plan
// front end below is its consumer); the walk owns its definition.
pub use crate::convert_walk::MAX_PLAN_WIRE_DEPTH;
use crate::temporal::Temporal;
use crate::value::{Node, Path, Relationship, Value};

pub fn value_to_cypher(v: &Value) -> CypherValue {
    // The data policy saturates at its cap (ADR-0462), never errors: the
    // Result channel exists for the plan policy's budget (ADR-0628), so
    // this front end's unwrap states an invariant, never a reachable
    // error.
    value_to_cypher_walk(v, DepthPolicy::Data)
        .expect("the data-value crossing saturates at its cap, never errors")
}

/// The plan-wire form of [`value_to_cypher`] (ADR-0628): the identical
/// mapping under the plan policy — **not** the ADR-0462 data-value cap (a
/// plan tree is engine-produced metadata whose depth scales with the
/// statement's clause count, not a runtime data value that cap governs)
/// but [`MAX_PLAN_WIRE_DEPTH`], the walk-sized budget above. The one
/// conversion every plan-path crossing routes through — the wire
/// envelope's plan document, the formatter's plan-wire composition, the
/// HTTP Query API v2 plan field — never a per-surface re-derivation.
/// Iterative: the crossing holds constant stack on any caller's thread.
///
/// # Errors
///
/// Past [`MAX_PLAN_WIRE_DEPTH`] container levels the crossing fails with
/// the typed [`Error::Capacity`] refusal in the
/// [`crate::CapacityKind::PlanWire`] class — never a silent truncation,
/// never a panic. The error's text states the scope: the statement itself
/// executed; only its plan document is refused.
///
/// ```
/// use relate_core::{plan_value_to_cypher, Value};
/// use relate_model::CypherValue;
///
/// // A plan tree nests as deep as the statement has clauses — past the
/// // ADR-0462 data-value cap — and the crossing keeps every level under
/// // its budget.
/// let mut tree = Value::Int(1);
/// for _ in 0..600 {
///     tree = Value::List(vec![tree].into());
/// }
/// let crossed =
///     plan_value_to_cypher(&tree).expect("600 levels sit far under the budget");
/// let mut node = &crossed;
/// for _ in 0..600 {
///     let CypherValue::List(items) = node else {
///         panic!("every plan-wire level crosses as a list, got {node:?}")
///     };
///     node = &items[0];
/// }
/// assert_eq!(*node, CypherValue::Int(1));
/// ```
pub fn plan_value_to_cypher(v: &Value) -> crate::Result<CypherValue> {
    value_to_cypher_walk(v, DepthPolicy::Plan)
}

/// Classify an engine `Value` into its canonical ADR-0224 kind — the
/// engine-side spelling of `relate_model::wire::column_type_kind`,
/// classifying into the same single-sourced [`ColumnTypeKind`] set.
///
/// `Value` has no `Geometry` variant (that is a `CypherValue`-only kind,
/// `Point` on this side). `Value::Uuid` has no `CypherValue` counterpart —
/// `value_to_cypher` crosses it as `Str` — so it classifies `String`: the
/// wire taxonomy cannot distinguish it, and the `Uuid` kind stays reserved
/// per ADR-0224. `Value::Bytes` stamps ADR-0224's `BINARY`: the column type
/// names the engine value, which the Bolt surface sends as PackStream Bytes
/// (ADR-0327).
pub fn column_type_kind(value: &Value) -> ColumnTypeKind {
    match value {
        Value::Null => ColumnTypeKind::Any,
        Value::Bool(_) => ColumnTypeKind::Boolean,
        Value::Int(_) => ColumnTypeKind::Integer,
        Value::Float(_) => ColumnTypeKind::Float,
        Value::String(_) | Value::Uuid(_) => ColumnTypeKind::String,
        Value::Bytes(_) => ColumnTypeKind::Binary,
        Value::List(_) => ColumnTypeKind::List,
        Value::Map(_) => ColumnTypeKind::Map,
        Value::Node(_) => ColumnTypeKind::Node,
        Value::Relationship(_) => ColumnTypeKind::Relationship,
        Value::Path(_) => ColumnTypeKind::Path,
        Value::Point(_) => ColumnTypeKind::Point,
        Value::Vector { .. } => ColumnTypeKind::Vector,
        Value::Temporal(t) => match t {
            Temporal::Date(_) | Temporal::CivilDate { .. } => ColumnTypeKind::Date,
            Temporal::LocalTime(_) | Temporal::Time { .. } => ColumnTypeKind::Time,
            Temporal::LocalDateTime(_)
            | Temporal::CivilDateTime { .. }
            | Temporal::DateTime { .. }
            | Temporal::CivilZoned(_) => ColumnTypeKind::DateTime,
            Temporal::Duration { .. } => ColumnTypeKind::Duration,
        },
    }
}

/// One canonical ADR-0224 type name for an engine `Value` — the name
/// spelling of [`column_type_kind`], for the engine's
/// `QueryResult::column_types` stamp.
pub fn column_type_name(value: &Value) -> &'static str {
    column_type_kind(value).name()
}

pub fn cypher_to_value(v: &CypherValue) -> Value {
    match v {
        CypherValue::Null => Value::Null,
        CypherValue::Bool(b) => Value::Bool(*b),
        CypherValue::Int(i) => Value::Int(*i),
        CypherValue::Float(f) => Value::Float(*f),
        CypherValue::Str(s) => Value::String(s.as_str().into()),
        CypherValue::List(xs) => {
            Value::List(xs.iter().map(cypher_to_value).collect::<Vec<_>>().into())
        }
        CypherValue::Map(m) => Value::Map(
            m.iter()
                .map(|(k, v)| (k.clone(), cypher_to_value(v)))
                .collect::<BTreeMap<_, _>>()
                .into(),
        ),
        CypherValue::Node {
            id,
            labels,
            properties,
        } => Value::Node(Arc::new(node_from_fields(*id, labels, properties))),
        CypherValue::Relationship {
            id,
            start_id,
            end_id,
            rel_type,
            properties,
        } => Value::Relationship(Arc::new(rel_from_fields(
            *id, *start_id, *end_id, rel_type, properties,
        ))),
        CypherValue::Path {
            nodes,
            relationships,
        } => {
            // `CypherValue::Path` erases its element types
            // (`Vec<CypherValue>`), so a non-entity element is
            // representable. Every producer validates the shape -- the
            // engine builds paths from graph entities and the Arrow
            // typed-JSON decoder rejects bad members -- so a mismatch here
            // is an invariant violation: fail loudly instead of silently
            // dropping the element (the old `filter_map` did).
            let nodes = nodes
                .iter()
                .map(cypher_to_node)
                .collect::<Result<Vec<_>, _>>()
                .expect("CypherValue::Path nodes must be CypherValue::Node");
            let rels = relationships
                .iter()
                .map(cypher_to_relationship)
                .collect::<Result<Vec<_>, _>>()
                .expect("CypherValue::Path relationships must be CypherValue::Relationship");
            Value::Path(Arc::new(Path { nodes, rels }))
        }
        CypherValue::Date(d) => Value::Temporal(Temporal::Date(*d)),
        CypherValue::Time(t, tz) => match tz {
            None => Value::Temporal(Temporal::LocalTime(*t)),
            Some(off) => Value::Temporal(Temporal::Time {
                time: *t,
                offset: off.local_minus_utc(),
            }),
        },
        CypherValue::LocalDateTime(dt) => Value::Temporal(Temporal::LocalDateTime(*dt)),
        CypherValue::DateTime(dt, zone) => {
            let offset = dt.offset().local_minus_utc();
            let utc = dt.naive_utc();
            Value::Temporal(Temporal::DateTime {
                utc,
                offset,
                zone: zone.clone(),
            })
        }
        CypherValue::Duration(d) => Value::Temporal(Temporal::Duration {
            months: d.months,
            days: d.days,
            nanos: d.seconds as i128 * 1_000_000_000 + d.nanoseconds as i128,
        }),
        CypherValue::Point(p) => Value::Point(p.clone()),
        // The packed form crosses as one Arc clone (ADR-0706) — the
        // per-coordinate `cypher_to_value` walk is gone.
        CypherValue::Vector { coord, coords, .. } => Value::vector(*coord, coords.clone()),
        // Documented deviation (mirrors the Uuid arm in `value_to_cypher`):
        // the core `Value` has no Geometry variant, so a Geometry crossing
        // the model -> core bridge degrades to Null. Persisted properties
        // never take this path: the storage codec rejects Geometry before
        // conversion (`reject_geometry` in relate-storage).
        CypherValue::Geometry(_) => Value::Null,
    }
}

/// Node construction from `CypherValue::Node` fields: infallible because
/// the caller's pattern already proved the variant.
fn node_from_fields(
    id: i64,
    labels: &[String],
    properties: &IndexMap<String, CypherValue>,
) -> Node {
    Node {
        id,
        labels: Arc::new(labels.to_vec()),
        properties: crate::PropertyMap::share(
            properties
                .iter()
                .map(|(k, v)| (k.clone(), cypher_to_value(v)))
                .collect::<std::collections::BTreeMap<_, _>>(),
        ),
    }
}

/// Typed conversion of a `CypherValue::Path` node element: errors on any
/// other variant, so a malformed path element cannot be silently dropped.
fn cypher_to_node(v: &CypherValue) -> Result<Node, String> {
    match v {
        CypherValue::Node {
            id,
            labels,
            properties,
        } => Ok(node_from_fields(*id, labels, properties)),
        other => Err(format!(
            "path node element must be NODE, got {}",
            other.type_name()
        )),
    }
}

/// Relationship construction from `CypherValue::Relationship` fields (see
/// [`node_from_fields`]).
fn rel_from_fields(
    id: i64,
    start: i64,
    end: i64,
    rel_type: &str,
    properties: &IndexMap<String, CypherValue>,
) -> Relationship {
    Relationship {
        id,
        start,
        end,
        rel_type: rel_type.to_string(),
        properties: crate::PropertyMap::share(
            properties
                .iter()
                .map(|(k, v)| (k.clone(), cypher_to_value(v)))
                .collect::<std::collections::BTreeMap<_, _>>(),
        ),
    }
}

/// Typed conversion of a `CypherValue::Path` relationship element: errors
/// on any other variant (see [`cypher_to_node`]).
fn cypher_to_relationship(v: &CypherValue) -> Result<Relationship, String> {
    match v {
        CypherValue::Relationship {
            id,
            start_id,
            end_id,
            rel_type,
            properties,
        } => Ok(rel_from_fields(
            *id, *start_id, *end_id, rel_type, properties,
        )),
        other => Err(format!(
            "path relationship element must be RELATIONSHIP, got {}",
            other.type_name()
        )),
    }
}

pub(crate) fn temporal_to_cypher(t: &Temporal) -> CypherValue {
    match t {
        Temporal::Date(d) => CypherValue::Date(*d),
        // Both civil arms cross as the value's own ISO rendering when the
        // year leaves chrono's range — one fallback spelling, the value's,
        // never a bespoke formatter.
        Temporal::CivilDate { year, month, day } => civil_naive_date(*year, *month, *day)
            .map(CypherValue::Date)
            .unwrap_or_else(|| CypherValue::Str(t.iso_string())),
        Temporal::LocalTime(tm) => CypherValue::Time(*tm, None),
        // An offset outside chrono's `FixedOffset` range (any i32 offset is
        // legal in the stored model, and Bolt accepts them) crosses as the
        // value's ISO string — the same stated fallback the `DateTime` arm
        // uses, never a silent loss of the offset.
        Temporal::Time { time, offset } => match FixedOffset::east_opt(*offset) {
            Some(off) => CypherValue::Time(*time, Some(off)),
            None => CypherValue::Str(t.iso_string()),
        },
        Temporal::LocalDateTime(dt) => CypherValue::LocalDateTime(*dt),
        Temporal::CivilDateTime {
            year,
            month,
            day,
            time,
        } => match civil_naive_date(*year, *month, *day) {
            Some(d) => CypherValue::LocalDateTime(d.and_time(*time)),
            None => CypherValue::Str(t.iso_string()),
        },
        // A far-year zoned datetime (ADR-0683) crosses as its ISO
        // rendering — the same stated fallback the other out-of-range
        // civil arms use; the model's DateTime is chrono-bound.
        Temporal::CivilZoned(_) => CypherValue::Str(t.iso_string()),
        Temporal::DateTime { utc, offset, zone } => {
            let Some(off) = FixedOffset::east_opt(*offset) else {
                return CypherValue::Str(t.iso_string());
            };
            let dt = off.from_utc_datetime(utc);
            // The model's second field is the IANA region name it brackets
            // in `Display`; a fixed-offset spelling (`Z`, `+01:00`,
            // `GMT+03:00`) is already carried by the offset and must not
            // render as a second zone suffix.
            CypherValue::DateTime(
                dt,
                crate::temporal::zone_region(zone.as_deref()).map(String::from),
            )
        }
        Temporal::Duration {
            months,
            days,
            nanos,
        } => {
            // The stored sub-day part is an i128 nanosecond count whose second
            // quotient can reach 2^63 (`Temporal::duration_checked` documents
            // and admits that f64-image edge); `as i64` would wrap a positive
            // duration across the sign boundary. The model's `IsoDuration`
            // stores i64 seconds, so a value past that bound crosses as its
            // ISO string — the bridge's stated fallback, like the civil arms'.
            let Ok(seconds) = i64::try_from(nanos / 1_000_000_000) else {
                return CypherValue::Str(t.iso_string());
            };
            let nanoseconds = i64::try_from(nanos % 1_000_000_000)
                .expect("a sub-second remainder is below 10^9 in magnitude");
            CypherValue::Duration(IsoDuration::new(*months, *days, seconds, nanoseconds))
        }
    }
}

/// The civil date as chrono sees it, or `None` when the year does not fit
/// `i32`. `i32::try_from`, never `as`: an extreme civil year wraps into a
/// plausible-looking one (`4294967397` → 101) and would silently answer a
/// wrong date instead of the ISO-string fallback.
fn civil_naive_date(year: i64, month: u32, day: u32) -> Option<chrono::NaiveDate> {
    i32::try_from(year)
        .ok()
        .and_then(|year| chrono::NaiveDate::from_ymd_opt(year, month, day))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;
    fn node(id: i64) -> Node {
        Node {
            id,
            labels: Arc::new(vec!["L".into()]),
            properties: crate::PropertyMap::share(BTreeMap::from([(
                "name".to_string(),
                Value::String("x".into()),
            )])),
        }
    }

    fn rel(id: i64, start: i64, end: i64) -> Relationship {
        Relationship {
            id,
            start,
            end,
            rel_type: "R".into(),
            properties: crate::PropertyMap::share(BTreeMap::from([(
                "w".to_string(),
                Value::Float(0.5),
            )])),
        }
    }

    // The Path arm converts each element directly through the extracted
    // helpers: same output as the generic dispatcher, with no
    // clone-wrap-redispatch.
    #[test]
    fn value_to_cypher_path_matches_direct_conversion() {
        let path = Value::from_path(Path {
            nodes: vec![node(1), node(2)],
            rels: vec![rel(3, 1, 2)],
        });
        let CypherValue::Path {
            nodes,
            relationships,
        } = value_to_cypher(&path)
        else {
            panic!("path did not convert to CypherValue::Path");
        };
        assert_eq!(
            nodes,
            vec![
                value_to_cypher(&Value::from_node(node(1))),
                value_to_cypher(&Value::from_node(node(2))),
            ]
        );
        assert_eq!(
            relationships,
            vec![value_to_cypher(&Value::from_relationship(rel(3, 1, 2)))]
        );
    }

    // B1: an extreme civil year must not wrap into a plausible-looking one —
    // `*year as i32` turned `4294967397` into year 101 and answered a wrong
    // LocalDateTime instead of the ISO-string fallback.
    #[test]
    fn a_civil_year_past_i32_keeps_its_iso_string() {
        let t = crate::temporal::parse_iso("localdatetime", "4294967397-01-01T12:00").unwrap();
        match value_to_cypher(&Value::Temporal(t)) {
            CypherValue::Str(s) => {
                assert!(s.contains("4294967397"), "{s}");
                assert!(!s.contains("0101"), "the year wrapped: {s}");
            }
            other => panic!("expected the ISO-string fallback, got {other:?}"),
        }
        // A civil year that does fit chrono still crosses as a LocalDateTime.
        let small = Temporal::CivilDateTime {
            year: 10_000,
            month: 1,
            day: 1,
            time: chrono::NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
        };
        assert!(
            matches!(
                value_to_cypher(&Value::Temporal(small)),
                CypherValue::LocalDateTime(_)
            ),
            "a representable civil year must keep the typed form"
        );
        // C6: the civil-date twin uses the value's own ISO rendering, never a
        // bespoke spelling (`+4294967397-01-01`, not `4294967397-01-01`).
        let civil_date = Temporal::CivilDate {
            year: 4_294_967_397,
            month: 1,
            day: 1,
        };
        assert_eq!(
            value_to_cypher(&Value::Temporal(civil_date.clone())),
            CypherValue::Str(civil_date.iso_string())
        );
    }

    /// RB6: a stored offset outside chrono's `FixedOffset` range (any i32 is
    /// legal in the stored model, and Bolt accepts them) crosses as the
    /// value's ISO string — the `DateTime` arm's stated fallback, never a
    /// silently offset-less local time.
    #[test]
    fn a_time_offset_past_fixed_offset_range_keeps_its_iso_string() {
        let time = chrono::NaiveTime::from_hms_opt(12, 0, 0).unwrap();
        let unrepresentable = Temporal::Time {
            time,
            offset: 100_000,
        };
        assert_eq!(
            value_to_cypher(&Value::Temporal(unrepresentable.clone())),
            CypherValue::Str(unrepresentable.iso_string())
        );
        // An in-range offset keeps the typed Time carrying it.
        let representable = Temporal::Time { time, offset: 3600 };
        assert!(
            matches!(
                value_to_cypher(&Value::Temporal(representable)),
                CypherValue::Time(_, Some(_))
            ),
            "an in-range offset must keep the typed Time"
        );
    }

    // B2: the sub-day part's second quotient can be 2^63; `as i64` wrapped a
    // positive duration across the sign boundary into i64::MIN.
    #[test]
    fn a_duration_past_i64_seconds_keeps_its_iso_string() {
        let nanos = (i128::from(i64::MAX) + 1) * 1_000_000_000;
        let t = Temporal::Duration {
            months: 0,
            days: 0,
            nanos,
        };
        match value_to_cypher(&Value::Temporal(t)) {
            CypherValue::Str(s) => assert!(s.starts_with('P'), "{s}"),
            other => panic!("expected the ISO-string fallback, got {other:?}"),
        }
        // An in-range duration still crosses as a Duration.
        let ok = Temporal::Duration {
            months: 0,
            days: 1,
            nanos: 1_000_000_000,
        };
        assert!(matches!(
            value_to_cypher(&Value::Temporal(ok)),
            CypherValue::Duration(_)
        ));
    }

    // A well-formed CypherValue::Path round-trips, and the typed element
    // helpers error on a mismatched element instead of silently dropping
    // it (the old `filter_map` behavior).
    #[test]
    fn cypher_to_value_path_roundtrip_and_typed_element_errors() {
        let path = Value::from_path(Path {
            nodes: vec![node(1), node(2)],
            rels: vec![rel(3, 1, 2)],
        });
        let cypher = value_to_cypher(&path);
        assert_eq!(cypher_to_value(&cypher), path);

        let bad = cypher_to_node(&CypherValue::Int(1)).expect_err("non-node accepted");
        assert!(bad.contains("INTEGER"), "unexpected error: {bad}");
        let bad = cypher_to_relationship(&cypher).expect_err("path accepted as relationship");
        assert!(bad.contains("PATH"), "unexpected error: {bad}");
    }

    // ADR-0628: a container chain deeper than MAX_VALUE_DEPTH — a plan
    // tree's own shape — converts intact through the plan-wire crossing
    // (501 levels, far under the plan budget), while the data crossing
    // saturates at the ADR-0462 cap. Both sides of the contrast pinned on
    // one value.
    #[test]
    fn a_past_cap_chain_converts_intact_as_plan_wire_and_nulls_as_data() {
        // Built raw, the way the plan producer builds its tree: the checked
        // constructors reject this depth by design (ADR-0462), and a plan
        // tree never goes through them.
        let mut deep = Value::Int(7);
        for _ in 0..=crate::MAX_VALUE_DEPTH {
            deep = Value::List(vec![deep].into());
        }

        // The plan-wire crossing keeps every one of the 501 levels.
        let plan =
            plan_value_to_cypher(&deep).expect("501 levels sit far under the plan-wire budget");
        let mut node = &plan;
        for _ in 0..=crate::MAX_VALUE_DEPTH {
            let CypherValue::List(items) = node else {
                panic!("every plan-wire level crosses as a list, got {node:?}");
            };
            assert_eq!(items.len(), 1, "each level keeps its single member");
            node = &items[0];
        }
        assert_eq!(*node, CypherValue::Int(7), "the innermost scalar survives");

        // The data crossing saturates: the 501st container crosses as Null
        // inside the capped 500th (ADR-0462's valve).
        let data = value_to_cypher(&deep);
        let mut node = &data;
        for _ in 1..crate::MAX_VALUE_DEPTH {
            let CypherValue::List(items) = node else {
                panic!("the data crossing keeps the 500 capped levels, got {node:?}");
            };
            node = &items[0];
        }
        let CypherValue::List(items) = node else {
            panic!("the 500th level stays a list, got {node:?}");
        };
        assert_eq!(
            items[0],
            CypherValue::Null,
            "the past-cap container crosses as Null (ADR-0462)"
        );
    }

    // ADR-0628's stack-safe budget (the rust round's final shape): exactly
    // MAX_PLAN_WIRE_DEPTH container levels cross intact, one more refuses
    // with the typed capacity error naming the budget — never a silent
    // truncation, never a panic. The crossing is iterative (constant
    // stack), and the crossed values' recursive drops — the by-design
    // per-level frames the budget still admits — are the walk-sized
    // ~100 KB a 1 000-level value costs, held by any ordinary thread.
    #[test]
    fn the_plan_wire_budget_pins_its_boundary() {
        let build =
            |levels: u32| (0..levels).fold(Value::Int(7), |v, _| Value::List(vec![v].into()));
        let at = plan_value_to_cypher(&build(crate::MAX_PLAN_WIRE_DEPTH));
        let past = plan_value_to_cypher(&build(crate::MAX_PLAN_WIRE_DEPTH + 1));
        assert!(
            at.is_ok(),
            "exactly the budget's levels cross intact: {at:?}"
        );
        match past {
            Err(Error::Capacity {
                kind: crate::CapacityKind::PlanWire,
                limit,
                ..
            }) => {
                assert_eq!(
                    limit,
                    u64::from(crate::MAX_PLAN_WIRE_DEPTH),
                    "the refusal names the budget"
                );
            }
            other => panic!(
                "one level past the budget refuses with the typed plan-wire \
                 capacity class, got {other:?}"
            ),
        }
    }
}
