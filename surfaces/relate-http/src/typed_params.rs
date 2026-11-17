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
//! in Cypher, e.g. `date($p)`), a Point to its `{coordinates, crs}`
//! map, a Vector to a list of numbers, and graph entities
//! (`Node`/`Relationship`/`Path`) are rejected — they cannot be
//! parameters.

use serde_json::{Map, Value as Json};

use std::collections::BTreeMap;

/// A statement request's parameter dialect, resolved from its
/// Content-Type (ADR-0425): the Query API family, including the unversioned
/// v1.0 alias, carries typed envelopes; `application/json` carries plain values.
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
    // One chained conversion: typed→plain per entry, then plain→engine.
    map.iter()
        .map(|(k, v)| typed_param_to_plain(v).map(|v| (k.clone(), v)))
        .collect::<Result<Map<String, Json>, String>>()
        .and_then(|map| relate::params_from_map(&map))
}

/// The typed forms this decoder accepts (the set the response encoder
/// emits, ADR-0236).
const KNOWN_TYPES: &[&str] = &[
    "Null",
    "Boolean",
    "Integer",
    "Float",
    "String",
    "Base64",
    "UUID",
    "List",
    "Map",
    "Date",
    "Time",
    "LocalTime",
    "OffsetDateTime",
    "ZonedDateTime",
    "LocalDateTime",
    "Duration",
    "Point",
    "Vector",
    "Unsupported",
];

/// Decode one parameter value; plain JSON passes through recursively.
pub fn typed_param_to_plain(value: &Json) -> Result<Json, String> {
    match value {
        Json::Array(items) => items
            .iter()
            .map(typed_param_to_plain)
            .collect::<Result<Vec<_>, _>>()
            .map(Json::Array),
        Json::Object(map) => {
            let is_typed =
                map.len() == 2 && map.contains_key("$type") && map.contains_key("_value");
            if !is_typed {
                return map
                    .iter()
                    .map(|(k, v)| Ok((k.clone(), typed_param_to_plain(v)?)))
                    .collect::<Result<Map<String, Json>, String>>()
                    .map(Json::Object);
            }
            let tag = map["$type"].as_str().unwrap_or_default();
            let inner = &map["_value"];
            match tag {
                "Null" => Ok(Json::Null),
                "Boolean" | "String" | "Base64" | "UUID" => Ok(inner.clone()),
                "Integer" => int_value(inner),
                "Float" => float_value(inner),
                "List" => {
                    let items = inner
                        .as_array()
                        .ok_or_else(|| format!("typed List {tag}: _value must be an array"))?;
                    items
                        .iter()
                        .map(typed_param_to_plain)
                        .collect::<Result<Vec<_>, _>>()
                        .map(Json::Array)
                }
                "Map" => {
                    let obj = inner
                        .as_object()
                        .ok_or_else(|| "typed Map: _value must be an object".to_string())?;
                    obj.iter()
                        .map(|(k, v)| Ok((k.clone(), typed_param_to_plain(v)?)))
                        .collect::<Result<Map<String, Json>, String>>()
                        .map(Json::Object)
                }
                // Temporal payloads are ISO-8601 strings; typed values are
                // built in Cypher (date($p), point($p), …).
                "Date" | "Time" | "LocalTime" | "OffsetDateTime" | "ZonedDateTime"
                | "LocalDateTime" | "Duration" => Ok(inner.clone()),
                // A point decodes to its {coordinates, crs} map (build a real
                // point in Cypher with point($p)).
                "Point" => typed_param_to_plain(&Json::Object(
                    inner
                        .as_object()
                        .ok_or_else(|| "typed Point: _value must be an object".to_string())?
                        .clone(),
                )),
                // A vector decodes to the plain list of numbers its
                // string-typed coordinates spell.
                "Vector" => {
                    let coordinates = inner
                        .get("coordinates")
                        .and_then(Json::as_array)
                        .ok_or_else(|| {
                            "typed Vector: _value.coordinates must be an array".to_string()
                        })?;
                    coordinates
                        .iter()
                        .map(|c| match c {
                            Json::String(s) => s
                                .parse::<f64>()
                                .map(float_json)
                                .map_err(|_| format!("typed Vector: bad coordinate {s:?}")),
                            other => Ok(other.clone()),
                        })
                        .collect::<Result<Vec<_>, _>>()
                        .map(Json::Array)
                }
                "Node" | "Relationship" | "Path" => Err(format!(
                    "typed {tag}: graph entities cannot be query parameters"
                )),
                other if KNOWN_TYPES.contains(&other) => {
                    Err(format!("typed {other}: not accepted as a parameter"))
                }
                other => Err(format!("unknown typed parameter $type {other:?}")),
            }
        }
        _ => Ok(value.clone()),
    }
}

/// An Integer payload (the typed form strings it) as a JSON number.
fn int_value(value: &Json) -> Result<Json, String> {
    match value {
        Json::String(s) => s
            .parse::<i64>()
            .map(|i| Json::Number(i.into()))
            .map_err(|_| format!("typed Integer: bad integer {s:?}")),
        Json::Number(n) => Ok(Json::Number(n.clone())),
        other => Err(format!("typed Integer: bad payload {other}")),
    }
}

/// A Float payload (string or number) as a JSON number. The wire's
/// non-finite spellings (`NaN`/`Inf`/`-Inf`) parse back via `f64`.
fn float_value(value: &Json) -> Result<Json, String> {
    match value {
        Json::String(s) => s
            .parse::<f64>()
            .map(float_json)
            .map_err(|_| format!("typed Float: bad float {s:?}")),
        Json::Number(n) => Ok(Json::Number(n.clone())),
        other => Err(format!("typed Float: bad payload {other}")),
    }
}

/// A float as a JSON number, or its string spelling when non-finite (the
/// wire codec's convention).
fn float_json(f: f64) -> Json {
    relate_model::wire::json_f64(f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn decodes_the_typed_scalars() {
        assert_eq!(
            typed_param_to_plain(&json!({"$type": "Integer", "_value": "10"})).unwrap(),
            json!(10)
        );
        assert_eq!(
            typed_param_to_plain(&json!({"$type": "Float", "_value": "1.5"})).unwrap(),
            json!(1.5)
        );
        assert_eq!(
            typed_param_to_plain(&json!({"$type": "Boolean", "_value": true})).unwrap(),
            json!(true)
        );
        assert_eq!(
            typed_param_to_plain(&json!({"$type": "String", "_value": "x"})).unwrap(),
            json!("x")
        );
        assert_eq!(
            typed_param_to_plain(&json!({"$type": "Null", "_value": null})).unwrap(),
            Json::Null
        );
        // Temporal payloads stay their ISO strings.
        assert_eq!(
            typed_param_to_plain(&json!({"$type": "Date", "_value": "2024-01-01"})).unwrap(),
            json!("2024-01-01")
        );
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
        assert_eq!(
            typed_param_to_plain(&typed).unwrap(),
            json!([1, {"n": 0.5}])
        );
    }

    #[test]
    fn a_map_with_extra_keys_stays_a_map() {
        // The ambiguity rule: only exactly {$type, _value} is a typed value.
        let user_map = json!({"$type": "Integer", "_value": "10", "other": 1});
        assert_eq!(
            typed_param_to_plain(&user_map).unwrap(),
            json!({"$type": "Integer", "_value": "10", "other": 1})
        );
    }

    #[test]
    fn unknown_or_unacceptable_types_are_errors() {
        assert!(typed_param_to_plain(&json!({"$type": "Wat", "_value": 1}))
            .unwrap_err()
            .contains("unknown typed parameter"));
        assert!(
            typed_param_to_plain(&json!({"$type": "Node", "_value": {}}))
                .unwrap_err()
                .contains("cannot be query parameters")
        );
        assert!(typed_param_to_plain(&json!({"$type": "Integer", "_value": "nope"})).is_err());
    }

    #[test]
    fn a_vector_decodes_to_its_number_list() {
        let typed = json!({"$type": "Vector", "_value": {"coordinatesType": "INT8", "coordinates": ["1", "2"]}});
        assert_eq!(typed_param_to_plain(&typed).unwrap(), json!([1.0, 2.0]));
    }
}
