//! Merge-key derivation (ADR-0401 §1): the local store's node and
//! relationship uniqueness constraints — read through `SHOW CONSTRAINTS`
//! — become the push's merge keys, one per label/type, composite keys
//! included. A `--merge-key LABEL:PROP[,PROP]` override wins over the
//! constraint-derived key for that label.

use std::collections::BTreeMap;

use relate::{Connection, Value};

use crate::schema::{parse_constraint_kind, ConstraintShape};
use crate::LoadError;

/// The merge-key tables the plan is built from: node label → key
/// properties, relationship type → key properties. Sorted maps, so the
/// plan's entity order is deterministic.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct MergeKeys {
    pub nodes: BTreeMap<String, Vec<String>>,
    pub rels: BTreeMap<String, Vec<String>>,
}

impl MergeKeys {
    /// The key properties for a node label, or none — unkeyed. The table
    /// already holds the winner; [`read_merge_keys`] decides precedence.
    pub(crate) fn node_key(&self, label: &str) -> Option<&[String]> {
        self.nodes.get(label).map(Vec::as_slice)
    }

    /// The key properties for a relationship type, or none — structural
    /// (ADR-0401 §5). Precedence is decided in [`read_merge_keys`].
    pub(crate) fn rel_key(&self, rel_type: &str) -> Option<&[String]> {
        self.rels.get(rel_type).map(Vec::as_slice)
    }
}

/// Read the store's merge keys: `SHOW CONSTRAINTS` rows whose kind is a
/// uniqueness or (node/relationship) key constraint — both guarantee
/// uniqueness, so both are merge keys — projected per label/type. A label
/// with several such constraints uses the alphabetically-first constraint
/// name's properties. `node_overrides` / `rel_overrides` are the parsed
/// `--merge-key` / `--rel-key` values; precedence is decided here, once:
/// an override (explicit user intent) beats the constraint-derived key
/// for its label or type, which beats no key (ADR-0401 §5).
pub(crate) fn read_merge_keys(
    conn: &mut Connection,
    node_overrides: &BTreeMap<String, Vec<String>>,
    rel_overrides: &BTreeMap<String, Vec<String>>,
) -> Result<MergeKeys, LoadError> {
    let result = conn
        .query("SHOW CONSTRAINTS YIELD name, type, entityType, labelsOrTypes, properties")
        .map_err(|e| LoadError::Engine(e.to_string()))?;
    let columns: Vec<&str> = result.header.columns.iter().map(String::as_str).collect();
    let mut constraints: Vec<(String, bool, String, Vec<String>)> = Vec::new();
    for row in &result.rows {
        // One typed read of the five YIELD columns: a missing or mistyped
        // column is this row's own error, never a silent skip (F-1-13).
        let row: BTreeMap<&str, &Value> = columns.iter().copied().zip(row.iter()).collect();
        let name = required_str(row.get("name").copied(), "name")?;
        let kind = required_str(row.get("type").copied(), "type")?;
        let entity_type = required_str(row.get("entityType").copied(), "entityType")?;
        let labels = required_string_list(row.get("labelsOrTypes").copied(), "labelsOrTypes")?;
        let properties = required_string_list(row.get("properties").copied(), "properties")?;
        // The kind's entity prefix must equal entityType (F-4-1/2), and only
        // uniqueness and key shapes are merge keys; existence and property
        // type constraints are not.
        let Some((kind_entity, shape)) = parse_constraint_kind(&kind) else {
            continue;
        };
        if kind_entity != entity_type {
            return Err(LoadError::Engine(format!(
                "SHOW CONSTRAINTS row {name:?} has kind {kind:?} on entityType {entity_type:?}"
            )));
        }
        if !matches!(
            shape,
            ConstraintShape::PropertyUniqueness | ConstraintShape::Key
        ) {
            continue;
        }
        // A merge key is per one label/type: composite *properties* are a
        // key, a constraint spanning several labels is not.
        let [label] = labels.as_slice() else {
            continue;
        };
        if properties.is_empty() {
            return Err(LoadError::Engine(format!(
                "SHOW CONSTRAINTS row {name:?} has no properties"
            )));
        }
        constraints.push((
            name,
            entity_type == "RELATIONSHIP",
            label.clone(),
            properties,
        ));
    }
    // Deterministic pick: rows sorted by constraint name; the first
    // constraint per label/type wins.
    constraints.sort_by(|a, b| a.0.cmp(&b.0));
    let mut keys = MergeKeys::default();
    for (_, relationship, label, properties) in constraints {
        let table = if relationship {
            &mut keys.rels
        } else {
            &mut keys.nodes
        };
        table.entry(label).or_insert(properties);
    }
    // The explicit --merge-key / --rel-key overrides win for their
    // labels and types.
    for (label, properties) in node_overrides {
        keys.nodes.insert(label.clone(), properties.clone());
    }
    for (rel_type, properties) in rel_overrides {
        keys.rels.insert(rel_type.clone(), properties.clone());
    }
    Ok(keys)
}

/// A required string column of a SHOW row: a missing or mistyped column is
/// the row's own error (F-1-13), never a silent "".
fn required_str(value: Option<&Value>, column: &str) -> Result<String, LoadError> {
    value
        .and_then(Value::as_string)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            LoadError::Engine(format!(
                "a SHOW CONSTRAINTS row is missing column {column:?}"
            ))
        })
}

/// A required LIST<STRING> column of a SHOW row.
fn required_string_list(value: Option<&Value>, column: &str) -> Result<Vec<String>, LoadError> {
    let items = match value {
        Some(Value::List(items)) => items,
        _ => {
            return Err(LoadError::Engine(format!(
                "a SHOW CONSTRAINTS row is missing list column {column:?}"
            )))
        }
    };
    let parsed = items
        .iter()
        .map(|item| {
            item.as_string()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .ok_or_else(|| {
                    LoadError::Engine(format!(
                        "column {column:?} of a SHOW CONSTRAINTS row has a non-string entry"
                    ))
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use relate::Database;

    fn seeded_conn() -> (Database, relate::Connection) {
        let db = Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        for ddl in [
            "CREATE CONSTRAINT m_title IF NOT EXISTS FOR (m:Movie) REQUIRE m.title IS UNIQUE",
            "CREATE CONSTRAINT p_name IF NOT EXISTS FOR (p:Person) REQUIRE p.name IS UNIQUE",
            "CREATE CONSTRAINT p_tag IF NOT EXISTS FOR (p:Person) REQUIRE (p.tenant, p.tag) IS NODE KEY",
            "CREATE CONSTRAINT p_bio IF NOT EXISTS FOR (p:Person) REQUIRE p.bio IS NOT NULL",
            "CREATE CONSTRAINT p_year IF NOT EXISTS FOR (p:Person) REQUIRE p.year IS TYPED INTEGER",
            "CREATE CONSTRAINT r_since IF NOT EXISTS FOR ()-[r:ACTED_IN]-() REQUIRE r.since IS UNIQUE",
        ] {
            conn.query(ddl)
                .unwrap_or_else(|e| panic!("{ddl}: {e}"));
        }
        (db, conn)
    }

    /// Uniqueness and node/relationship key constraints are merge keys,
    /// composite keys included; existence and property-type constraints are
    /// not. Several constraints on one label pick the alphabetically-first
    /// constraint name's properties.
    #[test]
    fn merge_keys_derive_from_constraints() {
        let (_db, mut conn) = seeded_conn();
        let keys = read_merge_keys(&mut conn, &BTreeMap::new(), &BTreeMap::new()).unwrap();
        let movie_key = ["title".to_string()];
        // Person has two candidate constraints; `p_name` sorts first.
        assert_eq!(keys.node_key("Movie"), Some(movie_key.as_slice()));
        assert_eq!(keys.node_key("Person"), Some(&["name".to_string()][..]));
        assert_eq!(keys.node_key("Studio"), None);
        assert_eq!(keys.rel_key("ACTED_IN"), Some(&["since".to_string()][..]));
        assert_eq!(keys.rel_key("DIRECTED"), None);
    }

    /// A composite NODE KEY constraint is a composite merge key when it is
    /// the only candidate (a label where the uniqueness constraint's name
    /// sorts later).
    #[test]
    fn composite_key_constraints_are_composite_merge_keys() {
        let db = Database::open(":memory:").unwrap();
        let mut conn = db.connect().unwrap();
        conn.query(
            "CREATE CONSTRAINT z_tag IF NOT EXISTS FOR (p:Event) REQUIRE (p.tenant, p.code) IS NODE KEY",
        )
        .unwrap();
        let keys = read_merge_keys(&mut conn, &BTreeMap::new(), &BTreeMap::new()).unwrap();
        assert_eq!(
            keys.node_key("Event"),
            Some(&["tenant".to_string(), "code".to_string()][..])
        );
    }

    /// A `--merge-key` override wins over the constraint-derived key for
    /// its label and supplies one for an unconstrained label.
    #[test]
    fn merge_key_overrides_win_and_supply() {
        let (_db, mut conn) = seeded_conn();
        let mut overrides = BTreeMap::new();
        overrides.insert("Person".to_string(), vec!["email".to_string()]);
        overrides.insert("Studio".to_string(), vec!["id".to_string()]);
        let keys = read_merge_keys(&mut conn, &overrides, &BTreeMap::new()).unwrap();
        assert_eq!(keys.node_key("Person"), Some(&["email".to_string()][..]));
        assert_eq!(keys.node_key("Studio"), Some(&["id".to_string()][..]));
    }

    /// A `--rel-key` override wins over the constraint-derived key for its
    /// type and supplies one for an unconstrained type (ADR-0401 §5).
    #[test]
    fn rel_key_overrides_win_and_supply() {
        let (_db, mut conn) = seeded_conn();
        let mut rel_overrides = BTreeMap::new();
        rel_overrides.insert("ACTED_IN".to_string(), vec!["role".to_string()]);
        rel_overrides.insert("DIRECTED".to_string(), vec!["edition".to_string()]);
        let keys = read_merge_keys(&mut conn, &BTreeMap::new(), &rel_overrides).unwrap();
        assert_eq!(keys.rel_key("ACTED_IN"), Some(&["role".to_string()][..]));
        assert_eq!(keys.rel_key("DIRECTED"), Some(&["edition".to_string()][..]));
    }
}
