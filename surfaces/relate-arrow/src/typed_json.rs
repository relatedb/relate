//! Arrow-owned typed-json-v1 codec (ADR-0088).
//!
//! Encoding validates each member as it is emitted: no separate recursive
//! validation pass may run ahead of the encoder and disagree with its output.

use arrow_schema::ArrowError;
use indexmap::IndexMap;
use relate_model::serde_value::{cypher_value_to_json, try_json_to_cypher_value_v2, EncodeError};
use relate_model::CypherValue;
use serde_json::{json, Map, Number, Value};

fn invalid(message: impl Into<String>) -> ArrowError {
    ArrowError::InvalidArgumentError(message.into())
}

fn finite_number(f: f64, path: &str) -> Result<Value, ArrowError> {
    Number::from_f64(f).map(Value::Number).ok_or_else(|| {
        invalid(format!(
            "{path}: non-finite float cannot be represented in typed JSON"
        ))
    })
}

fn model_error_at(path: &str, error: EncodeError) -> ArrowError {
    // The model starts each independently encoded payload at `$`. Its suffix
    // belongs below the enclosing Arrow typed member, not at the cell root.
    match error.path.strip_prefix('$') {
        Some(suffix) => invalid(format!("{path}{suffix}: {}", error.message)),
        None => invalid(format!("{path}: {error}")),
    }
}

/// Encode a non-null composite or mixed cell as an exact typed-json-v1 envelope.
pub(crate) fn encode(value: &CypherValue) -> Result<String, ArrowError> {
    Ok(encode_value(value, "$")?.to_string())
}

/// Encode the properties child of a marked Node/Relationship Struct using the
/// same typed members as a MAP payload, without a top-level MAP envelope.
pub(crate) fn encode_props(props: &IndexMap<String, CypherValue>) -> Result<String, ArrowError> {
    Ok(props_value(props, "$.properties")?.to_string())
}

fn props_value(props: &IndexMap<String, CypherValue>, path: &str) -> Result<Value, ArrowError> {
    props
        .iter()
        .map(|(key, value)| Ok((key.clone(), encode_value(value, &format!("{path}.{key}"))?)))
        .collect::<Result<Map<_, _>, _>>()
        .map(Value::Object)
}

fn encode_value(value: &CypherValue, path: &str) -> Result<Value, ArrowError> {
    let payload = match value {
        CypherValue::Null => Value::Null,
        CypherValue::Bool(b) => Value::Bool(*b),
        CypherValue::Int(i) => Value::Number((*i).into()),
        CypherValue::Float(f) => finite_number(*f, path)?,
        CypherValue::Str(s) => Value::String(s.clone()),
        CypherValue::List(items) => items
            .iter()
            .enumerate()
            .map(|(i, item)| encode_value(item, &format!("{path}[{i}]")))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array)?,
        CypherValue::Map(props) => props_value(props, path)?,
        CypherValue::Node {
            id,
            labels,
            properties,
        } => json!({
            "id": id, "labels": labels, "properties": props_value(properties, &format!("{path}.properties"))?,
        }),
        CypherValue::Relationship {
            id,
            start_id,
            end_id,
            rel_type,
            properties,
        } => json!({
            "id": id, "type": rel_type, "start_node": start_id, "end_node": end_id,
            "properties": props_value(properties, &format!("{path}.properties"))?,
        }),
        CypherValue::Path {
            nodes,
            relationships,
        } => {
            if nodes.len() != relationships.len().saturating_add(1)
                || !nodes.iter().all(|v| matches!(v, CypherValue::Node { .. }))
                || !relationships
                    .iter()
                    .all(|v| matches!(v, CypherValue::Relationship { .. }))
            {
                return Err(invalid(format!(
                    "{path}: path nodes/relationships have invalid shape"
                )));
            }
            json!({
                "nodes": nodes.iter().enumerate().map(|(i, node)| encode_value(node, &format!("{path}.nodes[{i}]"))).collect::<Result<Vec<_>, _>>()?,
                "relationships": relationships.iter().enumerate().map(|(i, relationship)| encode_value(relationship, &format!("{path}.relationships[{i}]"))).collect::<Result<Vec<_>, _>>()?,
            })
        }
        // The `FixedSizeList` i32 dimension ceiling is the Arrow column
        // path's own rule (`check_vector_child_budget`), not a shape of
        // the typed-json value — the Vector family takes the same single
        // model-delegation arm as Point/Geometry/the temporals.
        // The model owns every persistence payload shape; Arrow owns only the
        // typed envelope and graph-member recursion around these values.
        CypherValue::Point(_)
        | CypherValue::Geometry(_)
        | CypherValue::Vector { .. }
        | CypherValue::Date(_)
        | CypherValue::Time(..)
        | CypherValue::DateTime(..)
        | CypherValue::LocalDateTime(_)
        | CypherValue::Duration(_) => {
            cypher_value_to_json(value).map_err(|e| model_error_at(path, e))?
        }
    };
    Ok(json!({
        "type": if matches!(value, CypherValue::Geometry(_)) { "GEOMETRY" } else { value.type_name() },
        "value": payload,
    }))
}

/// Decode a marked typed-json-v1 cell. Unmarked strings never come here.
pub(crate) fn decode(s: &str) -> Result<CypherValue, ArrowError> {
    let json = serde_json::from_str(s)
        .map_err(|e| ArrowError::ParseError(format!("invalid typed JSON: {e}")))?;
    decode_value(&json)
}

/// Decode a marked graph Struct's properties child (an object of envelopes).
pub(crate) fn decode_props(s: &str) -> Result<IndexMap<String, CypherValue>, ArrowError> {
    let json: Value = serde_json::from_str(s)
        .map_err(|e| ArrowError::ParseError(format!("invalid properties JSON: {e}")))?;
    decode_props_value(&json)
}

fn decode_props_value(v: &Value) -> Result<IndexMap<String, CypherValue>, ArrowError> {
    let props = v
        .as_object()
        .ok_or_else(|| ArrowError::ParseError("expected properties object".into()))?;
    props
        .iter()
        .map(|(key, value)| Ok((key.clone(), decode_value(value)?)))
        .collect()
}

fn require_exact_payload_fields(
    tag: &str,
    payload: &Value,
    required: &[&str],
) -> Result<(), ArrowError> {
    let obj = payload
        .as_object()
        .ok_or_else(|| ArrowError::ParseError(format!("invalid {tag} payload")))?;
    if obj.len() != required.len() || required.iter().any(|name| !obj.contains_key(*name)) {
        return Err(ArrowError::ParseError(format!(
            "{tag} payload requires exactly {}",
            required.join(", ")
        )));
    }
    Ok(())
}

fn decode_value(v: &Value) -> Result<CypherValue, ArrowError> {
    let obj = v
        .as_object()
        .ok_or_else(|| ArrowError::ParseError("expected typed value object".into()))?;
    if obj.len() != 2 {
        return Err(ArrowError::ParseError(
            "typed value requires exactly type and value".into(),
        ));
    }
    let tag = obj
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ArrowError::ParseError("missing typed value type".into()))?;
    let payload = obj
        .get("value")
        .ok_or_else(|| ArrowError::ParseError("missing typed value payload".into()))?;
    let field = |name: &str| -> Result<&Value, ArrowError> {
        payload
            .get(name)
            .ok_or_else(|| ArrowError::ParseError(format!("missing {tag}.{name}")))
    };
    let int = |v: &Value| {
        v.as_i64()
            .ok_or_else(|| ArrowError::ParseError(format!("invalid {tag} integer")))
    };
    let value = match tag {
        "NULL" if payload.is_null() => CypherValue::Null,
        "BOOLEAN" => CypherValue::Bool(
            payload
                .as_bool()
                .ok_or_else(|| ArrowError::ParseError("invalid BOOLEAN".into()))?,
        ),
        "INTEGER" => CypherValue::Int(int(payload)?),
        "FLOAT" => CypherValue::Float(
            payload
                .as_f64()
                .filter(|v| v.is_finite())
                .ok_or_else(|| ArrowError::ParseError("invalid FLOAT".into()))?,
        ),
        "STRING" => CypherValue::Str(
            payload
                .as_str()
                .ok_or_else(|| ArrowError::ParseError("invalid STRING".into()))?
                .to_owned(),
        ),
        "LIST" => CypherValue::List(
            payload
                .as_array()
                .ok_or_else(|| ArrowError::ParseError("invalid LIST".into()))?
                .iter()
                .map(decode_value)
                .collect::<Result<_, _>>()?,
        ),
        "MAP" => CypherValue::Map(decode_props_value(payload)?),
        "NODE" => {
            require_exact_payload_fields("NODE", payload, &["id", "labels", "properties"])?;
            let labels = field("labels")?
                .as_array()
                .ok_or_else(|| ArrowError::ParseError("invalid NODE.labels".into()))?
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| ArrowError::ParseError("invalid NODE label".into()))
                })
                .collect::<Result<_, _>>()?;
            CypherValue::Node {
                id: int(field("id")?)?,
                labels,
                properties: decode_props_value(field("properties")?)?,
            }
        }
        "RELATIONSHIP" => {
            require_exact_payload_fields(
                "RELATIONSHIP",
                payload,
                &["id", "type", "start_node", "end_node", "properties"],
            )?;
            CypherValue::Relationship {
                id: int(field("id")?)?,
                start_id: int(field("start_node")?)?,
                end_id: int(field("end_node")?)?,
                rel_type: field("type")?
                    .as_str()
                    .ok_or_else(|| ArrowError::ParseError("invalid RELATIONSHIP.type".into()))?
                    .to_owned(),
                properties: decode_props_value(field("properties")?)?,
            }
        }
        "PATH" => {
            require_exact_payload_fields("PATH", payload, &["nodes", "relationships"])?;
            let decode_members = |name: &str| -> Result<Vec<CypherValue>, ArrowError> {
                field(name)?
                    .as_array()
                    .ok_or_else(|| ArrowError::ParseError(format!("invalid PATH.{name}")))?
                    .iter()
                    .map(decode_value)
                    .collect()
            };
            let nodes = decode_members("nodes")?;
            let relationships = decode_members("relationships")?;
            if !nodes.iter().all(|v| matches!(v, CypherValue::Node { .. }))
                || !relationships
                    .iter()
                    .all(|v| matches!(v, CypherValue::Relationship { .. }))
                || nodes.len() != relationships.len().saturating_add(1)
            {
                return Err(ArrowError::ParseError("invalid PATH members".into()));
            }
            CypherValue::Path {
                nodes,
                relationships,
            }
        }
        "DATE" | "TIME" | "DATE_TIME" | "LOCAL_DATE_TIME" | "DURATION" | "POINT" | "GEOMETRY"
        | "VECTOR" => {
            let decoded = try_json_to_cypher_value_v2(payload)
                .map_err(|e| ArrowError::ParseError(e.to_string()))?;
            let valid = matches!(
                (&decoded, tag),
                (CypherValue::Date(_), "DATE")
                    | (CypherValue::Time(..), "TIME")
                    | (CypherValue::DateTime(..), "DATE_TIME")
                    | (CypherValue::LocalDateTime(_), "LOCAL_DATE_TIME")
                    | (CypherValue::Duration(_), "DURATION")
                    | (CypherValue::Point(_), "POINT")
                    | (CypherValue::Geometry(_), "GEOMETRY")
                    | (CypherValue::Vector { .. }, "VECTOR")
            );
            if !valid {
                return Err(ArrowError::ParseError(format!("invalid {tag} payload")));
            }
            decoded
        }
        _ => {
            return Err(ArrowError::ParseError(format!(
                "unknown or invalid typed value: {tag}"
            )))
        }
    };
    Ok(value)
}

#[cfg(test)]
#[path = "typed_json/tests.rs"]
mod tests;
