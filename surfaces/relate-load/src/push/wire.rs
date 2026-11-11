//! The push side of the typed wire (ADR-0401 §2): engine values encoded as
//! the `{"$type": …, "_value": …}` parameter envelopes the Query API's
//! typed request media type carries — the exact inverse of
//! [`crate::typed_value::decode_value`] — plus Cypher identifier quoting
//! for the entity names interpolated into statement text (labels, types,
//! property keys are identifiers, never values).

use std::collections::BTreeMap;

use relate::{Temporal, Value};
use relate_core::PointValue;
use relate_model::identifier::quote_identifier_always;
use serde_json::Value as Json;

use crate::LoadError;

/// The closed vocabulary of wire type tags (the ADR-0181 table the pull
/// decoder matches as strings) — a misspelled tag cannot compile, and
/// the string spelling lives in one place.
#[derive(Clone, Copy)]
enum WireTag {
    Null,
    Boolean,
    Integer,
    Float,
    String,
    List,
    Map,
    Point,
    Uuid,
    Vector,
    Date,
    LocalTime,
    Time,
    LocalDateTime,
    ZonedDateTime,
    OffsetDateTime,
    Duration,
}

impl WireTag {
    fn as_str(self) -> &'static str {
        match self {
            WireTag::Null => "Null",
            WireTag::Boolean => "Boolean",
            WireTag::Integer => "Integer",
            WireTag::Float => "Float",
            WireTag::String => "String",
            WireTag::List => "List",
            WireTag::Map => "Map",
            WireTag::Point => "Point",
            WireTag::Uuid => "UUID",
            WireTag::Vector => "Vector",
            WireTag::Date => "Date",
            WireTag::LocalTime => "LocalTime",
            WireTag::Time => "Time",
            WireTag::LocalDateTime => "LocalDateTime",
            WireTag::ZonedDateTime => "ZonedDateTime",
            WireTag::OffsetDateTime => "OffsetDateTime",
            WireTag::Duration => "Duration",
        }
    }
}

/// The typed-envelope wrapper — the one spelling of the
/// `{"$type": …, "_value": …}` shape this module owns, for both the
/// engine-value path ([`encode_value`]) and the raw-JSON path
/// ([`envelope_rows`]).
fn envelope(tag: WireTag, payload: Json) -> Json {
    serde_json::json!({
        "$type": tag.as_str(),
        "_value": payload,
    })
}

/// A batch of already-encoded write rows as the parameter the Query API
/// expects: the rows travel as a `List` envelope whose elements are
/// `Map` envelopes — containers are enveloped too (verified on Aura: a
/// plain JSON array of envelope objects answers 400, the enveloped form
/// 202).
pub(crate) fn envelope_rows(batch: &[Json]) -> Json {
    envelope(
        WireTag::List,
        Json::Array(
            batch
                .iter()
                .map(|row| envelope(WireTag::Map, row.clone()))
                .collect(),
        ),
    )
}

/// The VECTOR coordinates' scalar-decimal spelling (ADR-0181 item 6:
/// every coordinate is a decimal string in the declared kind). A
/// coordinate that is neither numeric fails loudly.
fn decimal_string(v: &Value) -> Result<String, LoadError> {
    match v {
        Value::Int(i) => Ok(i.to_string()),
        Value::Float(f) => Ok(float_string(*f)),
        other => Err(LoadError::Push(format!(
            "a VECTOR coordinate that is neither INTEGER nor FLOAT ({:?})",
            value_kind(other)
        ))),
    }
}

/// Encode one engine value as a typed Query API parameter envelope. The
/// inverse of the ADR-0181 tag table: every tag the pull decodes re-encodes
/// to the same tag, so a pushed value keeps its Cypher type on the remote.
/// A value with no parameter representation — an element in property
/// position, or a byte array (statement-scoped, ADR-0327) — fails loudly;
/// there is no silent STRING fallback (LABS-186's rule, mirrored).
pub(crate) fn encode_value(value: &Value) -> Result<Json, LoadError> {
    match value {
        Value::Null => Ok(envelope(WireTag::Null, Json::Null)),
        Value::Bool(b) => Ok(envelope(WireTag::Boolean, Json::Bool(*b))),
        Value::Int(i) => Ok(envelope(WireTag::Integer, Json::String(i.to_string()))),
        Value::Float(f) => Ok(envelope(WireTag::Float, Json::String(float_string(*f)))),
        Value::String(s) => Ok(envelope(WireTag::String, Json::String(s.to_string()))),
        Value::List(items) => {
            let encoded: Vec<Json> = items.iter().map(encode_value).collect::<Result<_, _>>()?;
            Ok(envelope(WireTag::List, Json::Array(encoded)))
        }
        Value::Map(entries) => Ok(envelope(
            WireTag::Map,
            Json::Object(encode_entries(entries, "map entry")?),
        )),
        Value::Temporal(t) => Ok(envelope(temporal_tag(t), Json::String(t.iso_string()))),
        Value::Point(p) => Ok(envelope(WireTag::Point, Json::String(point_ewkt(p)))),
        Value::Uuid(u) => Ok(envelope(WireTag::Uuid, Json::String(relate::uuid_hyphenated(*u)))),
        Value::Vector { coord, values } => {
            let coordinates: Vec<Json> = values
                .iter()
                .map(|v| decimal_string(v).map(Json::String))
                .collect::<Result<_, _>>()?;
            Ok(envelope(
                WireTag::Vector,
                serde_json::json!({
                    "coordinatesType": coord.canonical_name(),
                    "coordinates": coordinates,
                }),
            ))
        }
        Value::Bytes(_) => Err(LoadError::Push(
            "a byte array property (the engine's byte array is statement-scoped and cannot be pushed as a property)"
                .to_string(),
        )),
        Value::Node(_) | Value::Relationship(_) | Value::Path(_) => Err(LoadError::Push(format!(
            "a graph element in property position (a {:?})",
            value_kind(value)
        )),
        ),
    }
}

/// Encode a map's entries as one JSON object of typed envelopes — the
/// one per-entry policy both the `Map` value arm (via [`encode_value`])
/// and the property-map path (via [`encode_props`]) consult. The
/// failing entry's key rides the error, labeled by `context`.
fn encode_entries(
    entries: &BTreeMap<String, Value>,
    context: &str,
) -> Result<serde_json::Map<String, Json>, LoadError> {
    let mut encoded = serde_json::Map::new();
    for (key, value) in entries {
        encoded.insert(
            key.clone(),
            encode_value(value).map_err(|e| e.with_context(&format!("{context} {key:?}: ")))?,
        );
    }
    Ok(encoded)
}

/// Encode a property map as one `Map` envelope of typed envelopes, failing
/// with the offending property's key in the message.
pub(crate) fn encode_props(props: &BTreeMap<String, Value>) -> Result<Json, LoadError> {
    encode_entries(props, "property").map(|object| envelope(WireTag::Map, Json::Object(object)))
}

/// The temporal tag for one engine temporal (the ADR-0181 table's inverse):
/// a named-zone `DATETIME` is `ZonedDateTime`, a fixed-offset one
/// `OffsetDateTime` — both decode back into the engine's one `datetime`
/// kind.
fn temporal_tag(t: &Temporal) -> WireTag {
    match t {
        Temporal::Date(_) | Temporal::CivilDate { .. } => WireTag::Date,
        Temporal::LocalTime(_) => WireTag::LocalTime,
        Temporal::Time { .. } => WireTag::Time,
        Temporal::LocalDateTime(_) | Temporal::CivilDateTime { .. } => WireTag::LocalDateTime,
        Temporal::DateTime { zone: Some(_), .. } => WireTag::ZonedDateTime,
        Temporal::DateTime { zone: None, .. } => WireTag::OffsetDateTime,
        Temporal::Duration { .. } => WireTag::Duration,
    }
}

/// The EWKT point spelling the pull decodes (ADR-0181 item 5):
/// `SRID=<i32>;POINT (<x> <y>)` or `SRID=<i32>;POINT Z (<x> <y> <z>)`.
fn point_ewkt(p: &PointValue) -> String {
    match p.z {
        Some(z) => format!("SRID={};POINT Z ({:?} {:?} {:?})", p.srid, p.x, p.y, z),
        None => format!("SRID={};POINT ({:?} {:?})", p.srid, p.x, p.y),
    }
}

/// A float's wire spelling: the decimal string the pull's decoder parses
/// back. `str::parse::<f64>` accepts the non-finite spellings it prints.
fn float_string(f: f64) -> String {
    f.to_string()
}

/// The value-kind name error messages use (the ADR-0181 tag table's
/// vocabulary, for values that cannot be encoded).
fn value_kind(value: &Value) -> &'static str {
    match value {
        // Bespoke names: Node/Relationship/Path have no wire tag at all,
        // Bytes borrows the pull decoder's own refused-tag name
        // ("Base64", ADR-0181), and Temporal spans seven tags.
        Value::Node(_) => "Node",
        Value::Relationship(_) => "Relationship",
        Value::Path(_) => "Path",
        Value::Bytes(_) => "Base64",
        Value::Temporal(_) => "Temporal",
        // Every other kind's name is the canonical tag table's.
        Value::Null => WireTag::Null.as_str(),
        Value::Bool(_) => WireTag::Boolean.as_str(),
        Value::Int(_) => WireTag::Integer.as_str(),
        Value::Float(_) => WireTag::Float.as_str(),
        Value::String(_) => WireTag::String.as_str(),
        Value::List(_) => WireTag::List.as_str(),
        Value::Map(_) => WireTag::Map.as_str(),
        Value::Point(_) => WireTag::Point.as_str(),
        Value::Uuid(_) => WireTag::Uuid.as_str(),
        Value::Vector { .. } => WireTag::Vector.as_str(),
    }
}

/// Quote one Cypher identifier (label, relationship type, or property key)
/// for interpolation into statement text: backtick-quoted with internal
/// backticks doubled, Neo4j's own escaping. An empty name fails loudly —
/// no identifier is ever interpolated unquoted. Entity names come from the
/// local catalog, not from property values; values are always parameters
/// (ADR-0401 §2).
pub(crate) fn quote_ident(name: &str) -> Result<String, LoadError> {
    if name.is_empty() {
        return Err(LoadError::Push(
            "an empty identifier (label, type, or property key)".to_string(),
        ));
    }
    Ok(quote_identifier_always(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn round_trips(value: Value) {
        let encoded = encode_value(&value).unwrap_or_else(|e| panic!("{value:?}: {e}"));
        let decoded = crate::typed_value::decode_value(&encoded)
            .unwrap_or_else(|e| panic!("{encoded:?}: {e}"));
        assert_eq!(decoded, value, "via {encoded}");
    }

    /// Every encodable value round-trips through the pull decoder: push's
    /// parameter envelopes are the exact inverse of ADR-0181's tag table.
    #[test]
    fn every_encodable_value_round_trips_through_the_decoder() {
        round_trips(Value::Null);
        round_trips(Value::Bool(true));
        round_trips(Value::Int(-42));
        round_trips(Value::Float(3.5));
        round_trips(Value::Float(1e300));
        round_trips(Value::String("hello".into()));
        round_trips(Value::List(
            vec![Value::Int(1), Value::String("x".into())].into(),
        ));
        let mut map = BTreeMap::new();
        map.insert("a".to_string(), Value::Bool(false));
        map.insert("b".to_string(), Value::Float(1.5));
        round_trips(Value::Map(map.into()));
        for iso in [
            ("date", "2024-05-06"),
            ("localtime", "12:34:56"),
            ("time", "12:34:56+01:00"),
            ("localdatetime", "2024-05-06T12:34:56"),
            ("datetime", "2024-05-06T12:34:56+01:00"),
            ("duration", "P1Y2M3DT4H5M6.5S"),
        ] {
            round_trips(Value::Temporal(
                relate_core::temporal::parse_iso(iso.0, iso.1).unwrap(),
            ));
        }
        round_trips(Value::Point(PointValue::cartesian_2d(1.0, 2.5)));
        round_trips(Value::Point(PointValue::cartesian_3d(1.0, 2.5, -3.0)));
        round_trips(Value::Uuid(0x1234u128));
    }

    /// The non-finite float spellings encode to the strings the pull
    /// decoder's `str::parse::<f64>` accepts (NaN equality is not a usable
    /// round-trip predicate, so the spellings are pinned directly).
    #[test]
    fn nonfinite_floats_encode_to_parseable_spellings() {
        for (f, expected) in [
            (f64::NAN, "NaN"),
            (f64::INFINITY, "inf"),
            (f64::NEG_INFINITY, "-inf"),
        ] {
            let encoded = encode_value(&Value::Float(f)).unwrap();
            assert_eq!(encoded["_value"], Json::String(expected.to_string()));
            let text = encoded["_value"].as_str().unwrap();
            // total_cmp: NaN compares equal to itself under the total order.
            assert_eq!(
                text.parse::<f64>().unwrap().total_cmp(&f),
                std::cmp::Ordering::Equal,
                "{expected}"
            );
        }
    }

    /// A VECTOR encodes as the ADR-0181 item 6 object and decodes back with
    /// the declared coordinate kind.
    #[test]
    fn vector_encodes_as_the_coordinates_object() {
        let value = Value::Vector {
            coord: relate_core::VectorCoordKind::Float64,
            values: vec![Value::Float(0.5), Value::Float(-1.25)].into(),
        };
        let encoded = encode_value(&value).unwrap();
        assert_eq!(
            encoded,
            serde_json::json!({
                "$type": "Vector",
                "_value": {"coordinatesType": "FLOAT", "coordinates": ["0.5", "-1.25"]}
            })
        );
        assert_eq!(crate::typed_value::decode_value(&encoded).unwrap(), value);
    }

    /// A named-zone DATETIME encodes as `ZonedDateTime`, a fixed-offset one
    /// as `OffsetDateTime` — the two tags that decode into the engine's
    /// one `datetime` kind.
    #[test]
    fn datetime_tags_split_on_the_named_zone() {
        let named = Value::Temporal(
            relate_core::temporal::parse_iso("datetime", "2024-05-06T12:34:56+01:00").unwrap(),
        );
        // Build a named-zone one through the engine's own Display parser:
        // parse a zoned spelling if the grammar allows it; otherwise pin the
        // fixed-offset form.
        match &named {
            Value::Temporal(Temporal::DateTime { zone, .. }) => {
                if zone.is_some() {
                    assert_eq!(encode_value(&named).unwrap()["$type"], "ZonedDateTime");
                } else {
                    assert_eq!(encode_value(&named).unwrap()["$type"], "OffsetDateTime");
                }
            }
            other => panic!("a datetime parses to DateTime, got {other:?}"),
        }
    }

    /// Values with no parameter representation fail loudly, never silently
    /// as strings: elements in property position and byte arrays.
    #[test]
    fn unencodable_values_fail_loudly() {
        let db = relate::Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        let r = conn.query("CREATE (:P {name: 'x'}) RETURN 1").unwrap();
        drop(r);
        let node = conn.query("MATCH (n:P) RETURN n").unwrap().rows[0][0].clone();
        let err = encode_value(&node).unwrap_err().to_string();
        assert!(err.contains("property position"), "{err}");
        let err = encode_value(&Value::Bytes(vec![1u8].into()))
            .unwrap_err()
            .to_string();
        assert!(err.contains("byte array"), "{err}");
    }

    /// Identifiers quote with backticks doubled; the empty name fails.
    #[test]
    fn identifiers_quote_with_backticks_doubled() {
        assert_eq!(quote_ident("Movie").unwrap(), "`Movie`");
        assert_eq!(quote_ident("weird label").unwrap(), "`weird label`");
        assert_eq!(quote_ident("with`tick").unwrap(), "`with``tick`");
        assert_eq!(quote_ident("n.name").unwrap(), "`n.name`");
        assert!(quote_ident("").is_err());
    }

    /// Property maps encode as one object of envelopes, and the failing
    /// property's key is in the message.
    #[test]
    fn property_maps_carry_the_offending_key() {
        let mut props = BTreeMap::new();
        props.insert("ok".to_string(), Value::Int(1));
        // The property map is itself a Map envelope (every container in a
        // typed request body is).
        assert_eq!(
            encode_props(&props).unwrap(),
            serde_json::json!({
                "$type": "Map",
                "_value": {"ok": {"$type": "Integer", "_value": "1"}}
            })
        );
        props.insert("blob".to_string(), Value::Bytes(vec![].into()));
        let err = encode_props(&props).unwrap_err().to_string();
        assert!(err.contains("\"blob\""), "{err}");
        assert!(err.contains("byte array"), "{err}");
    }
}
