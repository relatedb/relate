//! The Query API v2 **request** parameter folds, one per request
//! dialect (ADR-0425): a typed request (`application/vnd.neo4j.query`
//! or its versioned family) carries
//! `{"$type": <Cypher type>, "_value": …}` envelopes — the encoding
//! official clients send — while a plain `application/json` request
//! carries plain JSON values, where an envelope-shaped object is a user
//! **map** (byte-for-byte what a real server delivers to Cypher; our
//! earlier unconditional envelope decode was the divergence trap this
//! ADR closes).
//!
//! Ambiguity rule (the typed dialect): an object is a typed value only
//! when its keys are exactly `$type` and `_value`; a user map that
//! merely contains a `$type` key among others stays a map. Temporal
//! payloads decode to their ISO-8601 strings (typed values are built
//! in Cypher, e.g. `date($p)`), a Point to an engine point (the EWKT
//! string Neo4j sends), a Vector to a list of numbers, a non-finite
//! Float to that float (JSON has no such number, and a plain fold would
//! bind a STRING), and graph entities (`Node`/`Relationship`/`Path`) are
//! rejected — they cannot be parameters.

use serde_json::{Map, Value as Json};

use std::collections::BTreeMap;

/// A statement request's parameter dialect, resolved from its
/// Content-Type (ADR-0425): the Query API family, including the unversioned
/// v1.0 alias, carries typed envelopes; `application/json` carries plain values.
#[derive(Debug, Clone, Copy)]
pub(crate) enum RequestDialect {
    /// Query API envelope parameters decode via [`typed_request_params`].
    Typed,
    /// `Content-Type: application/json`: plain values through the
    /// engine's [`relate::params_from_map`] — an envelope-shaped
    /// object stays a map.
    Plain,
}

/// The accepted-request-types guidance, stated once — both fail-closed
/// messages carry it (single-line, no reflow artifacts).
const ACCEPTED_REQUEST_TYPES: &str = "application/json for plain parameters, or application/vnd.neo4j.query (v1.0 alias) and its versioned family, including application/vnd.neo4j.query.v1.2, for typed envelope parameters";

/// Resolve the dialect from a request's Content-Type: plain JSON (with
/// any `; charset=…` parameter ignored) is [`RequestDialect::Plain`], a
/// member of the `application/vnd.neo4j.query` family, including its bare
/// v1.0 alias, is [`RequestDialect::Typed`]. Missing or unsupported types
/// fail closed with the accepted spellings.
pub(crate) fn request_dialect(content_type: Option<&str>) -> Result<RequestDialect, String> {
    let Some(value) = content_type else {
        return Err(format!(
            "content-type is required: {ACCEPTED_REQUEST_TYPES}"
        ));
    };
    let media_type = crate::http::lowercased_media_type(value);
    if media_type == crate::http::APPLICATION_JSON {
        return Ok(RequestDialect::Plain);
    }
    if crate::http::is_query_family(&media_type) {
        return Ok(RequestDialect::Typed);
    }
    Err(format!(
        "content-type {media_type:?} is not a statement request type: {ACCEPTED_REQUEST_TYPES}"
    ))
}

/// The one dialect dispatcher (ADR-0425): the request's decoded dialect
/// picks the parameter fold — typed envelopes under the versioned content
/// type, plain JSON under `application/json`. Beside the enum it matches,
/// so the request-decode policy has one home and no response module
/// carries it.
pub(crate) fn request_params(
    dialect: RequestDialect,
    map: Option<&Map<String, Json>>,
) -> Result<BTreeMap<String, relate::Value>, String> {
    // The absent-parameters policy, folded once for both dialects: no
    // map means no params (the folds below take a map).
    let Some(map) = map else {
        return Ok(BTreeMap::new());
    };
    match dialect {
        RequestDialect::Typed => typed_request_params(map),
        RequestDialect::Plain => relate::params_from_map(map),
    }
}

/// The **typed** request fold (ADR-0425): a `parameters` object of
/// `{"$type","_value"}` envelope values (the v1.2 request dialect —
/// containers included, decoded recursively) becomes engine params; the
/// 400 mapping is the caller's, the absent-parameters policy the
/// dispatcher's.
fn typed_request_params(
    map: &Map<String, Json>,
) -> Result<BTreeMap<String, relate::Value>, String> {
    // The prefix, ADR-0462's depth check and the collect are the plain
    // fold's; this dialect supplies only its own value decoder.
    relate::params_from_map_with(map, typed_param_value)
}

/// One typed parameter as an engine value — the module's one decode walk.
///
/// It decodes to `relate::Value` directly rather than to plain JSON first:
/// two shapes have no plain-JSON form (a non-finite Float, and a Point,
/// whose object form would fold to a user map), and the direct walk keeps
/// the depth check with the value it validates. Containers recurse; a leaf
/// is either a tag this match names or a plain JSON value the one plain
/// fold ([`relate::plain_param_value`]) decodes.
fn typed_param_value(value: &Json) -> Result<relate::Value, String> {
    match value {
        Json::Array(items) => list_value(items),
        Json::Object(map) => match typed_envelope(map) {
            Some((tag, payload)) => typed_tag_value(tag, payload),
            // An ordinary user map (ADR-0425's ambiguity rule): recurse, so
            // a typed envelope nested inside it gets its own decode.
            None => map_value(map),
        },
        _ => relate::plain_param_value(value),
    }
}

/// The tag and payload of a typed envelope — an object whose keys are
/// exactly `$type` and `_value` (ADR-0425's ambiguity rule); any other
/// object is a user map. A non-string `$type` reads as the empty tag, so it
/// fails as an unknown tag instead of becoming a map.
fn typed_envelope(map: &Map<String, Json>) -> Option<(&str, &Json)> {
    (map.len() == 2 && map.contains_key("$type") && map.contains_key("_value"))
        .then(|| (map["$type"].as_str().unwrap_or_default(), &map["_value"]))
}

/// One `$type`-tagged value as its engine value (the ADR-0236 tag table,
/// the same vocabulary the response encoder emits). Each tag checks its
/// payload's shape, so a malformed typed parameter is a 400 rather than a
/// value of whatever type the payload happened to have.
fn typed_tag_value(tag: &str, payload: &Json) -> Result<relate::Value, String> {
    let string_payload = |what: &str| {
        payload
            .as_str()
            .map(|s| relate::Value::String(s.into()))
            .ok_or_else(|| format!("typed {what}: _value must be a JSON string"))
    };
    match tag {
        "Null" => payload
            .is_null()
            .then_some(relate::Value::Null)
            .ok_or_else(|| "typed Null: _value must be null".to_string()),
        "Boolean" => payload
            .as_bool()
            .map(relate::Value::Bool)
            .ok_or_else(|| "typed Boolean: _value must be a JSON boolean".to_string()),
        "String" | "UUID" => string_payload(tag),
        "Integer" => integer_value(payload),
        "Float" => float_value(payload),
        "List" => payload
            .as_array()
            .map_or_else(|| Err("typed List: _value must be an array".to_string()), |items| {
                list_value(items)
            }),
        "Map" => payload
            .as_object()
            .map_or_else(|| Err("typed Map: _value must be an object".to_string()), map_value),
        // Temporal payloads are ISO-8601 strings; typed values are built in
        // Cypher (date($p), …).
        "Date" | "Time" | "LocalTime" | "OffsetDateTime" | "ZonedDateTime" | "LocalDateTime"
        | "Duration" => string_payload(tag),
        // The EWKT string Neo4j sends, or the legacy `{coordinates, crs}`
        // object (which folds to a map through this same walk, so a nested
        // non-finite Float is a real float). Any other payload is refused.
        "Point" => match payload {
            Json::String(wkt) => relate_model::point::parse_ewkt(wkt)
                .map(relate::Value::Point)
                .map_err(|detail| format!("typed Point: {detail}")),
            Json::Object(map) => map_value(map),
            other => Err(format!(
                "typed Point: _value must be an EWKT string or a {{coordinates, crs}} object, got {other}"
            )),
        },
        "Vector" => vector_value(payload),
        "Node" | "Relationship" | "Path" => Err(format!(
            "typed {tag}: graph entities cannot be query parameters"
        )),
        // No engine value to bind: the response encoder emits neither, and
        // a payload folded to whatever JSON type it had would be silently
        // wrong.
        "Base64" | "Unsupported" => Err(format!("typed {tag}: not accepted as a parameter")),
        other => Err(format!("unknown typed parameter $type {other:?}")),
    }
}

fn list_value(items: &[Json]) -> Result<relate::Value, String> {
    Ok(relate::Value::List(std::sync::Arc::new(
        items
            .iter()
            .map(typed_param_value)
            .collect::<Result<Vec<_>, _>>()?,
    )))
}

fn map_value(map: &Map<String, Json>) -> Result<relate::Value, String> {
    Ok(relate::Value::Map(std::sync::Arc::new(
        map.iter()
            .map(|(k, v)| Ok((k.clone(), typed_param_value(v)?)))
            .collect::<Result<BTreeMap<_, _>, String>>()?,
    )))
}

/// An `Integer` payload (the wire strings it).
fn integer_value(payload: &Json) -> Result<relate::Value, String> {
    match payload {
        Json::String(text) => text
            .parse::<i64>()
            .map(relate::Value::Int)
            .map_err(|_| format!("typed Integer: bad integer {text:?}")),
        Json::Number(number) => number
            .as_i64()
            .map(relate::Value::Int)
            .ok_or_else(|| format!("typed Integer: bad payload {payload}")),
        other => Err(format!("typed Integer: bad payload {other}")),
    }
}

/// A `Float` payload (a decimal string on the wire, or a JSON number) as
/// the engine float — the non-finite spellings (`Infinity`, `-Infinity`,
/// `NaN`) parse like any other (`REL8-2033`).
fn float_value(payload: &Json) -> Result<relate::Value, String> {
    float_payload(payload)
        .map(relate::Value::Float)
        .ok_or_else(|| format!("typed Float: bad float {payload}"))
}

/// One float payload reader — the wire's string form or a plain JSON number
/// (the `Integer` twin is [`integer_value`]).
fn float_payload(payload: &Json) -> Option<f64> {
    match payload {
        Json::String(text) => text.parse::<f64>().ok(),
        Json::Number(number) => number.as_f64(),
        _ => None,
    }
}

/// A `Vector` as the engine list of numbers its coordinate payload spells.
fn vector_value(payload: &Json) -> Result<relate::Value, String> {
    let coordinates = payload
        .get("coordinates")
        .and_then(Json::as_array)
        .ok_or_else(|| "typed Vector: _value.coordinates must be an array".to_string())?;
    let values = coordinates
        .iter()
        .map(|coordinate| {
            float_payload(coordinate)
                .map(relate::Value::Float)
                .ok_or_else(|| format!("typed Vector: bad coordinate {coordinate}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(relate::Value::List(std::sync::Arc::new(values)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use relate::Value;
    use serde_json::json;

    /// The items of an engine list value.
    fn items(value: relate::Value) -> Vec<relate::Value> {
        let Value::List(items) = value else {
            panic!("expected a list, got {value:?}");
        };
        items.as_ref().clone()
    }

    #[test]
    fn decodes_the_typed_scalars() {
        assert_eq!(
            typed_param_value(&json!({"$type": "Integer", "_value": "10"})).unwrap(),
            Value::Int(10)
        );
        assert_eq!(
            typed_param_value(&json!({"$type": "Float", "_value": "1.5"})).unwrap(),
            Value::Float(1.5)
        );
        assert_eq!(
            typed_param_value(&json!({"$type": "Boolean", "_value": true})).unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            typed_param_value(&json!({"$type": "String", "_value": "x"})).unwrap(),
            Value::String("x".into())
        );
        assert_eq!(
            typed_param_value(&json!({"$type": "Null", "_value": null})).unwrap(),
            Value::Null
        );
        // Temporal payloads stay their ISO strings.
        assert_eq!(
            typed_param_value(&json!({"$type": "Date", "_value": "2024-01-01"})).unwrap(),
            Value::String("2024-01-01".into())
        );
    }

    /// A non-finite Float is a real float, not the STRING the plain JSON
    /// fold would have to bind (REL8-2033).
    #[test]
    fn non_finite_floats_bind_their_float() {
        for (spelling, expected) in [
            ("Infinity", f64::INFINITY),
            ("-Infinity", f64::NEG_INFINITY),
            ("NaN", f64::NAN),
            ("Inf", f64::INFINITY),
        ] {
            let decoded =
                typed_param_value(&json!({"$type": "Float", "_value": spelling})).unwrap();
            let Value::Float(f) = decoded else {
                panic!("{spelling}: expected a float, got {decoded:?}");
            };
            assert!(
                f == expected || (f.is_nan() && expected.is_nan()),
                "{spelling}: got {f}"
            );
        }
    }

    /// A Point's EWKT payload is an engine point; the legacy
    /// `{coordinates, crs}` object keeps folding to a map, through the same
    /// walk (so a nested non-finite float is still a float).
    #[test]
    fn points_decode_to_the_engine_point() {
        let point =
            typed_param_value(&json!({"$type": "Point", "_value": "SRID=7203;POINT (1.0 2.0)"}))
                .unwrap();
        assert!(
            matches!(point, Value::Point(ref p) if p.srid == 7203 && p.x == 1.0 && p.y == 2.0),
            "{point:?}"
        );
        let legacy = typed_param_value(&json!({
            "$type": "Point",
            "_value": {"coordinates": [1.0, 2.0], "crs": {"srid": 7203}},
        }))
        .unwrap();
        assert!(matches!(legacy, Value::Map(_)), "{legacy:?}");
    }

    #[test]
    fn decodes_nested_lists_and_maps() {
        let typed = json!({
            "$type": "List",
            "_value": [
                {"$type": "Integer", "_value": "1"},
                {"$type": "Map", "_value": {"n": {"$type": "Float", "_value": "0.5"}}},
            ],
        });
        let decoded = typed_param_value(&typed).unwrap();
        assert_eq!(
            decoded,
            Value::List(std::sync::Arc::new(vec![
                Value::Int(1),
                Value::Map(std::sync::Arc::new(std::collections::BTreeMap::from([(
                    "n".to_string(),
                    Value::Float(0.5)
                )])))
            ]))
        );
    }

    #[test]
    fn a_map_with_extra_keys_stays_a_map() {
        // The ambiguity rule: only exactly {$type, _value} is a typed value.
        let user_map = json!({"$type": "Integer", "_value": "10", "other": 1});
        let decoded = typed_param_value(&user_map).unwrap();
        let Value::Map(entries) = &decoded else {
            panic!("expected a map, got {decoded:?}");
        };
        assert_eq!(entries.len(), 3);
        assert_eq!(entries["$type"], Value::String("Integer".into()));
    }

    /// Every tag checks its payload's shape: a well-formed envelope with the
    /// wrong payload type is a 400, never a value of whatever type the
    /// payload happened to carry.
    #[test]
    fn malformed_payloads_are_errors() {
        for typed in [
            json!({"$type": "Boolean", "_value": "true"}),
            json!({"$type": "String", "_value": 1}),
            json!({"$type": "Point", "_value": 5}),
            json!({"$type": "Point", "_value": true}),
            json!({"$type": "Vector", "_value": {"coordinates": [true]}}),
            json!({"$type": "Vector", "_value": {"coordinates": [null]}}),
            json!({"$type": "Null", "_value": "x"}),
            json!({"$type": "Base64", "_value": "AAAA"}),
            json!({"$type": "Unsupported", "_value": "x"}),
        ] {
            assert!(
                typed_param_value(&typed).is_err(),
                "{typed} decoded instead of erroring"
            );
        }
    }

    #[test]
    fn unknown_or_unacceptable_types_are_errors() {
        assert!(typed_param_value(&json!({"$type": "Wat", "_value": 1}))
            .unwrap_err()
            .contains("unknown typed parameter"));
        assert!(typed_param_value(&json!({"$type": "Node", "_value": {}}))
            .unwrap_err()
            .contains("cannot be query parameters"));
        assert!(typed_param_value(&json!({"$type": "Integer", "_value": "nope"})).is_err());
    }

    #[test]
    fn a_vector_decodes_to_its_number_list() {
        let typed = json!({"$type": "Vector", "_value": {"coordinatesType": "INT8", "coordinates": ["1", "2"]}});
        assert_eq!(
            items(typed_param_value(&typed).unwrap()),
            vec![Value::Float(1.0), Value::Float(2.0)]
        );
    }
}
