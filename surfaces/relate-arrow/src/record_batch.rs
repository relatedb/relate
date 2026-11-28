//! Convert CypherValue result sets to Arrow `RecordBatch` with real Arrow types.
//!
//! # Schema mapping
//!
//! Schema inference is **full-column** (see [`crate::schema::infer_column_type`]):
//! every present value in a column is classified and merged, so a column whose
//! first value is `Null` is still typed by later rows, and no present value is
//! ever dropped to `None`.
//!
//! | [`CypherValue`] | [`crate::schema::ColType`] | Arrow type |
//! |---|---|---|
//! | `Null` (whole column) | `Null` | `Utf8` (all null) |
//! | `Bool` | `Bool` | `Boolean` |
//! | `Int` | `Int` | `Int64` |
//! | `Float`, or exactly representable `Int`+`Float` | `Float` | `Float64` (ints widened) |
//! | `Str` | `Str` | `Utf8` (verbatim) |
//! | `Date` | `Date` | `Date32` (days since epoch) |
//! | Unzoned `Time` | `Time` | `Time64(Nanosecond)` (ns since midnight) |
//! | Offset-bearing `Time` | `TypedJson` | `Utf8` recursive typed JSON (offset retained) |
//! | `DateTime` with one shared fixed offset and no zone name | `DateTime` | `Timestamp(Nanosecond, tz)` (UTC instant) |
//! | `DateTime` with heterogeneous offsets or a zone name | `TypedJson` | `Utf8` recursive typed JSON (per-row offset/name retained) |
//! | `LocalDateTime` | `LocalDateTime` | `Timestamp(Nanosecond, None)` |
//! | `Duration` | `Duration` | `Interval(MonthDayNano)` |
//! | 2D `Point` / `Geometry` | `Spatial` | `Binary` (WKB) + GeoArrow field metadata |
//! | `Vector` | `Vector{coord,dim}` | `FixedSizeList<primitive>` (Int8/16/32/64, Float32/64; FLOAT16/BFLOAT16 widen to Float32) |
//! | `Node` | `Node` | `Struct{id:Int64, labels:List<Utf8>, properties:Utf8(json)}` |
//! | `Relationship` | `Relationship` | `Struct{id, type, start_node, end_node, properties}` |
//! | 3D `Point`, inexact `Int`+`Float`, `List`/`Map`/`Path`, incompatible mix, or out-of-range temporal/duration | `TypedJson` | `Utf8` recursive typed JSON (`relate.encoding=typed-json-v1`) |
//!
//! This Utf8 fallback recursively tags every value, preserving graph identity
//! without promoting lookalike ordinary maps. See ADR-0088.
//! Timestamps and durations outside the native Arrow ranges use this lossless
//! typed-JSON encoding instead of being truncated or replaced with defaults.

use std::collections::HashMap;
use std::sync::Arc;

use arrow::array::ArrayRef;
use arrow::buffer::NullBuffer;
use arrow::error::ArrowError;
use arrow_array::builder::{
    Float32Builder, Float64Builder, Int16Builder, Int32Builder, Int64Builder, Int8Builder,
    ListBuilder, StringBuilder,
};
use arrow_array::types::IntervalMonthDayNano;
use arrow_array::{
    Array, BooleanArray, Date32Array, FixedSizeListArray, Float64Array, Int64Array,
    IntervalMonthDayNanoArray, RecordBatch, StringArray, StructArray, Time64NanosecondArray,
    TimestampNanosecondArray,
};
use arrow_schema::{DataType, Field, Fields, Schema};
use chrono::{FixedOffset, NaiveDate, Timelike};
use indexmap::IndexMap;
use relate_model::{wire, Coord, CypherValue, GeometryValue, VectorCoordKind};

use crate::geo_encoding::{geometries_to_wkb_array, MAX_GEOMETRY_DEPTH};
use crate::schema::{self, ColType};
use crate::typed_json;

/// GeoArrow WKB extension name, set on the field of a spatial (`Binary`) column.
pub const GEOARROW_EXTENSION_NAME: &str = "geoarrow.wkb";
/// Field-metadata key recording the SRID of a spatial column (for decode).
pub const RELATE_SRID_KEY: &str = "relate.srid";
/// Field-metadata key for typed composite Utf8 columns.
pub const RELATE_ENCODING_KEY: &str = "relate.encoding";
/// Exact version of the recursive typed composite encoding.
pub const ENCODING_TYPED_JSON_V1: &str = "typed-json-v1";
/// Field-metadata key identifying graph and vector values.
pub const RELATE_VALUE_TYPE_KEY: &str = "relate.value_type";
pub const VALUE_TYPE_NODE_V1: &str = "node-v1";
pub const VALUE_TYPE_RELATIONSHIP_V1: &str = "relationship-v1";
pub const VALUE_TYPE_VECTOR_V1: &str = "vector-v1";
/// Legacy metadata values retained for decoder migration; never emitted.
pub const ENCODING_JSON: &str = "json";
pub const ENCODING_TAGGED: &str = "tagged";

const NANOS_PER_SEC: i64 = 1_000_000_000;
const MAX_VECTOR_CHILD_ELEMENTS: usize = 16_777_216;

/// A per-column build plan: the inferred kind plus any column-level metadata
/// (spatial SRID, datetime zone) that must be identical across every chunk.
struct ColPlan {
    name: String,
    kind: ColType,
    /// SRID for a `Spatial` column, checked over every row.
    srid: Option<i32>,
    /// Shared fixed-offset timezone for a native `DateTime` column.
    tz: Option<String>,
}

fn plan_columns(
    columns: &[String],
    rows: &[IndexMap<String, CypherValue>],
) -> Result<Vec<ColPlan>, ArrowError> {
    columns
        .iter()
        .map(|name| {
            let inferred = schema::infer_column(name, rows);
            let kind = inferred.kind;
            let (srid, tz) = match kind {
                ColType::Spatial => (Some(spatial_srid(name, rows)?), None),
                ColType::DateTime => {
                    let offset = inferred.datetime_offset.ok_or_else(|| {
                        ArrowError::InvalidArgumentError(format!(
                            "native DateTime column '{name}' has no inferred fixed offset"
                        ))
                    })?;
                    (None, Some(format_offset(&offset)))
                }
                _ => (None, None),
            };
            // Child slots include masked nulls across the full result even
            // when emitted in chunks. Coordinates are checked as they are
            // appended to the direct array (or encoded as typed JSON).
            if let ColType::Vector { dim, .. } = kind {
                check_vector_child_budget(name, dim, rows.len())?;
            }
            Ok(ColPlan {
                name: name.clone(),
                kind,
                srid,
                tz,
            })
        })
        .collect()
}

/// Count parent-null and absent rows: FixedSizeList still needs `dim` masked
/// child placeholders for each. Use the whole result even when emitting chunks.
fn check_vector_child_budget(col: &str, dim: usize, rows: usize) -> Result<(), ArrowError> {
    match dim.checked_mul(rows) {
        Some(count) if count <= MAX_VECTOR_CHILD_ELEMENTS => Ok(()),
        _ => Err(ArrowError::InvalidArgumentError(format!(
            "vector column '{col}' dimension {dim} * row count {rows} exceeds child element bound {MAX_VECTOR_CHILD_ELEMENTS} (or overflows)"
        ))),
    }
}

fn spatial_srid(col: &str, rows: &[IndexMap<String, CypherValue>]) -> Result<i32, ArrowError> {
    let mut first = None;
    for (row_index, row) in rows.iter().enumerate() {
        let (srid, geometry) = match row.get(col) {
            Some(CypherValue::Point(p)) => {
                check_finite_coordinate(&[p.x, p.y], col, row_index, "")?;
                (p.srid, None)
            }
            Some(CypherValue::Geometry(g)) => (g.srid(), Some(g)),
            _ => continue,
        };
        let field_srid = *first.get_or_insert(srid);
        if field_srid != srid {
            return Err(ArrowError::InvalidArgumentError(format!(
                "column '{col}' has conflicting spatial SRIDs {field_srid} and {srid}"
            )));
        }
        if let Some(geometry) = geometry {
            check_spatial_geometry(geometry, field_srid, col, row_index, "", 1)?;
        }
    }
    first.ok_or_else(|| {
        ArrowError::InvalidArgumentError(format!(
            "spatial column '{col}' has no non-null Point or Geometry to supply an SRID"
        ))
    })
}

fn check_wkb_member_depth(
    col: &str,
    row: usize,
    path: &str,
    depth: usize,
) -> Result<(), ArrowError> {
    if depth > MAX_GEOMETRY_DEPTH {
        return Err(ArrowError::InvalidArgumentError(format!(
            "column '{col}' row {row} geometry member {path} WKB depth {depth} exceeds {MAX_GEOMETRY_DEPTH}"
        )));
    }
    Ok(())
}

fn check_finite_coordinate(
    coord: &[f64; 2],
    col: &str,
    row: usize,
    path: &str,
) -> Result<(), ArrowError> {
    let location = if path.is_empty() { "$" } else { path };
    for (axis, value) in [("x", coord[0]), ("y", coord[1])] {
        if !value.is_finite() {
            return Err(ArrowError::InvalidArgumentError(format!(
                "column '{col}' row {row} coordinate {location} {axis} must be finite"
            )));
        }
    }
    Ok(())
}

/// WKB carries neither collection members' SRIDs nor unbounded nesting, and
/// cannot encode non-finite spatial coordinates. Check all three in the
/// full-result preflight before descending or building any batch.
fn check_spatial_geometry(
    geometry: &GeometryValue,
    field_srid: i32,
    col: &str,
    row: usize,
    path: &str,
    depth: usize,
) -> Result<(), ArrowError> {
    match geometry {
        GeometryValue::LineString { coordinates, .. } => {
            for (index, coord) in coordinates.iter().enumerate() {
                check_finite_coordinate(coord, col, row, &format!("{path}[{index}]"))?;
            }
        }
        GeometryValue::Polygon { rings, .. } => {
            for (ring_index, ring) in rings.iter().enumerate() {
                for (index, coord) in ring.iter().enumerate() {
                    check_finite_coordinate(
                        coord,
                        col,
                        row,
                        &format!("{path}[{ring_index}][{index}]"),
                    )?;
                }
            }
        }
        GeometryValue::GeometryCollection { geometries, srid } => {
            for (index, child) in geometries.iter().enumerate() {
                let member_path = format!("{path}[{index}]");
                check_wkb_member_depth(col, row, &member_path, depth + 1)?;
                let child_srid = child.srid();
                if child_srid != *srid || child_srid != field_srid {
                    return Err(ArrowError::InvalidArgumentError(format!(
                        "column '{col}' row {row} collection member {member_path} has SRID {child_srid}, conflicting with parent SRID {srid} and field SRID {field_srid}"
                    )));
                }
                check_spatial_geometry(child, field_srid, col, row, &member_path, depth + 1)?;
            }
        }
        // These encode a nested WKB header for each member even though their
        // GeometryValue representation stores only coordinates.
        GeometryValue::MultiPoint { points, .. } => {
            for (index, coord) in points.iter().enumerate() {
                let member_path = format!("{path}[{index}]");
                check_wkb_member_depth(col, row, &member_path, depth + 1)?;
                check_finite_coordinate(coord, col, row, &member_path)?;
            }
        }
        GeometryValue::MultiLineString { lines, .. } => {
            for (line_index, line) in lines.iter().enumerate() {
                let member_path = format!("{path}[{line_index}]");
                check_wkb_member_depth(col, row, &member_path, depth + 1)?;
                for (index, coord) in line.iter().enumerate() {
                    check_finite_coordinate(coord, col, row, &format!("{member_path}[{index}]"))?;
                }
            }
        }
        GeometryValue::MultiPolygon { polygons, .. } => {
            for (polygon_index, polygon) in polygons.iter().enumerate() {
                let member_path = format!("{path}[{polygon_index}]");
                check_wkb_member_depth(col, row, &member_path, depth + 1)?;
                for (ring_index, ring) in polygon.iter().enumerate() {
                    for (index, coord) in ring.iter().enumerate() {
                        check_finite_coordinate(
                            coord,
                            col,
                            row,
                            &format!("{member_path}[{ring_index}][{index}]"),
                        )?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn format_offset(off: &FixedOffset) -> String {
    let secs = off.local_minus_utc();
    let sign = if secs < 0 { '-' } else { '+' };
    let a = secs.abs();
    format!("{}{:02}:{:02}", sign, a / 3600, (a % 3600) / 60)
}

// ---- top-level entry points -------------------------------------------------

/// Convert query result rows to a single Arrow `RecordBatch` with real,
/// full-column-inferred Arrow types. See the module docs for the schema
/// mapping.
///
/// Returns an `ArrowError` for invalid vector values, conflicting spatial
/// SRIDs, non-minute DateTime offsets (through the typed-JSON model writer),
/// or any native temporal/spatial plan that cannot be encoded without loss.
/// Out-of-range timestamps and durations are normally inferred as lossless
/// typed JSON instead.
///
/// A spatial WKB field stores one SRID; nested collections must share it.
/// Both writers check nested SRIDs, finite x/y coordinates and WKB header depth
/// across all native WKB rows before encoding. Typed-JSON spatial cells retain
/// their own SRID and are checked by the model payload writer, including z.
/// The top-level header is level 1, and nested headers (including
/// Multi* member headers) count toward the maximum of 64.
///
/// ```
/// use relate_arrow::{from_arrow::record_batch_to_rows, record_batch::{
///     result_to_record_batch, result_to_record_batches,
/// }};
/// use relate_model::{CypherValue, GeometryValue};
/// use indexmap::IndexMap;
///
/// let columns = ["location".to_owned()];
/// let row = |geometry| IndexMap::from([("location".to_owned(),
///     CypherValue::Geometry(geometry))]);
/// let line = |srid| GeometryValue::LineString {
///     coordinates: vec![[1.25, -2.5], [3.0, 4.75]], srid,
/// };
/// let nested = |srid| GeometryValue::GeometryCollection {
///     geometries: vec![GeometryValue::GeometryCollection {
///         geometries: vec![line(srid)], srid: 4326,
///     }], srid: 4326,
/// };
/// let valid = row(nested(4326));
/// let batch = result_to_record_batch(&columns, &[valid.clone()])?;
/// assert_eq!(batch.schema().field(0).metadata()["relate.srid"], "4326");
/// assert_eq!(record_batch_to_rows(&batch)?.1, vec![valid.clone()]);
///
/// // The later row would be in a separate chunk, but preflight rejects it
/// // before returning any batches. [0][0] locates the differing grandchild.
/// let rows = [valid, row(nested(7203))];
/// for error in [
///     result_to_record_batch(&columns, &rows).unwrap_err(),
///     result_to_record_batches(&columns, &rows, 1).unwrap_err(),
/// ] {
///     let text = error.to_string();
///     assert!(text.contains("column 'location' row 1"), "{text}");
///     assert!(text.contains("[0][0]"), "{text}");
///     assert!(text.contains("SRID 7203") && text.contains("field SRID 4326"), "{text}");
/// }
///
/// // Count each collection's WKB header, including the empty leaf's.
/// let deep = |depth| {
///     let mut geometry = GeometryValue::GeometryCollection {
///         geometries: vec![], srid: 4326,
///     };
///     for _ in 1..depth {
///         geometry = GeometryValue::GeometryCollection {
///             geometries: vec![geometry], srid: 4326,
///         };
///     }
///     geometry
/// };
/// let level_64 = row(deep(64));
/// assert_eq!(record_batch_to_rows(&result_to_record_batch(
///     &columns, &[level_64.clone()])?)?.1, vec![level_64]);
/// let rows = [row(nested(4326)), row(deep(65))];
/// for error in [
///     result_to_record_batch(&columns, &rows).unwrap_err(),
///     result_to_record_batches(&columns, &rows, 1).unwrap_err(),
/// ] {
///     assert!(error.to_string().contains("column 'location' row 1"));
///     assert!(error.to_string().contains("depth 65"));
///     assert!(error.to_string().contains("[0]"));
/// }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn result_to_record_batch(
    columns: &[String],
    rows: &[IndexMap<String, CypherValue>],
) -> Result<RecordBatch, arrow::error::ArrowError> {
    let plans = plan_columns(columns, rows)?;
    build_batch(&plans, rows, 0)
}

/// Like [`result_to_record_batch`] but splits the result into chunked batches
/// of at most `max_rows_per_batch` rows each, all sharing one schema inferred
/// over the **whole** result (so every chunk is schema-compatible). Returns a
/// single empty batch for an empty result.
/// Full-result native WKB SRID/depth/finite-coordinate, native DateTime offset
/// and vector child-budget checks run before chunking;
/// values are checked as each array is constructed. An error in any chunk
/// returns no batches.
pub fn result_to_record_batches(
    columns: &[String],
    rows: &[IndexMap<String, CypherValue>],
    max_rows_per_batch: usize,
) -> Result<Vec<RecordBatch>, arrow::error::ArrowError> {
    let plans = plan_columns(columns, rows)?;
    if rows.is_empty() {
        return Ok(vec![build_batch(&plans, rows, 0)?]);
    }
    let chunk = max_rows_per_batch.max(1);
    rows.chunks(chunk)
        .enumerate()
        .map(|(index, c)| build_batch(&plans, c, index * chunk))
        .collect::<Result<Vec<RecordBatch>, ArrowError>>()
}

fn build_batch(
    plans: &[ColPlan],
    rows: &[IndexMap<String, CypherValue>],
    row_offset: usize,
) -> Result<RecordBatch, arrow::error::ArrowError> {
    let mut fields: Vec<Field> = Vec::with_capacity(plans.len());
    let mut arrays: Vec<ArrayRef> = Vec::with_capacity(plans.len());
    for plan in plans {
        let (array, field) = build_column(plan, rows, row_offset)?;
        fields.push(field);
        arrays.push(array);
    }
    RecordBatch::try_new(Arc::new(Schema::new(fields)), arrays)
}

// ---- per-column array building ---------------------------------------------

/// Build the array for one column and the matching `Field`. The `Field`'s
/// `DataType` is always derived from the built array so the two can never
/// disagree (which would make `RecordBatch::try_new` fail).
fn build_column(
    plan: &ColPlan,
    rows: &[IndexMap<String, CypherValue>],
    row_offset: usize,
) -> Result<(ArrayRef, Field), ArrowError> {
    let name = &plan.name;
    let get = |r: &IndexMap<String, CypherValue>| r.get(name).cloned().unwrap_or(CypherValue::Null);

    Ok(match plan.kind {
        ColType::Null => {
            let arr = StringArray::from(vec![None as Option<&str>; rows.len()]);
            (Arc::new(arr), Field::new(name, DataType::Utf8, true))
        }
        ColType::Bool => {
            let vals: Vec<Option<bool>> = rows
                .iter()
                .map(|r| match get(r) {
                    CypherValue::Bool(b) => Some(b),
                    _ => None,
                })
                .collect();
            let arr = BooleanArray::from(vals);
            let dt = arr.data_type().clone();
            (Arc::new(arr), Field::new(name, dt, true))
        }
        ColType::Int => {
            let vals: Vec<Option<i64>> = rows
                .iter()
                .map(|r| match get(r) {
                    CypherValue::Int(i) => Some(i),
                    _ => None,
                })
                .collect();
            let arr = Int64Array::from(vals);
            let dt = arr.data_type().clone();
            (Arc::new(arr), Field::new(name, dt, true))
        }
        ColType::Float => {
            let vals: Vec<Option<f64>> = rows
                .iter()
                .map(|r| match get(r) {
                    CypherValue::Float(f) => Some(f),
                    CypherValue::Int(i) => Some(i as f64),
                    _ => None,
                })
                .collect();
            let arr = Float64Array::from(vals);
            let dt = arr.data_type().clone();
            (Arc::new(arr), Field::new(name, dt, true))
        }
        ColType::Str => {
            let vals: Vec<Option<String>> = rows
                .iter()
                .map(|r| match get(r) {
                    CypherValue::Str(s) => Some(s),
                    _ => None,
                })
                .collect();
            let arr = StringArray::from(vals);
            let dt = arr.data_type().clone();
            (Arc::new(arr), Field::new(name, dt, true))
        }
        ColType::Date => {
            let vals: Vec<Option<i32>> = rows
                .iter()
                .map(|r| match get(r) {
                    CypherValue::Date(d) => Some(date_to_epoch_days(d)),
                    _ => None,
                })
                .collect();
            let arr = Date32Array::from(vals);
            let dt = arr.data_type().clone();
            (Arc::new(arr), Field::new(name, dt, true))
        }
        ColType::Time => {
            let vals: Vec<Option<i64>> = rows
                .iter()
                .map(|r| match get(r) {
                    CypherValue::Time(t, None) => Some(time_to_nanos(&t)),
                    _ => None,
                })
                .collect();
            let arr = Time64NanosecondArray::from(vals);
            let dt = arr.data_type().clone();
            (Arc::new(arr), Field::new(name, dt, true))
        }
        ColType::DateTime => {
            let vals: Vec<Option<i64>> = rows
                .iter()
                .enumerate()
                .map(|(row, r)| match get(r) {
                    CypherValue::DateTime(dt, _) => dt.timestamp_nanos_opt().map(Some).ok_or_else(|| {
                        ArrowError::InvalidArgumentError(format!(
                            "column '{name}' row {row}: DateTime is outside Arrow nanosecond timestamp range"
                        ))
                    }),
                    _ => Ok(None),
                })
                .collect::<Result<_, _>>()?;
            let arr = TimestampNanosecondArray::from(vals).with_timezone_opt(plan.tz.clone());
            let dt = arr.data_type().clone();
            (Arc::new(arr), Field::new(name, dt, true))
        }
        ColType::LocalDateTime => {
            let vals: Vec<Option<i64>> = rows
                .iter()
                .enumerate()
                .map(|(row, r)| match get(r) {
                    CypherValue::LocalDateTime(ndt) => ndt.and_utc().timestamp_nanos_opt().map(Some).ok_or_else(|| {
                        ArrowError::InvalidArgumentError(format!(
                            "column '{name}' row {row}: LocalDateTime is outside Arrow nanosecond timestamp range"
                        ))
                    }),
                    _ => Ok(None),
                })
                .collect::<Result<_, _>>()?;
            let arr = TimestampNanosecondArray::from(vals);
            let dt = arr.data_type().clone();
            (Arc::new(arr), Field::new(name, dt, true))
        }
        ColType::Duration => {
            let vals: Vec<Option<IntervalMonthDayNano>> = rows
                .iter()
                .enumerate()
                .map(|(row, r)| match get(r) {
                    CypherValue::Duration(d) => {
                        let months = i32::try_from(d.months).map_err(|_| {
                            ArrowError::InvalidArgumentError(format!(
                                "column '{name}' row {row}: duration months {} exceed Arrow i32 range",
                                d.months
                            ))
                        })?;
                        let days = i32::try_from(d.days).map_err(|_| {
                            ArrowError::InvalidArgumentError(format!(
                                "column '{name}' row {row}: duration days {} exceed Arrow i32 range",
                                d.days
                            ))
                        })?;
                        let nanos = d
                            .seconds
                            .checked_mul(NANOS_PER_SEC)
                            .and_then(|s| s.checked_add(d.nanoseconds))
                            .ok_or_else(|| {
                                ArrowError::InvalidArgumentError(format!(
                                    "column '{name}' row {row}: duration nanoseconds exceed Arrow i64 range"
                                ))
                            })?;
                        Ok(Some(IntervalMonthDayNano::new(months, days, nanos)))
                    }
                    _ => Ok(None),
                })
                .collect::<Result<_, ArrowError>>()?;
            let arr = IntervalMonthDayNanoArray::from_iter(vals);
            let dt = arr.data_type().clone();
            (Arc::new(arr), Field::new(name, dt, true))
        }
        ColType::Spatial => {
            let srid = plan.srid.ok_or_else(|| {
                ArrowError::InvalidArgumentError(format!(
                    "spatial column '{name}' is missing its SRID"
                ))
            })?;
            let vals: Vec<CypherValue> = rows.iter().map(get).collect();
            let arr = geometries_to_wkb_array(&vals); // wires up the WKB encoder
            let dt = arr.data_type().clone();
            let mut md = HashMap::new();
            md.insert(
                "ARROW:extension:name".to_string(),
                GEOARROW_EXTENSION_NAME.to_string(),
            );
            md.insert(RELATE_SRID_KEY.to_string(), srid.to_string());
            md.insert(
                "ARROW:extension:metadata".to_string(),
                format!("{{\"crs\":\"EPSG:{srid}\"}}"),
            );
            (arr, Field::new(name, dt, true).with_metadata(md))
        }
        ColType::Vector { coord, dim } => {
            let vals: Vec<CypherValue> = rows.iter().map(get).collect();
            let arr = build_vector_array(name, coord, dim, &vals, row_offset)?;
            let dt = arr.data_type().clone();
            (
                arr,
                marked_field(name, dt, RELATE_VALUE_TYPE_KEY, VALUE_TYPE_VECTOR_V1),
            )
        }
        ColType::Node => {
            let vals: Vec<CypherValue> = rows.iter().map(get).collect();
            let arr = build_node_array(&vals, row_offset)
                .map_err(|e| ArrowError::InvalidArgumentError(format!("column '{name}': {e}")))?;
            let dt = arr.data_type().clone();
            (
                arr,
                marked_field(name, dt, RELATE_VALUE_TYPE_KEY, VALUE_TYPE_NODE_V1),
            )
        }
        ColType::Relationship => {
            let vals: Vec<CypherValue> = rows.iter().map(get).collect();
            let arr = build_relationship_array(&vals, row_offset)
                .map_err(|e| ArrowError::InvalidArgumentError(format!("column '{name}': {e}")))?;
            let dt = arr.data_type().clone();
            (
                arr,
                marked_field(name, dt, RELATE_VALUE_TYPE_KEY, VALUE_TYPE_RELATIONSHIP_V1),
            )
        }
        ColType::TypedJson => {
            let vals: Vec<Option<String>> = rows
                .iter()
                .enumerate()
                .map(|(row, r)| match get(r) {
                    CypherValue::Null => Ok(None),
                    v => typed_json::encode(&v).map(Some).map_err(|e| {
                        ArrowError::InvalidArgumentError(format!(
                            "column '{name}' row {}: {e}",
                            row_offset + row
                        ))
                    }),
                })
                .collect::<Result<_, _>>()?;
            let arr = StringArray::from(vals);
            let dt = arr.data_type().clone();
            (
                Arc::new(arr),
                marked_field(name, dt, RELATE_ENCODING_KEY, ENCODING_TYPED_JSON_V1),
            )
        }
    })
}

fn marked_field(name: &str, dt: DataType, key: &str, value: &str) -> Field {
    Field::new(name, dt, true).with_metadata(HashMap::from([(key.to_owned(), value.to_owned())]))
}

fn date_to_epoch_days(d: NaiveDate) -> i32 {
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
    d.signed_duration_since(epoch).num_days() as i32
}

fn time_to_nanos(t: &chrono::NaiveTime) -> i64 {
    t.num_seconds_from_midnight() as i64 * NANOS_PER_SEC + t.nanosecond() as i64
}

/// Build a `FixedSizeList<primitive>` array for a vector column. Every row is
/// either a `Vector` of the plan's `coord`/`dim`, or `Null` (padded with `dim`
/// placeholder zeros in the child and masked at the parent level).
fn build_vector_array(
    name: &str,
    coord: VectorCoordKind,
    dim: usize,
    vals: &[CypherValue],
    row_offset: usize,
) -> Result<ArrayRef, ArrowError> {
    let size = i32::try_from(dim).map_err(|_| {
        ArrowError::InvalidArgumentError(format!(
            "vector column '{name}' dimension {dim} exceeds i32"
        ))
    })?;
    let mut valid: Vec<bool> = Vec::with_capacity(vals.len());

    macro_rules! build_child {
        ($builder:expr, $child_dt:expr, $convert:expr, $zero:expr) => {{
            let mut b = $builder;
            for (row, v) in vals.iter().enumerate() {
                match v {
                    CypherValue::Vector { coord: c, coords } if *c == coord && coords.len() == dim => {
                        valid.push(true);
                        for (idx, e) in coords.iter().enumerate() {
                            let converted = ($convert)(e).ok_or_else(|| ArrowError::InvalidArgumentError(
                                 format!("column '{name}' row {} vector coordinate {idx} has wrong type, is out of range for, or loses precision in {coord:?}", row_offset + row)))?;
                            b.append_value(converted);
                        }
                    }
                    CypherValue::Null => {
                        valid.push(false);
                        for _ in 0..dim {
                            b.append_value($zero);
                        }
                    }
                    _ => return Err(ArrowError::InvalidArgumentError(format!(
                        "column '{name}' row {} vector does not match {coord:?} dimension {dim}", row_offset + row
                    ))),
                }
            }
            Ok::<_, ArrowError>((Arc::new(b.finish()) as ArrayRef, $child_dt))
        }};
    }

    let (child, child_dt): (ArrayRef, DataType) = match coord {
        VectorCoordKind::Int8 => {
            build_child!(Int8Builder::new(), DataType::Int8, narrow::<i8>, 0i8)
        }
        VectorCoordKind::Int16 => {
            build_child!(Int16Builder::new(), DataType::Int16, narrow::<i16>, 0i16)
        }
        VectorCoordKind::Int32 => {
            build_child!(Int32Builder::new(), DataType::Int32, narrow::<i32>, 0i32)
        }
        VectorCoordKind::Int64 => {
            build_child!(Int64Builder::new(), DataType::Int64, narrow::<i64>, 0i64)
        }
        // Every FLOAT16/BFLOAT16/FLOAT32 coordinate lives in the f32
        // family and widens exactly into Arrow's binary32 — one row, no
        // narrower floating type exists.
        // The three f32-family kinds share Arrow's binary32 child (no
        // narrower float exists); the arms must be spelled for the
        // match's exhaustiveness, the membership predicate lives on the
        // kind.
        VectorCoordKind::Float16 | VectorCoordKind::BFloat16 | VectorCoordKind::Float32 => {
            build_child!(Float32Builder::new(), DataType::Float32, as_f32, 0f32)
        }
        VectorCoordKind::Float64 => {
            build_child!(Float64Builder::new(), DataType::Float64, as_f64, 0f64)
        }
    }?;

    let field = Arc::new(Field::new("item", child_dt, false));
    let nulls = NullBuffer::from(valid);
    Ok(Arc::new(FixedSizeListArray::try_new(
        field,
        size,
        child,
        Some(nulls),
    )?))
}

/// once for the four integer rows of the builder table (ADR-0706).
fn narrow<T: TryFrom<i64>>(coord: Coord) -> Option<T> {
    match coord {
        Coord::I(i) => T::try_from(i).ok(),
        Coord::F(_) => None,
    }
}

/// The float families' one accessor each — the twins of [`narrow`], so
/// every builder-table row names one conversion (ADR-0706).
/// The float families' one widening (ADR-0706): the `Int` family under a
/// float kind is legal and reads as its number. Both arms succeed, so the
/// read is total — `as_f32` narrows to the column's binary32.
fn as_f64(coord: Coord) -> Option<f64> {
    Some(match coord {
        Coord::F(f) => f,
        Coord::I(n) => n as f64,
    })
}

fn as_f32(coord: Coord) -> Option<f32> {
    Some(as_f64(coord)? as f32)
}

/// Build a `Struct{id, labels, properties}` array for a node column.
fn build_node_array(vals: &[CypherValue], row_offset: usize) -> Result<ArrayRef, ArrowError> {
    let mut id_b = Int64Builder::new();
    let mut label_b = ListBuilder::new(StringBuilder::new());
    let mut prop_b = StringBuilder::new();
    let mut valid: Vec<bool> = Vec::with_capacity(vals.len());

    for (row, v) in vals.iter().enumerate() {
        match v {
            CypherValue::Node {
                id,
                labels,
                properties,
            } => {
                valid.push(true);
                id_b.append_value(*id);
                for l in labels {
                    label_b.values().append_value(l);
                }
                label_b.append(true);
                prop_b.append_value(typed_json::encode_props(properties).map_err(|e| {
                    ArrowError::InvalidArgumentError(format!(
                        "node row {} properties: {e}",
                        row_offset + row
                    ))
                })?);
            }
            _ => {
                valid.push(false);
                id_b.append_value(0);
                label_b.append(false);
                prop_b.append_null();
            }
        }
    }

    let id_arr = Arc::new(id_b.finish()) as ArrayRef;
    let label_arr = Arc::new(label_b.finish()) as ArrayRef;
    let prop_arr = Arc::new(prop_b.finish()) as ArrayRef;
    let fields = Fields::from(vec![
        Arc::new(Field::new("id", id_arr.data_type().clone(), false)),
        Arc::new(Field::new("labels", label_arr.data_type().clone(), true)),
        Arc::new(Field::new("properties", prop_arr.data_type().clone(), true)),
    ]);
    let nulls = NullBuffer::from(valid);
    Ok(Arc::new(StructArray::new(
        fields,
        vec![id_arr, label_arr, prop_arr],
        Some(nulls),
    )))
}

/// Build a `Struct{id, type, start_node, end_node, properties}` array for a
/// relationship column.
fn build_relationship_array(
    vals: &[CypherValue],
    row_offset: usize,
) -> Result<ArrayRef, ArrowError> {
    let mut id_b = Int64Builder::new();
    let mut type_b = StringBuilder::new();
    let mut start_b = Int64Builder::new();
    let mut end_b = Int64Builder::new();
    let mut prop_b = StringBuilder::new();
    let mut valid: Vec<bool> = Vec::with_capacity(vals.len());

    for (row, v) in vals.iter().enumerate() {
        match v {
            CypherValue::Relationship {
                id,
                start_id,
                end_id,
                rel_type,
                properties,
            } => {
                valid.push(true);
                id_b.append_value(*id);
                type_b.append_value(rel_type);
                start_b.append_value(*start_id);
                end_b.append_value(*end_id);
                prop_b.append_value(typed_json::encode_props(properties).map_err(|e| {
                    ArrowError::InvalidArgumentError(format!(
                        "relationship row {} properties: {e}",
                        row_offset + row
                    ))
                })?);
            }
            _ => {
                valid.push(false);
                id_b.append_value(0);
                type_b.append_null();
                start_b.append_value(0);
                end_b.append_value(0);
                prop_b.append_null();
            }
        }
    }

    let id_arr = Arc::new(id_b.finish()) as ArrayRef;
    let type_arr = Arc::new(type_b.finish()) as ArrayRef;
    let start_arr = Arc::new(start_b.finish()) as ArrayRef;
    let end_arr = Arc::new(end_b.finish()) as ArrayRef;
    let prop_arr = Arc::new(prop_b.finish()) as ArrayRef;
    let fields = Fields::from(vec![
        Arc::new(Field::new("id", id_arr.data_type().clone(), false)),
        Arc::new(Field::new("type", type_arr.data_type().clone(), true)),
        Arc::new(Field::new(
            "start_node",
            start_arr.data_type().clone(),
            false,
        )),
        Arc::new(Field::new("end_node", end_arr.data_type().clone(), false)),
        Arc::new(Field::new("properties", prop_arr.data_type().clone(), true)),
    ]);
    let nulls = NullBuffer::from(valid);
    Ok(Arc::new(StructArray::new(
        fields,
        vec![id_arr, type_arr, start_arr, end_arr, prop_arr],
        Some(nulls),
    )))
}

/// Convert a CypherValue to a JSON string for the all-Utf8
/// [`result_to_string_batch`] path. Strings pass through verbatim; everything
/// else is rendered as canonical wire JSON.
fn cypher_to_json_string(val: &CypherValue) -> String {
    match val {
        CypherValue::Str(s) => s.clone(),
        CypherValue::Null => String::new(),
        other => wire::value_to_wire_json(other).to_string(),
    }
}

/// Convert query result rows to a `RecordBatch` with all columns as Utf8
/// strings. Useful when type inference is not desired.
pub fn result_to_string_batch(
    columns: &[String],
    rows: &[IndexMap<String, CypherValue>],
) -> Result<RecordBatch, arrow::error::ArrowError> {
    let schema = schema::string_schema(columns);

    let arrays: Vec<ArrayRef> = columns
        .iter()
        .map(|col_name| {
            let values: Vec<Option<String>> = rows
                .iter()
                .map(|row| match row.get(col_name) {
                    Some(CypherValue::Null) | None => None,
                    Some(val) => Some(cypher_to_json_string(val)),
                })
                .collect();
            Arc::new(StringArray::from(values)) as ArrayRef
        })
        .collect();

    RecordBatch::try_new(schema, arrays)
}

#[cfg(test)]
#[path = "record_batch/tests.rs"]
mod tests;
