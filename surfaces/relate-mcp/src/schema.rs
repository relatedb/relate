//! `get_schema` backing: infer the store's schema from the catalog and a
//! sampled scan, in the reference `mcp-neo4j-cypher` server's output shape
//! (ADR-0232 §4).
//!
//! The result is a JSON object keyed by node label:
//!
//! ```json
//! {
//!   "Person": {
//!     "type": "node",
//!     "count": 2,
//!     "properties": {"name": {"type": "String"}},
//!     "relationships": {
//!       "KNOWS": {
//!         "direction": "out",
//!         "labels": ["Person"],
//!         "properties": {"since": {"type": "Integer"}}
//!       }
//!     }
//!   }
//! }
//! ```
//!
//! `count` is exact; property *types* are inferred from a sampled scan
//! (`sample_size` nodes and relationships per label, `-1` for a full scan)
//! because the property graph stores no per-label type declarations. Mixed
//! types on one key keep the first type seen. Labels come from the engine
//! catalog (`CALL db.labels()`); nodes with no label are outside the
//! per-label shape, as in the reference server's APOC-based answer.

use relate::{Connection, Database, QueryResult, Value};
use relate_model::identifier::quote_identifier_always;
use serde_json::{json, Map, Value as Json};

/// Build one property-schema entry (`{"type": …}`), merging a newly seen
/// type with any previously inferred one. Mixed types degrade to the first
/// type seen, matching the reference server's per-key single-type shape.
fn merge_property(properties: &mut Map<String, Json>, key: &str, ty: &str) {
    properties
        .entry(key.to_string())
        .or_insert_with(|| json!({"type": ty}));
}

/// Collect the schema map for every label in the store. `sample_size` is
/// the per-label scan bound: `Some(n)` samples at most n nodes and
/// relationships, `None` is a full scan (the wire/CLI edge's `-1`, already
/// converted by the caller).
pub fn collect_schema(db: &Database, sample_size: Option<u64>) -> Result<Json, String> {
    let mut conn = db
        .connect()
        .map_err(|err| crate::errors::engine_error_text(&err))?;
    let labels_result = conn
        .query("CALL db.labels()")
        .map_err(|err| crate::errors::engine_error_text(&err))?;
    // An empty store answers with no rows at all (and then the executor
    // may report no columns either), so the empty check comes first.
    if labels_result.rows.is_empty() {
        return Ok(Json::Object(Map::new()));
    }
    let label_col = column(&labels_result, "label")?;
    let mut labels: Vec<String> = Vec::new();
    for row in &labels_result.rows {
        if let Some(Value::String(label)) = row.get(label_col) {
            labels.push(label.to_string());
        }
    }

    let mut schema = Map::new();
    for label in &labels {
        schema.insert(label.clone(), label_entry(&mut conn, label, sample_size)?);
    }
    Ok(Json::Object(schema))
}

/// The schema entry for one label, as a readable three-step pipeline:
/// exact count, sampled property types, sampled outgoing relationships.
fn label_entry(
    conn: &mut Connection,
    label: &str,
    sample_size: Option<u64>,
) -> Result<Json, String> {
    let ident = quote_identifier_always(label);
    let limit = LimitClause(sample_size);

    // `count(n)` contract: exactly one integer row. A shape violation is a
    // tool error, never a plausible-looking `"count": 0`.
    let count_result = conn
        .query(&format!("MATCH (n:{ident}) RETURN count(n) AS count"))
        .map_err(|err| crate::errors::engine_error_text(&err))?;
    let count_col = column(&count_result, "count")?;
    let count = count_result
        .rows
        .first()
        .and_then(|row| row.get(count_col))
        .and_then(Value::as_int)
        .ok_or_else(|| "Error: the schema count query returned no integer row".to_string())?;

    let mut properties = Map::new();
    let nodes = conn
        .query(&format!("MATCH (n:{ident}) RETURN n AS node{limit}"))
        .map_err(|err| crate::errors::engine_error_text(&err))?;
    let node_col = column(&nodes, "node")?;
    for row in &nodes.rows {
        if let Some(Value::Node(node)) = row.get(node_col) {
            for (key, value) in &*node.properties {
                if let Some(ty) = value_type(value) {
                    merge_property(&mut properties, key, ty);
                }
            }
        }
    }

    let rels = conn
        .query(&format!(
            "MATCH (n:{ident})-[r]->(m) \
             RETURN type(r) AS rel_type, labels(m) AS target_labels, \
             properties(r) AS props{limit}"
        ))
        .map_err(|err| crate::errors::engine_error_text(&err))?;
    let relationships = fold_relationship_rows(&rels)?;

    let mut entry = Map::new();
    entry.insert("type".to_string(), json!("node"));
    entry.insert("count".to_string(), json!(count));
    if !properties.is_empty() {
        entry.insert("properties".to_string(), Json::Object(properties));
    }
    if !relationships.is_empty() {
        entry.insert("relationships".to_string(), Json::Object(relationships));
    }
    Ok(Json::Object(entry))
}

/// The per-relationship-type accumulation, typed so the untyped
/// entry/or_insert/as_object_mut plumbing (and its provably-dead fallback
/// branches) disappears: rows fold into this, and the wire shape is
/// serialized once at the end.
#[derive(Default)]
struct RelSchema {
    /// The target labels seen, deduplicated and ordered.
    labels: std::collections::BTreeSet<String>,
    /// The property-key → inferred-type entries.
    properties: Map<String, Json>,
}

/// Fold the relationship sample rows into the wire `relationships` map:
/// `{TYPE: {"direction": "out", "labels": […], "properties": {…}}}`.
fn fold_relationship_rows(rels: &QueryResult) -> Result<Map<String, Json>, String> {
    let type_col = column(rels, "rel_type")?;
    let labels_col = column(rels, "target_labels")?;
    let props_col = column(rels, "props")?;
    let mut by_type: std::collections::BTreeMap<String, RelSchema> =
        std::collections::BTreeMap::new();
    for row in &rels.rows {
        let Some(Value::String(rel_type)) = row.get(type_col) else {
            continue;
        };
        let entry = by_type.entry(rel_type.to_string()).or_default();
        if let Some(Value::List(targets)) = row.get(labels_col) {
            for target in targets.iter() {
                if let Value::String(target) = target {
                    entry.labels.insert(target.to_string());
                }
            }
        }
        if let Some(Value::Map(props)) = row.get(props_col) {
            for (key, value) in props.iter() {
                if let Some(ty) = value_type(value) {
                    merge_property(&mut entry.properties, key, ty);
                }
            }
        }
    }
    let mut relationships = Map::new();
    for (rel_type, schema) in by_type {
        let mut obj = Map::new();
        obj.insert("direction".to_string(), json!("out"));
        if !schema.labels.is_empty() {
            let labels: Vec<Json> = schema.labels.into_iter().map(Json::String).collect();
            obj.insert("labels".to_string(), Json::Array(labels));
        }
        if !schema.properties.is_empty() {
            obj.insert("properties".to_string(), Json::Object(schema.properties));
        }
        relationships.insert(rel_type, Json::Object(obj));
    }
    Ok(relationships)
}

/// The `LIMIT n` suffix for a sampling query; empty for a full scan.
struct LimitClause(Option<u64>);

impl std::fmt::Display for LimitClause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(n) => write!(f, " LIMIT {n}"),
            None => Ok(()),
        }
    }
}

/// The column index of `name` in a result, or an internal error string.
fn column(result: &QueryResult, name: &str) -> Result<usize, String> {
    result
        .header
        .columns
        .iter()
        .position(|c| c == name)
        .ok_or_else(|| format!("Error: schema query result is missing the \"{name}\" column"))
}

/// The schema type name for a sampled value; `None` for `null` (a null
/// sample says nothing about the key's type).
fn value_type(value: &Value) -> Option<&'static str> {
    match value {
        Value::Null => None,
        Value::Bool(_) => Some("Boolean"),
        Value::Int(_) => Some("Integer"),
        Value::Float(_) => Some("Float"),
        Value::String(_) => Some("String"),
        Value::List(_) => Some("List"),
        Value::Map(_) => Some("Map"),
        Value::Node(_) => Some("Node"),
        Value::Relationship(_) => Some("Relationship"),
        Value::Path(_) => Some("Path"),
        Value::Temporal(_) => Some("Temporal"),
        Value::Point(_) => Some("Point"),
        Value::Uuid(_) => Some("String"),
        // A byte array is never stored (ADR-0327); off Bolt it crosses as
        // LIST<INTEGER>, so it would sample as a List.
        Value::Bytes(_) => Some("List"),
        Value::Vector { .. } => Some("Vector"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded_db() -> Database {
        let db = Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        conn.query(
            "CREATE (a:Person {name: 'Ada', born: 1815})-[:KNOWS {since: 1843}]->(b:Person {name: 'Bob'})",
        )
        .unwrap();
        conn.query("CREATE (:City {name: 'Paris', lat: 48.85})")
            .unwrap();
        db
    }

    #[test]
    fn schema_matches_the_reference_output_shape() {
        let schema = collect_schema(&seeded_db(), Some(1000)).unwrap();
        let person = &schema["Person"];
        assert_eq!(person["type"], json!("node"));
        assert_eq!(person["count"], json!(2));
        assert_eq!(person["properties"]["name"]["type"], json!("String"));
        assert_eq!(person["properties"]["born"]["type"], json!("Integer"));
        let knows = &person["relationships"]["KNOWS"];
        assert_eq!(knows["direction"], json!("out"));
        assert_eq!(knows["labels"], json!(["Person"]));
        assert_eq!(knows["properties"]["since"]["type"], json!("Integer"));
        assert_eq!(schema["City"]["properties"]["lat"]["type"], json!("Float"));
    }

    #[test]
    fn sample_size_limits_the_scan() {
        let schema = collect_schema(&seeded_db(), Some(1)).unwrap();
        // The count is exact even when sampling sees fewer nodes.
        assert_eq!(schema["Person"]["count"], json!(2));
        // None is the full scan.
        let schema = collect_schema(&seeded_db(), None).unwrap();
        assert_eq!(schema["Person"]["count"], json!(2));
    }

    #[test]
    fn empty_store_has_an_empty_schema() {
        let db = Database::open(":memory:").unwrap();
        assert_eq!(collect_schema(&db, Some(1000)).unwrap(), json!({}));
    }

    #[test]
    fn backticked_labels_round_trip_through_the_sampling_queries() {
        let db = Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        conn.query("CREATE (n:`we``ird` {name: 'x'})").unwrap();
        let schema = collect_schema(&db, Some(1000)).unwrap();
        assert_eq!(schema["we`ird"]["count"], json!(1));
    }
}
