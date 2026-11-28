//! Infer an Arrow schema from CypherValue columns.
//!
//! There are two layers here:
//!
//! * [`infer_type`] / [`infer_schema`] are the legacy **single-value / single
//!   sample-row** helpers (kept for API stability). They cannot see a whole
//!   column, so they can neither promote `Int`+`Float` to `Float64` nor pick a
//!   real Arrow type for a temporal/spatial/vector value that only appears
//!   below row 0.
//!
//! * [`ColType`] + [`infer_column_type`] are the **full-column** inference used
//!   by [`crate::record_batch::result_to_record_batch`]. Every present value in
//!   a column is classified and the classifications are merged, so:
//!     - a column whose first value is `Null` is still typed by later values,
//!     - a column mixing `Int` and `Float` is promoted to `Float64` only when
//!       all integers are exactly representable (no present value is dropped), and
//!     - temporal / spatial / vector / node / relationship columns map to real
//!       Arrow types instead of `Utf8` JSON text.
//!
//!   Genuinely incompatible mixes, homogeneous composites, and out-of-range
//!   temporal values use one absorbing [`ColType::TypedJson`] fallback with a
//!   **self-describing** Utf8 encoding (see [`crate::record_batch`]).

use arrow_schema::{DataType, Field, Schema};
use chrono::FixedOffset;
use indexmap::IndexMap;
use relate_model::{CypherValue, VectorCoordKind};
use std::sync::Arc;

/// Infer an Arrow DataType from a single CypherValue (legacy, per-value).
///
/// Complex/temporal/spatial values collapse to `Utf8` here because a single
/// value carries no column context; the full-column inference in
/// [`infer_column_type`] is what actually drives
/// [`crate::record_batch::result_to_record_batch`].
pub fn infer_type(value: &CypherValue) -> DataType {
    match value {
        CypherValue::Bool(_) => DataType::Boolean,
        CypherValue::Int(_) => DataType::Int64,
        CypherValue::Float(_) => DataType::Float64,
        CypherValue::Str(_) => DataType::Utf8,
        CypherValue::Null => DataType::Utf8, // default null columns to string
        _ => DataType::Utf8,                 // complex types serialized as JSON strings
    }
}

/// Infer an Arrow schema from column names and a single sample row (legacy).
///
/// Prefer routing whole result sets through
/// [`crate::record_batch::result_to_record_batch`], which does full-column
/// inference via [`infer_column_type`].
pub fn infer_schema(columns: &[String], sample_row: &[CypherValue]) -> Arc<Schema> {
    let fields: Vec<Field> = columns
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let dt = if i < sample_row.len() {
                infer_type(&sample_row[i])
            } else {
                DataType::Utf8
            };
            Field::new(name, dt, true)
        })
        .collect();
    Arc::new(Schema::new(fields))
}

/// Create a schema with all Utf8 columns (fallback when no sample data available).
pub fn string_schema(columns: &[String]) -> Arc<Schema> {
    let fields: Vec<Field> = columns
        .iter()
        .map(|name| Field::new(name, DataType::Utf8, true))
        .collect();
    Arc::new(Schema::new(fields))
}

/// The logical type inferred for a whole column, one step above an Arrow
/// `DataType`. It carries just enough structure to (a) drive array building and
/// (b) be merged across rows. Two spatial subtypes (`Point` / `Geometry`) both
/// map to [`ColType::Spatial`] because both encode to WKB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColType {
    /// Every value in the column is `Null` (or the column is absent). Rendered
    /// as an all-null `Utf8` column.
    Null,
    Bool,
    Int,
    /// `Float`, or an exact `Int`+`Float` mix. Ints are widened to `f64`.
    Float,
    Str,
    /// `Date` -> Arrow `Date32` (days since the Unix epoch).
    Date,
    /// Unzoned `Time` -> Arrow `Time64(Nanosecond)` (nanoseconds since midnight).
    /// An offset-bearing Time instead uses [`ColType::TypedJson`].
    Time,
    /// DateTimes with one shared fixed offset and no zone names -> Arrow
    /// `Timestamp(Nanosecond, <tz>)` (UTC instant).
    DateTime,
    /// `LocalDateTime` -> Arrow `Timestamp(Nanosecond, None)`.
    LocalDateTime,
    /// `Duration` -> Arrow `Interval(MonthDayNano)`.
    Duration,
    /// `Point` or `Geometry` -> Arrow `Binary` (WKB) with GeoArrow field
    /// metadata.
    Spatial,
    /// `Vector` -> Arrow `FixedSizeList<primitive>` of a fixed coordinate kind
    /// and dimension.
    Vector {
        coord: VectorCoordKind,
        dim: usize,
    },
    /// `Node` -> Arrow `Struct { id, labels, properties }`.
    Node,
    /// `Relationship` -> Arrow `Struct { id, type, start_node, end_node,
    /// properties }`.
    Relationship,
    /// Absorbing typed-json-v1 Utf8 fallback for composites (`List` / `Map` /
    /// `Path`), incompatible kinds (including differently shaped vectors),
    /// offset-bearing Times, 3D Points, inexact Int+Float mixes,
    /// heterogeneous/named-zone DateTimes, and
    /// temporal/duration values outside native Arrow ranges. Preserves graph
    /// identity even when nested inside user maps and lists.
    TypedJson,
}

/// Classify a single present value into its column kind. `Null` classifies as
/// [`ColType::Null`] (the merge identity).
///
/// Temporal/duration values that cannot be represented in the target Arrow
/// type (a `DateTime`/`LocalDateTime` outside the `Timestamp(Nanosecond)`
/// range of ~1677..2262, or a `Duration` whose month/day count overflows `i32`
/// or whose seconds overflow when converted to nanoseconds) classify as
/// [`ColType::TypedJson`] instead, so they land in the self-describing fallback
/// with their complete typed payload preserved rather than being silently
/// truncated. DateTimes with a zone name or a non-minute offset also fall back;
/// [`infer_column_type`] checks for differing fixed offsets across the column.
pub fn value_kind(v: &CypherValue) -> ColType {
    match v {
        CypherValue::Null => ColType::Null,
        CypherValue::Bool(_) => ColType::Bool,
        CypherValue::Int(_) => ColType::Int,
        CypherValue::Float(_) => ColType::Float,
        CypherValue::Str(_) => ColType::Str,
        CypherValue::Date(_) => ColType::Date,
        CypherValue::Time(_, Some(_)) => ColType::TypedJson,
        CypherValue::Time(_, None) => ColType::Time,
        CypherValue::DateTime(dt, name) => {
            if dt.timestamp_nanos_opt().is_some()
                && dt.offset().local_minus_utc() % 60 == 0
                && name.is_none()
            {
                ColType::DateTime
            } else {
                ColType::TypedJson
            }
        }
        CypherValue::LocalDateTime(ndt) => {
            if ndt.and_utc().timestamp_nanos_opt().is_some() {
                ColType::LocalDateTime
            } else {
                ColType::TypedJson
            }
        }
        CypherValue::Duration(d) => {
            let months_ok = i32::try_from(d.months).is_ok();
            let days_ok = i32::try_from(d.days).is_ok();
            let nanos_ok = d
                .seconds
                .checked_mul(1_000_000_000)
                .and_then(|s| s.checked_add(d.nanoseconds))
                .is_some();
            if months_ok && days_ok && nanos_ok {
                ColType::Duration
            } else {
                ColType::TypedJson
            }
        }
        CypherValue::Point(p) if p.z.is_some() => ColType::TypedJson,
        CypherValue::Point(_) | CypherValue::Geometry(_) => ColType::Spatial,
        CypherValue::Vector { coord, coords } => ColType::Vector {
            coord: *coord,
            dim: coords.len(),
        },
        CypherValue::Node { .. } => ColType::Node,
        CypherValue::Relationship { .. } => ColType::Relationship,
        CypherValue::List(_) | CypherValue::Map(_) | CypherValue::Path { .. } => ColType::TypedJson,
    }
}

/// Merge two column kinds into the narrowest kind that can hold both, without
/// ever losing a present value:
/// * `Null` is the identity element.
/// * `Int` + `Float` -> `Float` (the full-column inference checks exactness).
/// * two `Vector`s merge only if their coordinate kind **and** dimension match.
/// * `TypedJson` absorbs every kind, including `Null`.
/// * anything else that differs -> `TypedJson` (self-describing tagged fallback).
pub fn merge(a: ColType, b: ColType) -> ColType {
    use ColType::*;
    match (a, b) {
        (TypedJson, _) | (_, TypedJson) => TypedJson,
        (Null, x) | (x, Null) => x,
        (Int, Int) => Int,
        (Int, Float) | (Float, Int) | (Float, Float) => Float,
        (Vector { coord: c1, dim: d1 }, Vector { coord: c2, dim: d2 }) => {
            if c1 == c2 && d1 == d2 {
                Vector { coord: c1, dim: d1 }
            } else {
                TypedJson
            }
        }
        (x, y) if x == y => x,
        _ => TypedJson,
    }
}

/// Full-column inference: classify and merge **every** present value in the
/// column named `col_name`. A column that is entirely `Null`/absent yields
/// [`ColType::Null`]. A homogeneous `Map` or an incompatible `Int`/`Str`
/// mix yields the same absorbing [`ColType::TypedJson`] fallback. Native
/// DateTimes must also have the same minute-aligned fixed offset on every
/// non-null row and no named-zone hint; otherwise each row needs typed JSON to
/// retain its own offset and display name. An `Int`+`Float` column uses native
/// `Float64` only if all its integers survive conversion to `f64` exactly.
///
/// ```
/// use relate_arrow::schema::{infer_column_type, ColType};
/// use relate_model::CypherValue;
/// use indexmap::IndexMap;
///
/// let map = CypherValue::Map(IndexMap::from([
///     ("name".into(), CypherValue::Str("Ada".into())),
/// ]));
/// let map_rows = vec![IndexMap::from([("value".into(), map)])];
/// assert_eq!(infer_column_type("value", &map_rows), ColType::TypedJson);
///
/// let mixed_rows = vec![
///     IndexMap::from([("value".into(), CypherValue::Int(1))]),
///     IndexMap::from([("value".into(), CypherValue::Str("two".into()))]),
/// ];
/// assert_eq!(infer_column_type("value", &mixed_rows), ColType::TypedJson);
/// ```
pub fn infer_column_type(col_name: &str, rows: &[IndexMap<String, CypherValue>]) -> ColType {
    infer_column(col_name, rows).kind
}

/// Writer-only inference metadata. A native DateTime kind always has a shared
/// offset; other kinds never carry one.
pub(crate) struct InferredColumn {
    pub(crate) kind: ColType,
    pub(crate) datetime_offset: Option<FixedOffset>,
}

pub(crate) fn infer_column(
    col_name: &str,
    rows: &[IndexMap<String, CypherValue>],
) -> InferredColumn {
    let mut kind = ColType::Null;
    let mut datetime_offset = None;
    let mut has_inexact_int = false;
    for row in rows {
        let value = row.get(col_name);
        if let Some(CypherValue::Int(i)) = value {
            // Compare through i128: f64 -> i64 saturates at the bounds, so
            // i64::MAX would otherwise look exact even though it rounds up.
            has_inexact_int |= ((*i as f64) as i128) != (*i as i128);
        }
        let vk = match value {
            Some(v) => value_kind(v),
            None => ColType::Null,
        };
        kind = merge(kind, vk);
        if kind == ColType::Float && has_inexact_int {
            kind = ColType::TypedJson;
        }
        if kind == ColType::DateTime {
            if let Some(CypherValue::DateTime(dt, None)) = value {
                let offset = *dt.offset();
                if datetime_offset.is_some_and(|first| first != offset) {
                    kind = ColType::TypedJson;
                } else {
                    datetime_offset = Some(offset);
                }
            }
        }
        if kind != ColType::DateTime {
            datetime_offset = None;
        }
        if kind == ColType::TypedJson {
            break; // TypedJson absorbs everything; no need to keep scanning.
        }
    }
    InferredColumn {
        kind,
        datetime_offset,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use relate_model::value::IsoDuration;
    use relate_model::VectorCoords;

    #[test]
    #[allow(clippy::approx_constant)]
    fn test_infer_type() {
        assert_eq!(infer_type(&CypherValue::Bool(true)), DataType::Boolean);
        assert_eq!(infer_type(&CypherValue::Int(42)), DataType::Int64);
        assert_eq!(infer_type(&CypherValue::Float(3.14)), DataType::Float64);
        assert_eq!(
            infer_type(&CypherValue::Str("hello".into())),
            DataType::Utf8
        );
        assert_eq!(infer_type(&CypherValue::Null), DataType::Utf8);
    }

    #[test]
    fn test_infer_schema() {
        let cols = vec!["name".to_string(), "age".to_string(), "active".to_string()];
        let sample = vec![
            CypherValue::Str("Alice".into()),
            CypherValue::Int(30),
            CypherValue::Bool(true),
        ];
        let schema = infer_schema(&cols, &sample);
        assert_eq!(schema.fields().len(), 3);
        assert_eq!(schema.field(0).data_type(), &DataType::Utf8);
        assert_eq!(schema.field(1).data_type(), &DataType::Int64);
        assert_eq!(schema.field(2).data_type(), &DataType::Boolean);
    }

    #[test]
    fn test_string_schema() {
        let cols = vec!["a".to_string(), "b".to_string()];
        let schema = string_schema(&cols);
        assert_eq!(schema.fields().len(), 2);
        assert!(schema
            .fields()
            .iter()
            .all(|f| f.data_type() == &DataType::Utf8));
    }

    fn rows_of(col: &str, vals: Vec<CypherValue>) -> Vec<IndexMap<String, CypherValue>> {
        vals.into_iter()
            .map(|v| {
                let mut m = IndexMap::new();
                m.insert(col.to_string(), v);
                m
            })
            .collect()
    }

    #[test]
    fn null_first_does_not_force_null_column() {
        // Column: [Null, Int, Int] -> Int (row-0 sampling would have said Utf8).
        let rows = rows_of(
            "x",
            vec![CypherValue::Null, CypherValue::Int(1), CypherValue::Int(2)],
        );
        assert_eq!(infer_column_type("x", &rows), ColType::Int);
    }

    #[test]
    fn int_then_float_promotes_to_float() {
        let rows = rows_of(
            "x",
            vec![
                CypherValue::Int(1),
                CypherValue::Float(2.5),
                CypherValue::Int(3),
            ],
        );
        assert_eq!(infer_column_type("x", &rows), ColType::Float);
    }

    #[test]
    fn inexact_integer_mixes_fall_back_in_both_orders() {
        for i in [9_007_199_254_740_993, -9_007_199_254_740_993, i64::MAX] {
            let int = CypherValue::Int(i);
            let float = CypherValue::Float(2.5);
            assert_eq!(
                infer_column_type("x", &rows_of("x", vec![int.clone()])),
                ColType::Int
            );
            for values in [
                vec![CypherValue::Null, int.clone(), float.clone()],
                vec![float.clone(), CypherValue::Null, int],
            ] {
                assert_eq!(
                    infer_column_type("x", &rows_of("x", values)),
                    ColType::TypedJson
                );
            }
        }
        for i in [i64::MIN, 9_007_199_254_740_994] {
            assert_eq!(
                infer_column_type(
                    "x",
                    &rows_of("x", vec![CypherValue::Float(2.5), CypherValue::Int(i)])
                ),
                ColType::Float
            );
        }
    }

    #[test]
    fn incompatible_mix_is_typed_json() {
        let rows = rows_of(
            "x",
            vec![CypherValue::Int(1), CypherValue::Str("two".into())],
        );
        assert_eq!(infer_column_type("x", &rows), ColType::TypedJson);
    }

    #[test]
    fn homogeneous_composites_use_absorbing_typed_json() {
        let node = CypherValue::Node {
            id: 1,
            labels: vec![],
            properties: IndexMap::new(),
        };
        for composite in [
            CypherValue::List(vec![CypherValue::Int(1)]),
            CypherValue::Map(IndexMap::from([("x".into(), CypherValue::Int(1))])),
            CypherValue::Path {
                nodes: vec![node],
                relationships: vec![],
            },
        ] {
            assert_eq!(value_kind(&composite), ColType::TypedJson);
            let rows = rows_of(
                "x",
                vec![
                    CypherValue::Null,
                    composite.clone(),
                    composite,
                    CypherValue::Int(2),
                ],
            );
            assert_eq!(infer_column_type("x", &rows), ColType::TypedJson);
        }
        assert_eq!(merge(ColType::Null, ColType::Int), ColType::Int);
        assert_eq!(merge(ColType::Int, ColType::Null), ColType::Int);
        assert_eq!(merge(ColType::Int, ColType::Float), ColType::Float);
        assert_eq!(merge(ColType::Float, ColType::Int), ColType::Float);
        for kind in [ColType::Null, ColType::Int, ColType::Float, ColType::Node] {
            assert_eq!(merge(ColType::TypedJson, kind), ColType::TypedJson);
            assert_eq!(merge(kind, ColType::TypedJson), ColType::TypedJson);
        }
        assert_eq!(
            infer_column_type(
                "x",
                &rows_of(
                    "x",
                    vec![
                        CypherValue::Int(1),
                        CypherValue::Str("two".into()),
                        CypherValue::Null,
                        CypherValue::Int(3),
                    ],
                ),
            ),
            ColType::TypedJson
        );
    }

    #[test]
    fn out_of_native_range_temporals_use_typed_json() {
        let ancient = chrono::DateTime::parse_from_rfc3339("1500-01-01T12:34:56+02:00").unwrap();
        let outside = [
            CypherValue::DateTime(ancient, None),
            CypherValue::LocalDateTime(ancient.naive_local()),
            CypherValue::Duration(IsoDuration::new(i32::MAX as i64 + 1, 0, 0, 0)),
            CypherValue::Duration(IsoDuration::new(0, i32::MIN as i64 - 1, 0, 0)),
            CypherValue::Duration(IsoDuration::new(0, 0, i64::MAX, 0)),
        ];
        for value in outside {
            assert_eq!(value_kind(&value), ColType::TypedJson);
            assert_eq!(
                infer_column_type(
                    "x",
                    &rows_of("x", vec![CypherValue::Null, value, CypherValue::Int(1)]),
                ),
                ColType::TypedJson
            );
        }
    }

    #[test]
    fn zoned_time_absorbs_unzoned_time_in_either_order() {
        let t = chrono::NaiveTime::from_hms_nano_opt(12, 34, 56, 789).unwrap();
        let plain = CypherValue::Time(t, None);
        let zoned = CypherValue::Time(t, Some(chrono::FixedOffset::east_opt(9000).unwrap()));
        assert_eq!(value_kind(&plain), ColType::Time);
        assert_eq!(value_kind(&zoned), ColType::TypedJson);
        for values in [
            vec![plain.clone(), CypherValue::Null, zoned.clone()],
            vec![zoned.clone(), plain.clone()],
        ] {
            assert_eq!(
                infer_column_type("x", &rows_of("x", values)),
                ColType::TypedJson
            );
        }
    }

    #[test]
    fn native_datetime_requires_one_unnamed_minute_aligned_offset_across_all_rows() {
        let base = chrono::DateTime::parse_from_rfc3339("2024-03-14T12:34:56+02:30").unwrap();
        let other = chrono::DateTime::parse_from_rfc3339("2024-03-14T12:34:56-05:00").unwrap();
        let subminute = base.with_timezone(&chrono::FixedOffset::east_opt(1).unwrap());
        let plain = CypherValue::DateTime(base, None);
        let differing = CypherValue::DateTime(other, None);
        let named = CypherValue::DateTime(base, Some("Europe/Stockholm".into()));
        assert_eq!(value_kind(&plain), ColType::DateTime);
        assert_eq!(value_kind(&named), ColType::TypedJson);
        assert_eq!(
            value_kind(&CypherValue::DateTime(subminute, None)),
            ColType::TypedJson
        );
        assert_eq!(
            infer_column_type(
                "x",
                &rows_of("x", vec![CypherValue::Null, plain.clone(), plain.clone()]),
            ),
            ColType::DateTime
        );
        for values in [
            vec![plain.clone(), CypherValue::Null, differing.clone()],
            vec![differing, plain.clone()],
            vec![named.clone()],
            vec![plain.clone(), named],
            vec![plain, CypherValue::DateTime(subminute, None)],
        ] {
            assert_eq!(
                infer_column_type("x", &rows_of("x", values)),
                ColType::TypedJson
            );
        }
    }

    #[test]
    fn inferred_datetime_offset_is_present_only_for_native_columns() {
        let plus = chrono::DateTime::parse_from_rfc3339("2024-03-14T12:34:56+02:30").unwrap();
        let minus = chrono::DateTime::parse_from_rfc3339("2024-03-14T12:34:56-05:00").unwrap();
        let mut native = rows_of(
            "dt",
            vec![CypherValue::Null, CypherValue::DateTime(plus, None)],
        );
        native.insert(0, IndexMap::new()); // Absent rows do not supply an offset.
        native.push(IndexMap::from([(
            "dt".into(),
            CypherValue::DateTime(plus, None),
        )]));
        let inferred = infer_column("dt", &native);
        assert_eq!(inferred.kind, ColType::DateTime);
        assert_eq!(inferred.datetime_offset, Some(*plus.offset()));
        assert_eq!(infer_column_type("dt", &native), inferred.kind);

        for values in [
            vec![
                CypherValue::DateTime(plus, None),
                CypherValue::DateTime(minus, None),
            ],
            vec![CypherValue::DateTime(plus, Some("Europe/Stockholm".into()))],
            vec![CypherValue::DateTime(plus, None), CypherValue::Int(1)],
            vec![CypherValue::DateTime(
                plus.with_timezone(&chrono::FixedOffset::east_opt(1).unwrap()),
                None,
            )],
        ] {
            let inferred = infer_column("dt", &rows_of("dt", values));
            assert_eq!(inferred.kind, ColType::TypedJson);
            assert_eq!(inferred.datetime_offset, None);
        }
        let inferred = infer_column("dt", &rows_of("dt", vec![CypherValue::Null]));
        assert_eq!(inferred.kind, ColType::Null);
        assert_eq!(inferred.datetime_offset, None);
    }

    /// The one FLOAT32 vector builder the two shape tests share.
    fn f32_vector(xs: Vec<f64>) -> CypherValue {
        CypherValue::vector(
            VectorCoordKind::Float32,
            VectorCoords::from_f32(xs.into_iter().map(|x| x as f32).collect::<Vec<_>>()),
        )
    }

    #[test]
    fn vectors_same_shape_merge() {
        let v = f32_vector;
        let rows = rows_of("x", vec![v(vec![1.0, 2.0]), v(vec![3.0, 4.0])]);
        assert_eq!(
            infer_column_type("x", &rows),
            ColType::Vector {
                coord: VectorCoordKind::Float32,
                dim: 2
            }
        );
    }

    #[test]
    fn vectors_different_dim_are_typed_json() {
        let v = f32_vector;
        let rows = rows_of("x", vec![v(vec![1.0, 2.0]), v(vec![3.0, 4.0, 5.0])]);
        assert_eq!(infer_column_type("x", &rows), ColType::TypedJson);
    }

    #[test]
    fn spatial_point_and_geometry_merge() {
        use relate_model::value::{GeometryValue, PointValue};
        let rows = rows_of(
            "g",
            vec![
                CypherValue::Point(PointValue::cartesian_2d(1.0, 2.0)),
                CypherValue::Geometry(GeometryValue::LineString {
                    coordinates: vec![[0.0, 0.0], [1.0, 1.0]],
                    srid: 7203,
                }),
            ],
        );
        assert_eq!(infer_column_type("g", &rows), ColType::Spatial);
    }

    #[test]
    fn three_dimensional_point_absorbs_spatial_and_null_rows() {
        use relate_model::value::{GeometryValue, PointValue};
        let point_3d = CypherValue::Point(PointValue::cartesian_3d(1.0, 2.0, 3.0));
        assert_eq!(value_kind(&point_3d), ColType::TypedJson);
        for values in [
            vec![point_3d.clone()],
            vec![
                CypherValue::Point(PointValue::cartesian_2d(1.0, 2.0)),
                point_3d.clone(),
            ],
            vec![
                point_3d,
                CypherValue::Null,
                CypherValue::Geometry(GeometryValue::LineString {
                    coordinates: vec![[1.0, 2.0]],
                    srid: 7203,
                }),
            ],
        ] {
            assert_eq!(
                infer_column_type("g", &rows_of("g", values)),
                ColType::TypedJson
            );
        }
    }

    #[test]
    fn all_null_column_is_null_kind() {
        let rows = rows_of("x", vec![CypherValue::Null, CypherValue::Null]);
        assert_eq!(infer_column_type("x", &rows), ColType::Null);
    }
}
