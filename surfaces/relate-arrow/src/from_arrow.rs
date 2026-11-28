//! Convert an Arrow `RecordBatch` back to CypherValue result sets.
//!
//! This is the inverse of [`crate::record_batch`]. It decodes the real Arrow
//! types produced there (`Date32`, `Time64`, `Timestamp`, `Interval`,
//! `FixedSizeList` vectors, WKB spatial `Binary`, node/relationship `Struct`s)
//! using exact field provenance. Untagged legacy composites are rejected: their
//! graph-shaped maps and lists cannot be distinguished from actual graph values.

use arrow::array::Array;
use arrow_array::types::IntervalMonthDayNano;
use arrow_array::{
    BinaryArray, BooleanArray, Date32Array, FixedSizeListArray, Float32Array, Float64Array,
    Int16Array, Int32Array, Int64Array, Int8Array, ListArray, StringArray, StructArray,
    Time64NanosecondArray, TimestampNanosecondArray,
};
use arrow_array::{IntervalMonthDayNanoArray, RecordBatch};
use arrow_schema::{ArrowError, DataType, Field, IntervalUnit, TimeUnit};
use chrono::{DateTime, Duration as ChronoDuration, FixedOffset, NaiveDate, NaiveTime};
use indexmap::IndexMap;
use relate_model::serde_value::parse_numeric_offset;
use relate_model::value::IsoDuration;
use relate_model::{CypherValue, VectorCoordKind, VectorCoords};
use std::collections::HashSet;

use crate::geo_encoding::wkb_to_cypher_value;
use crate::record_batch::{
    ENCODING_JSON, ENCODING_TAGGED, ENCODING_TYPED_JSON_V1, GEOARROW_EXTENSION_NAME,
    RELATE_ENCODING_KEY, RELATE_SRID_KEY, RELATE_VALUE_TYPE_KEY, VALUE_TYPE_NODE_V1,
    VALUE_TYPE_RELATIONSHIP_V1, VALUE_TYPE_VECTOR_V1,
};
use crate::typed_json;

const NANOS_PER_SEC: i64 = 1_000_000_000;

/// Column names and their decoded Arrow rows.
pub type DecodedRows = (Vec<String>, Vec<IndexMap<String, CypherValue>>);

/// Validated decoding instructions for one field and its nested children.
/// Only the preflight reads field metadata; rows reuse this plan.
enum PreparedField {
    Plain,
    Timestamp(Option<TimestampZone>),
    TypedJson,
    Spatial(i32),
    Vector,
    Node,
    Relationship,
    List(Box<Self>),
    Struct(Vec<Self>),
}

enum TimestampZone {
    Fixed(FixedOffset),
    Named(String),
}

/// Convert a RecordBatch to CypherValue rows.
///
/// Returns (column_names, rows) where each row is a map of column name to CypherValue.
/// Invalid typed values and malformed properties return an Arrow error.
///
/// ```
/// use relate_arrow::{from_arrow::record_batch_to_rows, record_batch::result_to_record_batch};
/// use relate_model::CypherValue;
/// use indexmap::IndexMap;
/// use arrow_array::{RecordBatch, StringArray};
/// use arrow_schema::{DataType, Field, Schema};
/// use std::{collections::HashMap, sync::Arc};
///
/// let node = CypherValue::Node {
///     id: 7, labels: vec!["Person".into()], properties: IndexMap::new(),
/// };
/// let mut lookalike = IndexMap::new();
/// lookalike.insert("id".into(), CypherValue::Int(7));
/// let rows = vec![IndexMap::from([
///     ("node".into(), node), ("map".into(), CypherValue::Map(lookalike)),
/// ])];
/// let batch = result_to_record_batch(&["node".into(), "map".into()], &rows)?;
/// assert_eq!(batch.schema().field(0).metadata()["relate.value_type"], "node-v1");
/// assert_eq!(batch.schema().field(1).metadata()["relate.encoding"], "typed-json-v1");
/// let (_, decoded) = record_batch_to_rows(&batch)?;
/// assert!(matches!(decoded[0]["node"], CypherValue::Node { id: 7, .. }));
/// assert!(matches!(decoded[0]["map"], CypherValue::Map(_)));
///
/// let legacy_field = Field::new("old", DataType::Utf8, true).with_metadata(
///     HashMap::from([("relate.encoding".into(), "json".into())]),
/// );
/// let legacy = RecordBatch::try_new(Arc::new(Schema::new(vec![legacy_field])),
///     vec![Arc::new(StringArray::from(vec![r#"{"id":7}"#]))])?;
/// assert!(record_batch_to_rows(&legacy).is_err());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// Zoned Time and DateTime columns retain their per-cell offsets when they
/// cannot be represented by one native Arrow timezone; subminute offsets
/// error instead of being rounded to minutes:
///
/// ```
/// use arrow_schema::{DataType, TimeUnit};
/// use chrono::{DateTime, FixedOffset, NaiveTime};
/// use relate_arrow::{from_arrow::record_batch_to_rows, record_batch::{
///     result_to_record_batch, result_to_record_batches,
/// }};
/// use relate_model::{serde_value::{cypher_value_to_json,
///     try_json_to_cypher_value_v2}, CypherValue};
/// use indexmap::IndexMap;
///
/// let clock = NaiveTime::from_hms_opt(12, 34, 56).unwrap();
/// let zoned = CypherValue::Time(clock, Some(FixedOffset::east_opt(2 * 3600 + 30 * 60).unwrap()));
/// let json = cypher_value_to_json(&zoned)?;
/// assert_eq!(json["offset"], "+02:30");
/// assert_eq!(try_json_to_cypher_value_v2(&json)?, zoned);
/// let columns = ["value".to_owned()];
/// let time_rows = vec![IndexMap::from([("value".to_owned(), zoned.clone())])];
/// let batch = result_to_record_batch(&columns, &time_rows)?;
/// assert_eq!(batch.schema().field(0).data_type(), &DataType::Utf8);
/// assert_eq!(batch.schema().field(0).metadata()["relate.encoding"], "typed-json-v1");
/// assert_eq!(record_batch_to_rows(&batch)?.1[0]["value"], zoned);
/// let unzoned_rows = vec![IndexMap::from([("value".to_owned(), CypherValue::Time(clock, None))])];
/// assert_eq!(result_to_record_batch(&columns, &unzoned_rows)?.schema().field(0).data_type(),
///     &DataType::Time64(TimeUnit::Nanosecond));
///
/// // A one-second offset (+00:00:01) cannot survive minute-precision JSON or Arrow.
/// let bad_time = CypherValue::Time(clock, Some(FixedOffset::east_opt(1).unwrap()));
/// let nested = CypherValue::Map(IndexMap::from([("times".to_owned(),
///     CypherValue::List(vec![zoned, bad_time.clone()]))]));
/// assert_eq!(cypher_value_to_json(&nested).unwrap_err().path, "$.times[1]");
/// assert!(serde_json::to_value(&bad_time).is_err());
/// let bad_rows = vec![IndexMap::from([("value".to_owned(), bad_time)])];
/// assert!(result_to_record_batch(&columns, &bad_rows).is_err());
///
/// let first = DateTime::parse_from_rfc3339("2024-03-14T12:34:56+02:30")?;
/// let second = DateTime::parse_from_rfc3339("2024-03-15T08:09:10-05:00")?;
/// let native_rows = vec![IndexMap::from([("value".to_owned(), CypherValue::DateTime(first, None))])];
/// assert_eq!(result_to_record_batch(&columns, &native_rows)?.schema().field(0).data_type(),
///     &DataType::Timestamp(TimeUnit::Nanosecond, Some("+02:30".into())));
/// let datetime_rows = vec![native_rows[0].clone(), IndexMap::from([(
///     "value".to_owned(), CypherValue::DateTime(second, Some("Custom/Zone".into())),
/// )])];
/// assert_eq!(result_to_record_batch(&columns, &datetime_rows[1..])?
///     .schema().field(0).data_type(), &DataType::Utf8); // A name alone forces fallback.
/// let single = result_to_record_batch(&columns, &datetime_rows)?;
/// let chunks = result_to_record_batches(&columns, &datetime_rows, 1)?;
/// assert_eq!(chunks.len(), 2);
/// for batch in std::iter::once(&single).chain(chunks.iter()) {
///     assert_eq!(batch.schema().field(0).data_type(), &DataType::Utf8);
///     assert_eq!(batch.schema().field(0).metadata()["relate.encoding"], "typed-json-v1");
/// }
/// let chunk_rows = chunks.iter().map(record_batch_to_rows)
///     .collect::<Result<Vec<_>, _>>()?.into_iter()
///     .flat_map(|(_, rows)| rows).collect::<Vec<_>>();
/// for decoded in [record_batch_to_rows(&single)?.1, chunk_rows] {
///     for (original, actual_row) in datetime_rows.iter().zip(&decoded) {
///         match (&original["value"], &actual_row["value"]) {
///             (CypherValue::DateTime(expected, expected_name), CypherValue::DateTime(actual, name)) => {
///                 assert_eq!(actual.offset(), expected.offset());
///                 assert_eq!(actual.naive_local(), expected.naive_local());
///                 assert_eq!(actual.naive_utc(), expected.naive_utc());
///                 assert_eq!(name, expected_name);
///             }
///             _ => panic!("expected DateTime roundtrip"),
///         }
///     }
/// }
/// let bad_dt = CypherValue::DateTime(first.with_timezone(&FixedOffset::east_opt(1).unwrap()),
///     Some("Custom/Zone".into()));
/// assert_eq!(cypher_value_to_json(&bad_dt).unwrap_err().path, "$");
/// assert!(serde_json::to_value(&bad_dt).is_err());
/// let mut invalid_rows = datetime_rows;
/// invalid_rows.push(IndexMap::from([("value".to_owned(), bad_dt)]));
/// let error = result_to_record_batches(&columns, &invalid_rows, 1).unwrap_err();
/// assert!(error.to_string().contains("column 'value' row 2"));
/// assert!(error.to_string().contains("offset"));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// Full-column inference preserves 3D Points and integers that cannot fit in
/// Float64 exactly. External WKB with non-finite coordinates is an error:
///
/// ```
/// use arrow_array::BinaryArray;
/// use arrow_schema::DataType;
/// use relate_arrow::{from_arrow::record_batch_to_rows,
///     geo_encoding::{wkb_array_to_geometries, wkb_to_cypher_value},
///     record_batch::{result_to_record_batch, result_to_record_batches}};
/// use relate_model::{value::PointValue, CypherValue, GeometryValue};
/// use indexmap::IndexMap;
///
/// let columns = ["value".to_owned()];
/// let geometry = CypherValue::Geometry(GeometryValue::LineString {
///     coordinates: vec![[1.0, 2.0]], srid: 7203,
/// });
/// let point = CypherValue::Point(PointValue::wgs84_3d(1.25, -2.5, 75.0));
/// let spatial_rows = vec![
///     IndexMap::from([("value".into(), geometry)]),
///     IndexMap::from([("value".into(), point)]),
/// ];
/// let chunks = result_to_record_batches(&columns, &spatial_rows, 1)?;
/// for (batch, original) in chunks.iter().zip(&spatial_rows) {
///     assert_eq!(batch.schema().field(0).data_type(), &DataType::Utf8);
///     assert_eq!(batch.schema().field(0).metadata()["relate.encoding"], "typed-json-v1");
///     assert_eq!(&record_batch_to_rows(batch)?.1[0], original);
/// }
/// let spatial = result_to_record_batch(&columns, &spatial_rows)?;
/// let spatial_decoded = record_batch_to_rows(&spatial)?.1;
/// assert_eq!(spatial_decoded, spatial_rows);
/// assert!(matches!(&spatial_decoded[1]["value"], CypherValue::Point(p)
///     if p.z == Some(75.0) && p.srid == 4979));
///
/// let numeric_rows = vec![
///     IndexMap::from([("value".into(), CypherValue::Int(9_007_199_254_740_993))]),
///     IndexMap::from([("value".into(), CypherValue::Float(2.5))]),
///     IndexMap::from([("value".into(), CypherValue::Int(i64::MAX))]),
/// ];
/// let numeric = result_to_record_batch(&columns, &numeric_rows)?;
/// assert_eq!(numeric.schema().field(0).data_type(), &DataType::Utf8);
/// assert_eq!(numeric.schema().field(0).metadata()["relate.encoding"], "typed-json-v1");
/// assert_eq!(record_batch_to_rows(&numeric)?.1, numeric_rows);
/// let numeric_chunks = result_to_record_batches(&columns, &numeric_rows, 1)?;
/// for (batch, original) in numeric_chunks.iter().zip(&numeric_rows) {
///     assert_eq!(batch.schema().field(0).data_type(), &DataType::Utf8);
///     assert_eq!(&record_batch_to_rows(batch)?.1[0], original);
/// }
/// let exact_rows = vec![
///     IndexMap::from([("value".into(), CypherValue::Int(9_007_199_254_740_992))]),
///     IndexMap::from([("value".into(), CypherValue::Float(2.5))]),
/// ];
/// assert_eq!(result_to_record_batch(&columns, &exact_rows)?.schema().field(0).data_type(),
///     &DataType::Float64);
///
/// let mut external = vec![1, 1, 0, 0, 0]; // little-endian 2D Point
/// external.extend_from_slice(&1.0_f64.to_le_bytes());
/// external.extend_from_slice(&f64::INFINITY.to_le_bytes());
/// let error = wkb_to_cypher_value(&external, 4326).unwrap_err();
/// assert!(error.to_string().contains("Point.y"));
/// let cells = BinaryArray::from(vec![None, Some(external.as_slice())]);
/// let error = wkb_array_to_geometries(&cells, 4326).unwrap_err();
/// assert!(error.to_string().contains("row 1"));
/// assert!(error.to_string().contains("Point.y"));
/// let mut empty = vec![0, 0, 0, 0, 1]; // big-endian Point EMPTY NaN sentinel
/// empty.extend_from_slice(&f64::NAN.to_be_bytes());
/// empty.extend_from_slice(&f64::NAN.to_be_bytes());
/// assert!(wkb_to_cypher_value(&empty, 4326).unwrap_err().to_string().contains("Point.x"));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn record_batch_to_rows(batch: &RecordBatch) -> Result<DecodedRows, ArrowError> {
    let schema = batch.schema();
    let columns: Vec<String> = schema.fields().iter().map(|f| f.name().clone()).collect();

    // This is the only schema/physical-type preflight. Decode below may slice
    // nested arrays, but those slices retain the validated child data types.
    // Keep this before the row loop for empty and all-null batches as well.
    let prepared = schema
        .fields()
        .iter()
        .enumerate()
        .map(|(i, field)| {
            validate_field(field, batch.column(i).as_ref())
                .map_err(|e| ArrowError::ParseError(format!("field '{}': {e}", field.name())))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut rows = Vec::with_capacity(batch.num_rows());
    for row_idx in 0..batch.num_rows() {
        let mut row = IndexMap::new();
        for (col_idx, col_name) in columns.iter().enumerate() {
            let array = batch.column(col_idx);
            let value = array_value_to_cypher(&prepared[col_idx], array.as_ref(), row_idx)
                .map_err(|e| {
                    ArrowError::ParseError(format!("field '{col_name}' row {row_idx}: {e}"))
                })?;
            row.insert(col_name.clone(), value);
        }
        rows.push(row);
    }

    Ok((columns, rows))
}

fn validate_field(field: &Field, array: &dyn Array) -> Result<PreparedField, ArrowError> {
    let dt = field.data_type();
    if dt != array.data_type() {
        return Err(ArrowError::CastError(format!(
            "field type {dt:?} does not match array type {:?}",
            array.data_type()
        )));
    }
    if let Some(encoding) = field.metadata().get(RELATE_ENCODING_KEY) {
        match encoding.as_str() {
            ENCODING_TYPED_JSON_V1 if *dt == DataType::Utf8 => {}
            ENCODING_JSON | ENCODING_TAGGED => {
                return Err(ArrowError::ParseError(format!(
                    "legacy Arrow encoding '{encoding}' must be regenerated"
                )));
            }
            ENCODING_TYPED_JSON_V1 => {
                return Err(ArrowError::ParseError("typed-json-v1 requires Utf8".into()));
            }
            _ => {
                return Err(ArrowError::ParseError(format!(
                    "unknown Arrow encoding '{encoding}'"
                )))
            }
        }
    }
    if let Some(value_type) = field.metadata().get(RELATE_VALUE_TYPE_KEY) {
        let valid = matches!(
            (value_type.as_str(), dt),
            (
                VALUE_TYPE_NODE_V1 | VALUE_TYPE_RELATIONSHIP_V1,
                DataType::Struct(_)
            ) | (VALUE_TYPE_VECTOR_V1, DataType::FixedSizeList(..))
        );
        if !valid {
            return Err(ArrowError::ParseError(format!(
                "unknown or incompatible Arrow value type '{value_type}' for {dt:?}"
            )));
        }
    }
    if field.metadata().contains_key(RELATE_ENCODING_KEY)
        && field.metadata().contains_key(RELATE_VALUE_TYPE_KEY)
    {
        return Err(ArrowError::ParseError(
            "conflicting Arrow value markers".into(),
        ));
    }
    let extension = field.metadata().get("ARROW:extension:name");
    let srid = field.metadata().get(RELATE_SRID_KEY);
    let spatial = if extension.is_some() || srid.is_some() || *dt == DataType::Binary {
        if *dt != DataType::Binary || extension.map(String::as_str) != Some(GEOARROW_EXTENSION_NAME)
        {
            return Err(ArrowError::ParseError(
                "Binary requires geoarrow.wkb extension".into(),
            ));
        }
        Some(spatial_srid(field)?)
    } else {
        None
    };
    let plan = match dt {
        DataType::Boolean
        | DataType::Int64
        | DataType::Float64
        | DataType::Date32
        | DataType::Time64(TimeUnit::Nanosecond)
        | DataType::Interval(IntervalUnit::MonthDayNano)
        | DataType::Binary
        | DataType::Utf8 => match (dt, spatial) {
            (DataType::Binary, Some(srid)) => PreparedField::Spatial(srid),
            (DataType::Utf8, _) if field.metadata().contains_key(RELATE_ENCODING_KEY) => {
                PreparedField::TypedJson
            }
            _ => PreparedField::Plain,
        },
        DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::Float32 => {
            // These primitives are supported as children of ordinary lists.
            PreparedField::Plain
        }
        DataType::Timestamp(TimeUnit::Nanosecond, tz) => {
            let zone = tz
                .as_ref()
                .map(|z| {
                    parse_fixed_offset(z).map(|offset| match offset {
                        Some(offset) => TimestampZone::Fixed(offset),
                        None => TimestampZone::Named(z.to_string()),
                    })
                })
                .transpose()?;
            PreparedField::Timestamp(zone)
        }
        DataType::List(item) => {
            let list = downcast_array::<ListArray>(array)?;
            PreparedField::List(Box::new(validate_field(item, list.values().as_ref())?))
        }
        DataType::FixedSizeList(item, _) => {
            let list = downcast_array::<FixedSizeListArray>(array)?;
            let child = validate_field(item, list.values().as_ref())?;
            let vector = field
                .metadata()
                .get(RELATE_VALUE_TYPE_KEY)
                .map(String::as_str)
                == Some(VALUE_TYPE_VECTOR_V1);
            if vector
                && !matches!(
                    item.data_type(),
                    DataType::Int8
                        | DataType::Int16
                        | DataType::Int32
                        | DataType::Int64
                        | DataType::Float32
                        | DataType::Float64
                )
            {
                return Err(ArrowError::CastError(
                    "unsupported vector coordinate type".into(),
                ));
            }
            if vector {
                PreparedField::Vector
            } else {
                PreparedField::List(Box::new(child))
            }
        }
        DataType::Struct(_) => {
            let structure = downcast_array::<StructArray>(array)?;
            let marker = field
                .metadata()
                .get(RELATE_VALUE_TYPE_KEY)
                .map(String::as_str);
            if let Some(marker @ (VALUE_TYPE_NODE_V1 | VALUE_TYPE_RELATIONSHIP_V1)) = marker {
                validate_graph_fields(structure, marker)?;
            }
            let children = structure
                .fields()
                .iter()
                .enumerate()
                .map(|(i, child)| validate_field(child, structure.column(i).as_ref()))
                .collect::<Result<Vec<_>, _>>()?;
            match marker {
                Some(VALUE_TYPE_NODE_V1) => PreparedField::Node,
                Some(VALUE_TYPE_RELATIONSHIP_V1) => PreparedField::Relationship,
                _ => PreparedField::Struct(children),
            }
        }
        _ => {
            return Err(ArrowError::CastError(format!(
                "unsupported Arrow type {dt:?}"
            )))
        }
    };
    Ok(plan)
}

fn validate_graph_fields(arr: &StructArray, marker: &str) -> Result<(), ArrowError> {
    let required: &[&str] = match marker {
        VALUE_TYPE_NODE_V1 => &["id", "labels", "properties"],
        VALUE_TYPE_RELATIONSHIP_V1 => &["id", "type", "start_node", "end_node", "properties"],
        _ => unreachable!("validated graph marker"),
    };
    let mut seen = HashSet::new();
    for field in arr.fields() {
        let name = field.name().as_str();
        if !seen.insert(name) {
            return Err(ArrowError::ParseError(format!(
                "duplicate {marker} field '{name}'"
            )));
        }
        if !required.contains(&name) {
            return Err(ArrowError::ParseError(format!(
                "unexpected {marker} field '{name}'"
            )));
        }
    }
    for name in required {
        if !seen.contains(name) {
            return Err(ArrowError::ParseError(format!(
                "missing {marker} field '{name}'"
            )));
        }
        let child = arr
            .fields()
            .iter()
            .find(|field| field.name() == *name)
            .ok_or_else(|| ArrowError::ParseError(format!("missing {marker} field '{name}'")))?;
        let valid_type = match *name {
            "id" | "start_node" | "end_node" => child.data_type() == &DataType::Int64,
            "type" | "properties" => child.data_type() == &DataType::Utf8,
            "labels" => {
                matches!(child.data_type(), DataType::List(item) if item.data_type() == &DataType::Utf8)
            }
            _ => unreachable!("required graph field"),
        };
        if !valid_type {
            return Err(ArrowError::CastError(format!(
                "{marker} field '{name}' has wrong type {:?}",
                child.data_type()
            )));
        }
        // Marked graph children have a fixed representation. Their strings
        // (including label items and properties) are read directly, not as
        // independent typed-json cells; accepting markers here would silently
        // ignore their meaning.
        let marked = |field: &Field| {
            field.metadata().contains_key(RELATE_ENCODING_KEY)
                || field.metadata().contains_key(RELATE_VALUE_TYPE_KEY)
        };
        if marked(child) || matches!(child.data_type(), DataType::List(item) if marked(item)) {
            return Err(ArrowError::ParseError(format!(
                "{marker} field '{name}' has unexpected value metadata"
            )));
        }
    }
    Ok(())
}

fn spatial_srid(field: &Field) -> Result<i32, ArrowError> {
    let value = field
        .metadata()
        .get(RELATE_SRID_KEY)
        .ok_or_else(|| ArrowError::ParseError("missing relate.srid".into()))?;
    value
        .parse::<i32>()
        .map_err(|_| ArrowError::ParseError(format!("invalid relate.srid '{value}'")))
}

fn downcast_array<T: Array + 'static>(array: &dyn Array) -> Result<&T, ArrowError> {
    array.as_any().downcast_ref::<T>().ok_or_else(|| {
        ArrowError::CastError(format!(
            "wrong physical array type for {:?}",
            array.data_type()
        ))
    })
}

/// Decode a cell using the preflighted plan. Nested List slices retain their
/// validated child type, and no field metadata is read during row decoding.
fn array_value_to_cypher(
    plan: &PreparedField,
    array: &dyn Array,
    idx: usize,
) -> Result<CypherValue, ArrowError> {
    if array.is_null(idx) {
        return Ok(CypherValue::Null);
    }

    Ok(match (array.data_type(), plan) {
        (DataType::Boolean, PreparedField::Plain) => {
            let arr = downcast_array::<BooleanArray>(array)?;
            CypherValue::Bool(arr.value(idx))
        }
        (DataType::Int64, PreparedField::Plain) => {
            let arr = downcast_array::<Int64Array>(array)?;
            CypherValue::Int(arr.value(idx))
        }
        (DataType::Int8, PreparedField::Plain) => {
            CypherValue::Int(downcast_array::<Int8Array>(array)?.value(idx) as i64)
        }
        (DataType::Int16, PreparedField::Plain) => {
            CypherValue::Int(downcast_array::<Int16Array>(array)?.value(idx) as i64)
        }
        (DataType::Int32, PreparedField::Plain) => {
            CypherValue::Int(downcast_array::<Int32Array>(array)?.value(idx) as i64)
        }
        (DataType::Float32, PreparedField::Plain) => {
            CypherValue::Float(downcast_array::<Float32Array>(array)?.value(idx) as f64)
        }
        (DataType::Float64, PreparedField::Plain) => {
            let arr = downcast_array::<Float64Array>(array)?;
            CypherValue::Float(arr.value(idx))
        }
        (DataType::Date32, PreparedField::Plain) => {
            let arr = downcast_array::<Date32Array>(array)?;
            let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
            match epoch.checked_add_signed(ChronoDuration::days(arr.value(idx) as i64)) {
                Some(d) => CypherValue::Date(d),
                None => return Err(ArrowError::ParseError("invalid Date32 value".into())),
            }
        }
        (DataType::Time64(TimeUnit::Nanosecond), PreparedField::Plain) => {
            let arr = downcast_array::<Time64NanosecondArray>(array)?;
            let nanos = arr.value(idx);
            let secs = u32::try_from(nanos.div_euclid(NANOS_PER_SEC))
                .map_err(|_| ArrowError::ParseError("invalid Time64 value".into()))?;
            let sub = nanos.rem_euclid(NANOS_PER_SEC) as u32;
            match NaiveTime::from_num_seconds_from_midnight_opt(secs, sub) {
                Some(t) => CypherValue::Time(t, None),
                None => return Err(ArrowError::ParseError("invalid Time64 value".into())),
            }
        }
        (DataType::Timestamp(TimeUnit::Nanosecond, _), PreparedField::Timestamp(zone)) => {
            let arr = downcast_array::<TimestampNanosecondArray>(array)?;
            timestamp_to_cypher(arr.value(idx), zone.as_ref())
        }
        (DataType::Interval(IntervalUnit::MonthDayNano), PreparedField::Plain) => {
            let arr = downcast_array::<IntervalMonthDayNanoArray>(array)?;
            interval_to_cypher(arr.value(idx))
        }
        (DataType::Binary, PreparedField::Spatial(srid)) => {
            let arr = downcast_array::<BinaryArray>(array)?;
            wkb_to_cypher_value(arr.value(idx), *srid)?
        }
        (DataType::FixedSizeList(item, _), PreparedField::Vector) => {
            let arr = downcast_array::<FixedSizeListArray>(array)?;
            fixed_size_list_to_vector(item.data_type(), &arr.value(idx))?
        }
        (DataType::FixedSizeList(..), PreparedField::List(child)) => {
            let arr = downcast_array::<FixedSizeListArray>(array)?;
            list_values(child, arr.value(idx).as_ref())?
        }
        (DataType::List(_), PreparedField::List(child)) => {
            let arr = downcast_array::<ListArray>(array)?;
            list_values(child, arr.value(idx).as_ref())?
        }
        (
            DataType::Struct(_),
            PreparedField::Node | PreparedField::Relationship | PreparedField::Struct(_),
        ) => {
            let arr = downcast_array::<StructArray>(array)?;
            struct_row_to_cypher(arr, idx, plan)?
        }
        (DataType::Utf8, PreparedField::Plain | PreparedField::TypedJson) => {
            let arr = downcast_array::<StringArray>(array)?;
            decode_utf8(plan, arr.value(idx))?
        }
        _ => {
            return Err(ArrowError::CastError(format!(
                "Arrow array type {:?} does not match prepared field",
                array.data_type(),
            )))
        }
    })
}

fn list_values(item: &PreparedField, values: &dyn Array) -> Result<CypherValue, ArrowError> {
    let items = (0..values.len())
        .map(|i| array_value_to_cypher(item, values, i))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(CypherValue::List(items))
}

fn timestamp_to_cypher(nanos: i64, tz: Option<&TimestampZone>) -> CypherValue {
    let utc = DateTime::from_timestamp_nanos(nanos);
    match tz {
        None => CypherValue::LocalDateTime(utc.naive_utc()),
        Some(TimestampZone::Fixed(off)) => CypherValue::DateTime(utc.with_timezone(off), None),
        // Named zones cannot be resolved without a tz database: preserve the
        // UTC instant and carry the name for display.
        Some(TimestampZone::Named(name)) => CypherValue::DateTime(
            utc.with_timezone(&FixedOffset::east_opt(0).unwrap()),
            Some(name.clone()),
        ),
    }
}

/// Parse an Arrow timestamp timezone string as a fixed offset (`Z`, `+HH:MM`,
/// `-HH:MM`). Returns `None` for named zones, an error for malformed signed
/// offsets. Called during schema validation, including on empty batches.
fn parse_fixed_offset(z: &str) -> Result<Option<FixedOffset>, ArrowError> {
    if z == "Z" || z.eq_ignore_ascii_case("utc") {
        return Ok(FixedOffset::east_opt(0));
    }
    let bytes = z.as_bytes();
    if !matches!(bytes.first(), Some(b'+' | b'-')) {
        return Ok(None);
    }
    parse_numeric_offset(z)
        .map(Some)
        .ok_or_else(|| ArrowError::ParseError(format!("invalid timestamp timezone offset '{z}'")))
}

fn interval_to_cypher(iv: IntervalMonthDayNano) -> CypherValue {
    let seconds = iv.nanoseconds.div_euclid(NANOS_PER_SEC);
    let nanoseconds = iv.nanoseconds.rem_euclid(NANOS_PER_SEC);
    CypherValue::Duration(IsoDuration::new(
        iv.months as i64,
        iv.days as i64,
        seconds,
        nanoseconds,
    ))
}

fn fixed_size_list_to_vector(
    child_dt: &DataType,
    child: &dyn Array,
) -> Result<CypherValue, ArrowError> {
    if (0..child.len()).any(|i| child.is_null(i)) {
        return Err(ArrowError::ParseError("null vector coordinate".into()));
    }
    let dim = child.len();
    // The arrow child array is already the packed per-family form
    // (ADR-0706): one helper per Arrow primitive carries the collect, so
    // each dispatch arm is a line naming its kind.
    let (coord, coords) = match child_dt {
        DataType::Int8 => (
            VectorCoordKind::Int8,
            int_coords::<arrow_array::types::Int8Type>(child, dim)?,
        ),
        DataType::Int16 => (
            VectorCoordKind::Int16,
            int_coords::<arrow_array::types::Int16Type>(child, dim)?,
        ),
        DataType::Int32 => (
            VectorCoordKind::Int32,
            int_coords::<arrow_array::types::Int32Type>(child, dim)?,
        ),
        DataType::Int64 => (
            VectorCoordKind::Int64,
            int_coords::<arrow_array::types::Int64Type>(child, dim)?,
        ),
        DataType::Float32 => (
            VectorCoordKind::Float32,
            VectorCoords::from_f32(
                float_coords::<arrow_array::types::Float32Type>(child, dim)?
                    .into_iter()
                    .map(|v| v as f32)
                    .collect::<Vec<_>>(),
            ),
        ),
        DataType::Float64 => (
            VectorCoordKind::Float64,
            VectorCoords::from_f64(float_coords::<arrow_array::types::Float64Type>(child, dim)?),
        ),
        _ => {
            return Err(ArrowError::CastError(format!(
                "unsupported vector child type {child_dt:?}"
            )))
        }
    };
    Ok(CypherValue::vector(coord, coords))
}

/// The integer families' one collect: downcast to the column's Arrow
/// primitive, widen each value to `i64` (the packed `Int` family), and
/// pair it with its `VectorCoordKind`. `T::Native: Into<i64>` holds for
/// every Arrow integer type, so one body serves all four.
fn int_coords<T>(child: &dyn Array, dim: usize) -> Result<VectorCoords, ArrowError>
where
    T: arrow_array::types::ArrowPrimitiveType,
    T::Native: Into<i64>,
{
    let a = downcast_array::<arrow_array::PrimitiveArray<T>>(child)?;
    let xs = (0..dim).map(|i| a.value(i).into()).collect::<Vec<_>>();
    Ok(VectorCoords::from_int(xs))
}

/// The float families' one collect: downcast, widen each value to `f64`,
/// reject a non-finite coordinate with the one typed message, and return
/// the widened vector — each dispatch arm builds its own family from it
/// (one buffer, never two).
fn float_coords<T>(child: &dyn Array, dim: usize) -> Result<Vec<f64>, ArrowError>
where
    T: arrow_array::types::ArrowPrimitiveType,
    T::Native: Into<f64>,
{
    let a = downcast_array::<arrow_array::PrimitiveArray<T>>(child)?;
    let mut xs = Vec::with_capacity(dim);
    for i in 0..dim {
        let v = a.value(i).into();
        if !v.is_finite() {
            return Err(ArrowError::ParseError(format!(
                "non-finite vector coordinate {v} at {i}"
            )));
        }
        xs.push(v);
    }
    Ok(xs)
}

fn labels_at(labels: &ListArray, idx: usize) -> Result<Vec<String>, ArrowError> {
    if labels.is_null(idx) {
        return Err(ArrowError::ParseError("null node labels".into()));
    }
    let inner = labels.value(idx);
    let sa = inner
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| ArrowError::CastError("node labels require List<Utf8>".into()))?;
    (0..sa.len())
        .map(|i| {
            if sa.is_null(i) {
                Err(ArrowError::ParseError("null node label".into()))
            } else {
                Ok(sa.value(i).to_string())
            }
        })
        .collect()
}

fn struct_child<'a, T: Array + 'static>(
    arr: &'a StructArray,
    name: &str,
    idx: usize,
) -> Result<&'a T, ArrowError> {
    let child = arr
        .column_by_name(name)
        .ok_or_else(|| ArrowError::ParseError(format!("missing {name} child")))?;
    let value = child
        .as_any()
        .downcast_ref::<T>()
        .ok_or_else(|| ArrowError::CastError(format!("wrong type for {name} child")))?;
    if value.is_null(idx) {
        return Err(ArrowError::ParseError(format!("null {name} child")));
    }
    Ok(value)
}

fn struct_i64(arr: &StructArray, name: &str, idx: usize) -> Result<i64, ArrowError> {
    Ok(struct_child::<Int64Array>(arr, name, idx)?.value(idx))
}

fn struct_str(arr: &StructArray, name: &str, idx: usize) -> Result<String, ArrowError> {
    Ok(struct_child::<StringArray>(arr, name, idx)?
        .value(idx)
        .to_string())
}

fn struct_props(
    arr: &StructArray,
    idx: usize,
) -> Result<IndexMap<String, CypherValue>, ArrowError> {
    let props = struct_child::<StringArray>(arr, "properties", idx)?;
    typed_json::decode_props(props.value(idx))
        .map_err(|e| ArrowError::ParseError(format!("invalid properties: {e}")))
}

fn struct_row_to_cypher(
    arr: &StructArray,
    idx: usize,
    plan: &PreparedField,
) -> Result<CypherValue, ArrowError> {
    match plan {
        PreparedField::Node => Ok(CypherValue::Node {
            id: struct_i64(arr, "id", idx)?,
            labels: labels_at(struct_child::<ListArray>(arr, "labels", idx)?, idx)?,
            properties: struct_props(arr, idx)?,
        }),
        PreparedField::Relationship => Ok(CypherValue::Relationship {
            id: struct_i64(arr, "id", idx)?,
            start_id: struct_i64(arr, "start_node", idx)?,
            end_id: struct_i64(arr, "end_node", idx)?,
            rel_type: struct_str(arr, "type", idx)?,
            properties: struct_props(arr, idx)?,
        }),
        PreparedField::Struct(children) => {
            let mut map = IndexMap::new();
            for (i, f) in arr.fields().iter().enumerate() {
                let child = children.get(i).ok_or_else(|| {
                    ArrowError::CastError(format!("missing prepared child for '{}'", f.name()))
                })?;
                map.insert(
                    f.name().clone(),
                    array_value_to_cypher(child, arr.column(i).as_ref(), idx)?,
                );
            }
            Ok(CypherValue::Map(map))
        }
        _ => Err(ArrowError::CastError("expected prepared Struct".into())),
    }
}

/// Only exact `typed-json-v1` metadata permits JSON decoding. Bare Utf8 is a
/// verbatim string; legacy untagged composites have no safe graph provenance.
fn decode_utf8(plan: &PreparedField, s: &str) -> Result<CypherValue, ArrowError> {
    match plan {
        PreparedField::TypedJson => typed_json::decode(s),
        PreparedField::Plain => Ok(CypherValue::Str(s.to_string())),
        _ => Err(ArrowError::CastError("expected prepared Utf8".into())),
    }
}

#[cfg(test)]
#[path = "from_arrow/tests.rs"]
mod tests;
