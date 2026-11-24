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
mod tests;
