//! Relate GraphData property JSON (`__type` tags) helpers.
//!
//! The general `Value` <-> CypherValue bridge lives in relate-core
//! (`relate_core::{value_to_cypher, cypher_to_value}`).

use std::collections::BTreeMap;

use relate_core::{cypher_to_value, value_to_cypher, Error, PropertyMap, Result, Temporal, Value};
use relate_model::serde_value::{
    props_to_json, try_json_entries_to_props_legacy, try_json_entries_to_props_v2,
};
use relate_model::CypherValue;
use serde_json::Value as JsonValue;

pub fn props_to_cypher_json(props: &PropertyMap) -> Result<serde_json::Map<String, JsonValue>> {
    // The core bridge can narrow or stringify some temporals and lists a
    // byte array's values (ADR-0327). Inspect the original values (including
    // nested ones) before converting any property.
    for (key, value) in props {
        validate_persistable(value, &format!("$.{key}"))?;
    }
    let properties = props
        .iter()
        .map(|(key, value)| (key.clone(), value_to_cypher(value)))
        .collect();
    props_to_json(&properties).map_err(|e| Error::storage(format!("invalid graph property {e}")))
}

fn validate_persistable(value: &Value, path: &str) -> Result<()> {
    let reason = match value {
        // Statement-scoped (ADR-0327): the executor rejects it as a property
        // with a typed TypeError first; never persist it as a list.
        Value::Bytes(_) => Some("byte array is not a persistence property"),
        Value::Temporal(Temporal::CivilDate { .. }) => Some("unsupported CivilDate"),
        Value::Temporal(Temporal::CivilDateTime { .. }) => Some("unsupported CivilDateTime"),
        Value::Temporal(Temporal::Duration { nanos, .. })
            if i64::try_from(nanos / 1_000_000_000).is_err() =>
        {
            Some("Duration seconds out of range")
        }
        Value::Temporal(Temporal::Time { offset, .. } | Temporal::DateTime { offset, .. })
            // Chrono's FixedOffset::east_opt accepts only (-86400, 86400).
            // The JSON writer represents offsets to minute precision.
            if *offset % 60 != 0 || !(-86_399..=86_399).contains(offset) =>
        {
            Some("invalid temporal offset")
        }
        Value::List(items) => {
            for (index, item) in items.iter().enumerate() {
                validate_persistable(item, &format!("{path}[{index}]"))?;
            }
            None
        }
        Value::Map(entries) => {
            for (key, item) in entries.iter() {
                validate_persistable(item, &format!("{path}.{key}"))?;
            }
            None
        }
        _ => None,
    };
    match reason {
        Some(reason) => Err(Error::storage(format!(
            "invalid graph property {path}: {reason}"
        ))),
        None => Ok(()),
    }
}

/// Decode a property map's JSON entries through the store's value encoding.
/// Takes the entries by reference (`&Map`, `&BTreeMap` or any iterator of
/// borrowed pairs), so no caller rebuilds an owned map to call it. A decode
/// failure names the property key path (`$.key...`) like the map decoders.
pub fn props_from_cypher_json<'a>(
    props: impl IntoIterator<Item = (&'a String, &'a JsonValue)>,
    value_encoding: u64,
) -> Result<BTreeMap<String, Value>> {
    let decoded = match value_encoding {
        1 => try_json_entries_to_props_legacy(props),
        // Encoding 3 is value encoding 2 plus the inline ontology field
        // (ADR-0120 §6); the property encoding is identical.
        2 | 3 => try_json_entries_to_props_v2(props),
        _ => return Err(Error::storage("unsupported graph value encoding")),
    }
    .map_err(|e| Error::storage(format!("invalid graph property {e}")))?;
    // The core bridge maps Geometry to Null, including inside lists and maps.
    // Validate the whole property map before converting any of its values.
    for (key, value) in &decoded {
        reject_geometry(value, &format!("$.{key}"))?;
    }
    Ok(decoded
        .into_iter()
        .map(|(key, value)| (key, cypher_to_value(&value)))
        .collect())
}

fn reject_geometry(value: &CypherValue, path: &str) -> Result<()> {
    match value {
        CypherValue::Geometry(_) => Err(Error::storage(format!(
            "invalid graph property {path}: unsupported Geometry"
        ))),
        CypherValue::List(items) => {
            for (index, item) in items.iter().enumerate() {
                reject_geometry(item, &format!("{path}[{index}]"))?;
            }
            Ok(())
        }
        CypherValue::Map(entries) => {
            for (key, item) in entries {
                reject_geometry(item, &format!("{path}.{key}"))?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn borrowed_entries_decode_like_a_map_and_name_the_key_path() {
        let ok: BTreeMap<String, JsonValue> =
            BTreeMap::from([("a".to_owned(), json!(1)), ("b".to_owned(), json!("x"))]);
        let decoded = props_from_cypher_json(&ok, 2).unwrap();
        assert_eq!(decoded.len(), 2);

        let key = "p".to_owned();
        let bad = json!({"__type": "point", "srid": 4326, "x": 1.5});
        let err = props_from_cypher_json([(&key, &bad)], 2).unwrap_err();
        assert!(
            err.to_string().contains("invalid graph property $.p.y"),
            "{err}"
        );
        assert!(props_from_cypher_json(&ok, 9).is_err());
    }
}
