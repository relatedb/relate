//! Decoder for the typed Query API v2 wire format that `relate pull`
//! negotiates (ADR-0181): `{"$type": …, "_value": …}` → [`relate::Value`].
//!
//! The decoder is the pure value half of the pull path: it sees one value
//! and never an element id or a property key — the caller attaches those
//! when it builds a [`crate::TypedWireError`] (ADR-0181 item 7). Values the
//! engine cannot represent (`Base64`, `Unsupported`, element tags in
//! property position, unknown tags) and payloads that do not match their
//! tag's shape fail loudly; there is no silent STRING fallback (LABS-186).

use std::collections::BTreeMap;

use relate::Value;
use relate_core::temporal::parse_iso;
use relate_core::{uuid_parse, VectorCoordKind};
use serde_json::Value as Json;

/// Why one `$type`-tagged value could not be decoded.
///
/// Carries no element id and no property key: the decoder never sees
/// either. The caller attaches both when it converts this into a
/// [`crate::TypedWireError`] (ADR-0181 item 7).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    /// The `$type` tag has no engine value: `Base64` (no byte-array
    /// value), `Unsupported` (the server could not serialize the value),
    /// `Node`/`Relationship`/`Path` in property position, or an unknown
    /// tag.
    #[error("unsupported typed value {tag:?}")]
    Unsupported { tag: String },
    /// The tag is known but `_value` is missing or does not have the
    /// tag's payload shape.
    #[error("malformed {tag} value: {detail}")]
    Malformed { tag: String, detail: String },
    /// No `$type` tag where the typed wire contract requires one.
    #[error("untagged value where a $type-tagged value was expected")]
    Untagged,
}

/// Decode one `$type`-tagged Query API v2 value into the engine value its
/// tag names (ADR-0181 item 3).
///
/// The input is the `{"$type": …, "_value": …}` envelope the typed wire
/// carries for every cell and property value; the output is the
/// `relate::Value` the ADR's tag table assigns (`List` and `Map`
/// recurse element-wise). A value the engine cannot represent, or one
/// whose `_value` does not match its tag, returns a [`DecodeError`] that
/// names the failing tag. The owning element id and property key are not
/// known here; the caller attaches them via
/// [`crate::TypedWireError::with_context`] (ADR-0181 item 7).
pub fn decode_value(value: &Json) -> Result<Value, DecodeError> {
    let obj = value.as_object().ok_or(DecodeError::Untagged)?;
    let tag = obj
        .get("$type")
        .and_then(Json::as_str)
        .ok_or(DecodeError::Untagged)?;
    let payload = obj
        .get("_value")
        .ok_or_else(|| malformed(tag, "missing \"_value\""))?;
    match tag {
        "Null" => Ok(Value::Null),
        "Boolean" => payload
            .as_bool()
            .map(Value::Bool)
            .ok_or_else(|| malformed(tag, "expected a JSON boolean \"_value\"")),
        "Integer" => integer(payload, tag),
        "Float" => float(payload, tag),
        "String" => payload
            .as_str()
            .map(|s| Value::String(s.into()))
            .ok_or_else(|| malformed(tag, "expected a JSON string \"_value\"")),
        "Date" => temporal("date", payload, tag),
        "Time" => temporal("time", payload, tag),
        "LocalTime" => temporal("localtime", payload, tag),
        "LocalDateTime" => temporal("localdatetime", payload, tag),
        "OffsetDateTime" | "ZonedDateTime" => temporal("datetime", payload, tag),
        "Duration" => temporal("duration", payload, tag),
        "Point" => point(payload, tag),
        "UUID" => uuid(payload, tag),
        "Vector" => vector(payload, tag),
        "List" => list(payload, tag),
        "Map" => map(payload, tag),
        // Element tags are valid in cell position only; a property value
        // carrying one is an unsupported typed value (ADR-0181 item 3).
        "Node" | "Relationship" | "Path" => Err(DecodeError::Unsupported {
            tag: tag.to_string(),
        }),
        // Base64 has no engine representation, and `Unsupported` is the
        // server's own "could not serialize" marker (ADR-0181 item 7).
        "Base64" | "Unsupported" => Err(DecodeError::Unsupported {
            tag: tag.to_string(),
        }),
        other => Err(DecodeError::Unsupported {
            tag: other.to_string(),
        }),
    }
}

fn malformed(tag: &str, detail: impl Into<String>) -> DecodeError {
    DecodeError::Malformed {
        tag: tag.to_string(),
        detail: detail.into(),
    }
}

fn expect_str<'a>(payload: &'a Json, tag: &str) -> Result<&'a str, DecodeError> {
    payload
        .as_str()
        .ok_or_else(|| malformed(tag, "expected a JSON string \"_value\""))
}

/// `Integer._value` is a decimal JSON string, parsed as `i64` (ADR-0181
/// item 4); a JSON number or any non-`i64` string is malformed.
fn integer(payload: &Json, tag: &str) -> Result<Value, DecodeError> {
    let s = expect_str(payload, tag)?;
    s.parse::<i64>()
        .map(Value::Int)
        .map_err(|_| malformed(tag, format!("{s:?} is not a decimal i64 string")))
}

/// `Float._value` is a JSON string (ADR-0181 item 4); `str::parse::<f64>()`
/// already accepts the wire's three non-finite spellings (`NaN`, `Infinity`,
/// `-Infinity`, case-insensitively and with an optional sign), so no literal
/// arm is needed for them.
fn float(payload: &Json, tag: &str) -> Result<Value, DecodeError> {
    let s = expect_str(payload, tag)?;
    s.parse::<f64>()
        .map(Value::Float)
        .map_err(|_| malformed(tag, format!("{s:?} is not a float string")))
}

/// The six temporal tags share one payload shape: an ISO-8601 string fed
/// to the engine's `parse_iso` under the ADR-0181 item 3 kind
/// (`OffsetDateTime` and `ZonedDateTime` are both the one `datetime` kind).
fn temporal(kind: &str, payload: &Json, tag: &str) -> Result<Value, DecodeError> {
    let s = expect_str(payload, tag)?;
    parse_iso(kind, s)
        .map(Value::Temporal)
        .map_err(|e| malformed(tag, format!("{s:?}: {e}")))
}

/// The EWKT point grammar of ADR-0181 item 5 — the one parser
/// [`relate_model::point::parse_ewkt`] owns; this arm only wraps its detail
/// in the decoder's `malformed` shape.
fn point(payload: &Json, tag: &str) -> Result<Value, DecodeError> {
    let s = expect_str(payload, tag)?;
    relate_model::point::parse_ewkt(s)
        .map(Value::Point)
        .map_err(|detail| malformed(tag, detail.to_string()))
}

/// `UUID._value` is the canonical hyphenated string; `None` from
/// [`uuid_parse`] is malformed (ADR-0181 item 3).
fn uuid(payload: &Json, tag: &str) -> Result<Value, DecodeError> {
    let s = expect_str(payload, tag)?;
    uuid_parse(s)
        .map(Value::Uuid)
        .ok_or_else(|| malformed(tag, format!("{s:?} is not a canonical hyphenated UUID")))
}

/// `Vector._value` is `{"coordinatesType": …, "coordinates": […]}`
/// (ADR-0181 item 6): the six Neo4j spellings resolve through
/// [`VectorCoordKind::from_alias`], and each string coordinate parses as
/// the declared kind (`i64` for the integer kinds, `f64` for the floats).
fn vector(payload: &Json, tag: &str) -> Result<Value, DecodeError> {
    let obj = payload
        .as_object()
        .ok_or_else(|| malformed(tag, "expected an object \"_value\""))?;
    let kind = obj
        .get("coordinatesType")
        .and_then(Json::as_str)
        .ok_or_else(|| malformed(tag, "missing string \"coordinatesType\""))?;
    let coord = VectorCoordKind::from_alias(kind)
        .ok_or_else(|| malformed(tag, format!("unknown coordinatesType {kind:?}")))?;
    let coords = obj
        .get("coordinates")
        .and_then(Json::as_array)
        .ok_or_else(|| malformed(tag, "missing array \"coordinates\""))?;
    let mut values = Vec::with_capacity(coords.len());
    for token in coords {
        let s = token
            .as_str()
            .ok_or_else(|| malformed(tag, "coordinates entries must be strings"))?;
        let value = if coord.is_integer() {
            Value::Int(s.parse().map_err(|_| {
                malformed(
                    tag,
                    format!("{s:?} is not an i64 for coordinatesType {kind:?}"),
                )
            })?)
        } else {
            Value::Float(s.parse().map_err(|_| {
                malformed(
                    tag,
                    format!("{s:?} is not an f64 for coordinatesType {kind:?}"),
                )
            })?)
        };
        values.push(value);
    }
    Ok(Value::Vector {
        coord,
        values: values.into(),
    })
}

fn list(payload: &Json, tag: &str) -> Result<Value, DecodeError> {
    let items = payload
        .as_array()
        .ok_or_else(|| malformed(tag, "expected an array \"_value\""))?
        .iter()
        .map(decode_value)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Value::List(items.into()))
}

fn map(payload: &Json, tag: &str) -> Result<Value, DecodeError> {
    let obj = payload
        .as_object()
        .ok_or_else(|| malformed(tag, "expected an object \"_value\""))?;
    let mut entries = BTreeMap::new();
    for (key, value) in obj {
        entries.insert(key.clone(), decode_value(value)?);
    }
    Ok(Value::Map(entries.into()))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use relate::{Database, Temporal, Value};
    use relate_core::PointValue;
    use serde_json::json;

    use super::*;

    /// The verbatim captured typed Query API v2 responses (ADR-0181).
    const FIXTURE: &str = include_str!("../tests/fixtures/typed_wire.json");

    const IMAGES: [&str; 2] = ["neo4j:2026.09-enterprise", "neo4j:5.26-enterprise"];

    fn fixture() -> Json {
        serde_json::from_str(FIXTURE).expect("the typed wire fixture parses")
    }

    fn tagged(tag: &str, value: Json) -> Json {
        json!({ "$type": tag, "_value": value })
    }

    fn decode(tag: &str, value: Json) -> Result<Value, DecodeError> {
        decode_value(&tagged(tag, value))
    }

    /// Every property value of both captured `mappable` bodies, keyed by
    /// property name. The first occurrence wins: the main node carries
    /// `name: "mappable"` and the peer node repeats the key with `"peer"`.
    fn mappable_properties() -> BTreeMap<String, Value> {
        let fixture = fixture();
        let mut out = BTreeMap::new();
        for image in IMAGES {
            let body = &fixture["servers"][image]["mappable"];
            for row in body["data"]["values"].as_array().expect("rows") {
                for cell in row.as_array().expect("cells") {
                    if !matches!(cell["$type"].as_str(), Some("Node" | "Relationship")) {
                        continue;
                    }
                    let properties = cell["_value"]["_properties"]
                        .as_object()
                        .expect("properties object");
                    for (key, value) in properties {
                        let decoded = decode_value(value)
                            .unwrap_or_else(|error| panic!("{image} property {key:?}: {error}"));
                        out.entry(key.clone()).or_insert(decoded);
                    }
                }
            }
        }
        out
    }

    /// Every property cell of both captured `mappable` bodies decodes, and
    /// the values keep the engine types of ADR-0181 item 3.
    // The captured float happens to start with pi's digits; it is the
    // fixture's value, not the constant.
    #[allow(clippy::approx_constant)]
    #[test]
    fn mappable_property_cells_decode_to_engine_values() {
        let properties = mappable_properties();
        // 30 node properties plus the relationship's `at`/`since`; the
        // peer node only repeats `name`.
        assert_eq!(properties.len(), 32, "{properties:?}");

        assert_eq!(properties["name"], Value::String("mappable".into()));
        assert_eq!(properties["string"], Value::String("hello".into()));
        assert_eq!(properties["boolean"], Value::Bool(true));
        assert_eq!(properties["integer"], Value::Int(42));
        assert_eq!(properties["float"], Value::Float(3.14159));
        assert_eq!(
            properties["uuid"],
            Value::Uuid(0x550e_8400_e29b_41d4_a716_4466_5544_0000)
        );
        assert_eq!(
            properties["point_wgs84"],
            Value::Point(PointValue {
                srid: 4326,
                x: -0.1278,
                y: 51.5074,
                z: None,
            })
        );
        assert_eq!(
            properties["point_wgs84_3d"],
            Value::Point(PointValue {
                srid: 4979,
                x: -0.1278,
                y: 51.5074,
                z: Some(11.0),
            })
        );
        assert_eq!(
            properties["point_cartesian_3d"],
            Value::Point(PointValue {
                srid: 9157,
                x: 1.0,
                y: 2.0,
                z: Some(3.0),
            })
        );
        assert_eq!(
            properties["at"],
            Value::Point(PointValue {
                srid: 4326,
                x: 10.0,
                y: 20.0,
                z: None,
            })
        );
        assert_eq!(
            properties["vector"],
            Value::Vector {
                coord: VectorCoordKind::Float64,
                values: vec![Value::Float(1.0), Value::Float(2.0)].into(),
            }
        );
        assert_eq!(
            properties["list_integer"],
            Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)].into())
        );
        assert_eq!(
            properties["list_float"],
            Value::List(vec![Value::Float(1.5), Value::Float(2.5)].into())
        );
        assert_eq!(
            properties["list_boolean"],
            Value::List(vec![Value::Bool(true), Value::Bool(false)].into())
        );
        assert_eq!(
            properties["list_string"],
            Value::List(vec![Value::String("a".into()), Value::String("b".into())].into())
        );

        // A List of Date is a List of Value::Temporal.
        let Value::List(dates) = &properties["list_date"] else {
            panic!("list_date: {:?}", properties["list_date"]);
        };
        assert_eq!(dates.len(), 2);
        assert!(dates
            .iter()
            .all(|v| matches!(v, Value::Temporal(Temporal::Date(_)))));

        // The remaining list properties are lists of their element types.
        for (key, is_temporal) in [
            ("list_time", true),
            ("list_localtime", true),
            ("list_localdatetime", true),
            ("list_datetime", true),
            ("list_duration", true),
            ("list_point", false),
        ] {
            let Value::List(items) = &properties[key] else {
                panic!("{key}: {:?}", properties[key]);
            };
            assert!(!items.is_empty(), "{key}");
            assert!(
                items.iter().all(|v| if is_temporal {
                    matches!(v, Value::Temporal(_))
                } else {
                    matches!(v, Value::Point(_))
                }),
                "{key}: {items:?}"
            );
        }

        // list_uuid is a list of UUIDs (2026.09 only).
        let Value::List(uuids) = &properties["list_uuid"] else {
            panic!("list_uuid: {:?}", properties["list_uuid"]);
        };
        assert!(uuids.iter().all(|v| matches!(v, Value::Uuid(_))));
    }

    /// `Integer` and `Float` payloads are JSON strings (ADR-0181 item 4):
    /// decimal i64, scientific/fractional f64, and the three non-finite
    /// spellings.
    #[test]
    fn numeric_payloads_are_json_strings() {
        assert_eq!(decode("Integer", json!("123")).unwrap(), Value::Int(123));
        assert_eq!(
            decode("Float", json!("3.2E9")).unwrap(),
            Value::Float(3.2e9)
        );
        assert!(matches!(
            decode("Float", json!("NaN")).unwrap(),
            Value::Float(f) if f.is_nan()
        ));
        assert_eq!(
            decode("Float", json!("Infinity")).unwrap(),
            Value::Float(f64::INFINITY)
        );
        assert_eq!(
            decode("Float", json!("-Infinity")).unwrap(),
            Value::Float(f64::NEG_INFINITY)
        );
    }

    /// The EWKT grammar of ADR-0181 item 5: an SRID-prefixed 2D point, and
    /// a `POINT Z` point whose `z` is `Some`.
    #[test]
    fn point_payloads_follow_the_ewkt_grammar() {
        assert_eq!(
            decode("Point", json!("SRID=4326;POINT (100.0 60.0)")).unwrap(),
            Value::Point(PointValue {
                srid: 4326,
                x: 100.0,
                y: 60.0,
                z: None,
            })
        );
        assert_eq!(
            decode("Point", json!("SRID=9157;POINT Z (1.0 2.0 3.0)")).unwrap(),
            Value::Point(PointValue {
                srid: 9157,
                x: 1.0,
                y: 2.0,
                z: Some(3.0),
            })
        );
    }

    /// A geographic longitude is wrapped into the stored domain, so a
    /// decoded point cannot sit outside the [-180, 180] band the POINT
    /// index's envelope clip assumes (LABS-1236); a Cartesian `x` is a raw
    /// coordinate and is never wrapped.
    #[test]
    fn geographic_longitudes_wrap_into_the_stored_domain() {
        for (spelling, x) in [
            ("SRID=4326;POINT (370.0 60.0)", 10.0),
            ("SRID=4326;POINT (-200.0 60.0)", 160.0),
            ("SRID=4979;POINT Z (180.5 60.0 3.0)", -179.5),
            // Endpoints are kept, and -0.0 normalizes to +0.0.
            ("SRID=4326;POINT (180.0 60.0)", 180.0),
            ("SRID=4326;POINT (-360.0 60.0)", 0.0),
        ] {
            match decode("Point", json!(spelling)).unwrap() {
                Value::Point(p) => assert_eq!(p.x, x, "{spelling}"),
                other => panic!("{spelling}: expected a point, got {other:?}"),
            }
        }
        // A Cartesian point's x is its own coordinate — no domain wrap.
        assert_eq!(
            decode("Point", json!("SRID=7203;POINT (370.0 60.0)")).unwrap(),
            Value::Point(PointValue {
                srid: 7203,
                x: 370.0,
                y: 60.0,
                z: None,
            })
        );
    }

    /// Each temporal tag decodes to the same `Value::Temporal` the matching
    /// Cypher constructor returns for the same `_value` through a
    /// `relate::Database` (ADR-0181 item 3).
    #[test]
    fn temporal_tags_match_their_cypher_constructors() {
        let cases = [
            ("Date", "date", "2015-03-26", "date"),
            ("Time", "time", "12:50:35.556+01:00", "time"),
            ("LocalTime", "localtime", "12:50:35.556", "localtime"),
            (
                "LocalDateTime",
                "localdatetime",
                "2015-07-04T19:32:24",
                "localdatetime",
            ),
            (
                "OffsetDateTime",
                "datetime",
                "2015-11-21T21:40:32.142-01:00",
                "datetime",
            ),
            (
                "ZonedDateTime",
                "datetime",
                "2015-11-21T21:40:32.142Z[Antarctica/Troll]",
                "datetime",
            ),
            ("Duration", "duration", "P14DT16H12M", "duration"),
        ];
        let db = Database::open(":memory:").expect("in-memory database");
        let mut conn = db.connect().expect("connection");
        for (tag, constructor, iso, kind) in cases {
            let decoded = decode(tag, json!(iso)).unwrap_or_else(|e| panic!("{tag}: {e}"));
            let Value::Temporal(temporal) = &decoded else {
                panic!("{tag}: expected a temporal value, got {decoded:?}");
            };
            assert_eq!(temporal.kind_name(), kind, "{tag}");
            let mut params = BTreeMap::new();
            params.insert("v".to_string(), Value::String(iso.into()));
            let result = conn
                .query_with_params(&format!("RETURN {constructor}($v) AS v"), params)
                .unwrap_or_else(|e| panic!("{constructor}($v): {e}"));
            assert_eq!(decoded, result.rows[0][0], "{tag}");
        }
    }

    /// `Null` and `Map` are armed defensively (ADR-0181 item 8): the wire
    /// cannot carry them as properties, but the tag table is total.
    #[test]
    fn null_and_map_are_decoded_defensively() {
        assert_eq!(decode("Null", json!(null)).unwrap(), Value::Null);
        assert_eq!(
            decode("Map", json!({"a": {"$type": "Integer", "_value": "1"}})).unwrap(),
            Value::Map(BTreeMap::from([("a".to_string(), Value::Int(1))]).into())
        );
    }

    /// The tags the engine cannot represent, and an unknown tag, error
    /// naming the offending `$type` (ADR-0181 item 7).
    #[test]
    fn unsupported_tags_error_with_the_tag() {
        for tag in [
            "Base64",
            "Unsupported",
            "Bogus",
            "Node",
            "Relationship",
            "Path",
        ] {
            let err = decode(tag, json!("payload")).unwrap_err();
            assert!(
                matches!(&err, DecodeError::Unsupported { tag: t } if t == tag),
                "{tag}: {err:?}"
            );
            assert!(err.to_string().contains(tag), "{tag}: {err}");
        }
    }

    /// A known tag with a missing or mis-shaped payload errors naming the
    /// tag (ADR-0181 items 3-6).
    #[test]
    fn malformed_payloads_error_with_the_tag() {
        for (tag, payload) in [
            ("Integer", json!("abc")),
            ("Integer", json!(123)),
            ("Float", json!("abc")),
            ("Float", json!(3.2)),
            ("Boolean", json!("true")),
            ("String", json!(7)),
            ("Date", json!("not a date")),
            ("UUID", json!("not-a-uuid")),
            ("List", json!({"not": "an array"})),
            ("Map", json!(["not", "an object"])),
            (
                "Vector",
                json!({"coordinatesType": "FLOAT64", "coordinates": [1.0]}),
            ),
            (
                "Vector",
                json!({"coordinatesType": "NOPE", "coordinates": ["1.0"]}),
            ),
            ("Vector", json!({"coordinatesType": "FLOAT64"})),
            ("Integer", json!(null)),
        ] {
            let err = decode(tag, payload.clone()).unwrap_err();
            assert!(
                matches!(&err, DecodeError::Malformed { tag: t, .. } if t == tag),
                "{tag} {payload}: {err:?}"
            );
            assert!(err.to_string().contains(tag), "{tag} {payload}: {err}");
        }
        // A missing `_value` is malformed even for a tag with no payload
        // shape of its own.
        let err = decode_value(&json!({"$type": "Date"})).unwrap_err();
        assert!(matches!(&err, DecodeError::Malformed { tag, .. } if tag == "Date"));
    }

    /// The point grammar rejects every other shape, including a missing
    /// `SRID=` prefix (ADR-0181 item 5).
    #[test]
    fn point_payloads_outside_the_grammar_error() {
        for payload in [
            "POINT (1 2)",
            "SRID=4326;POINTZ (1 2)",
            "SRID=4326;POINT Z (1 2)",
            "SRID=4326;POINT (1 2 3)",
            "SRID=abc;POINT (1 2)",
            "SRID=4326;POINT (1)",
            "SRID=4326;POINT (1 2",
        ] {
            let err = decode("Point", json!(payload)).unwrap_err();
            assert!(
                matches!(&err, DecodeError::Malformed { tag, .. } if tag == "Point"),
                "{payload}: {err:?}"
            );
            assert!(err.to_string().contains("Point"), "{payload}: {err}");
        }
    }

    /// A point coordinate must be finite (the rule `point()` enforces):
    /// the `Float` tag's `NaN`/`Infinity` spellings parse as `f64` but are
    /// refused here, so no decoded point can sit outside the domain the
    /// POINT index's envelope clip assumes (LABS-1236).
    #[test]
    fn non_finite_point_coordinates_error() {
        for payload in [
            "SRID=4326;POINT (NaN 60)",
            "SRID=4326;POINT (inf 60)",
            "SRID=4326;POINT (60 -Infinity)",
            "SRID=4326;POINT Z (1 2 inf)",
        ] {
            let err = decode("Point", json!(payload)).unwrap_err();
            assert!(
                matches!(&err, DecodeError::Malformed { tag, .. } if tag == "Point"),
                "{payload}: {err:?}"
            );
            assert!(
                err.to_string().contains("finite coordinate"),
                "{payload}: {err}"
            );
        }
    }

    /// The decoder upholds the stored-point domain the `point()` constructor
    /// enforces: a geographic latitude outside `[-90, 90]` is refused, so a
    /// wire value cannot enter the store as a point the constructor would
    /// reject.
    #[test]
    fn an_out_of_range_latitude_errors() {
        let err = decode("Point", json!("SRID=4326;POINT (10 91)")).unwrap_err();
        assert!(err.to_string().contains("latitude"), "{err}");
        // A cartesian point has no latitude rule.
        assert!(decode("Point", json!("SRID=7203;POINT (10 91)")).is_ok());
    }

    /// The decoder upholds the SRID's fixed dimension, the same rule
    /// `point()`'s SRID table enforces; an unknown SRID stays open (the load
    /// surface's raw `SRID=`).
    #[test]
    fn a_coordinate_count_that_disagrees_with_the_srid_errors() {
        for payload in [
            "SRID=7203;POINT Z (1 2 3)",
            "SRID=9157;POINT (1 2)",
            "SRID=4979;POINT (1 2)",
        ] {
            let err = decode("Point", json!(payload)).unwrap_err();
            assert!(
                err.to_string().contains("coordinate dimension"),
                "{payload}: {err}"
            );
        }
        assert!(decode("Point", json!("SRID=12345;POINT Z (1 2 3)")).is_ok());
    }

    /// A value without a `$type` tag (a server that ignored the typed
    /// `Accept`) errors as untagged (ADR-0181 item 7).
    #[test]
    fn untagged_values_error() {
        for value in [
            json!("2015-03-26"),
            json!(42),
            json!(null),
            json!({"elementId": "4:x:0"}),
            json!([1, 2]),
        ] {
            let err = decode_value(&value).unwrap_err();
            assert!(matches!(err, DecodeError::Untagged), "{value}: {err:?}");
            assert!(err.to_string().contains("untagged"), "{value}: {err}");
        }
    }

    /// A failing list element propagates the inner tag; the caller
    /// attaches the owning property key (ADR-0181 item 7).
    #[test]
    fn list_errors_name_the_failing_element_tag() {
        let err = decode(
            "List",
            json!([
                {"$type": "Integer", "_value": "1"},
                {"$type": "Integer", "_value": "abc"}
            ]),
        )
        .unwrap_err();
        assert!(
            matches!(&err, DecodeError::Malformed { tag, .. } if tag == "Integer"),
            "{err:?}"
        );
        assert!(err.to_string().contains("Integer"), "{err}");
    }

    /// The captured unrepresentable properties error instead of degrading
    /// to a string: `mappable_v1_1`'s UUID arrives as the server's
    /// `Unsupported` tag, and `unmappable`'s `blob` as `Base64`
    /// (ADR-0181 item 7).
    #[test]
    fn captured_unrepresentable_properties_error() {
        let fixture = fixture();
        let properties = &fixture["servers"]["neo4j:2026.09-enterprise"]["mappable_v1_1"]["data"]
            ["values"][0][0]["_value"]["_properties"];
        let err = decode_value(&properties["uuid"]).unwrap_err();
        assert!(
            matches!(&err, DecodeError::Unsupported { tag } if tag == "Unsupported"),
            "{err:?}"
        );

        for image in IMAGES {
            let properties = &fixture["servers"][image]["unmappable"]["data"]["values"][0][0]
                ["_value"]["_properties"];
            let err = decode_value(&properties["blob"]).unwrap_err();
            assert!(
                matches!(&err, DecodeError::Unsupported { tag } if tag == "Base64"),
                "{image}: {err:?}"
            );
            assert_eq!(
                decode_value(&properties["name"]).unwrap(),
                Value::String("unmappable".into())
            );
        }
    }
}
