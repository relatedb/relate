//! Result surface: `QueryResult` columns and stats, and engine-value to
//! Python conversion (nodes, relationships, points, ordered maps).

use super::*;
use crate::result::stats_to_map;

/// The keys of a Python dict, in iteration order.
fn dict_keys(dict: &Bound<'_, PyAny>) -> Vec<String> {
    dict.downcast::<PyDict>()
        .unwrap()
        .keys()
        .iter()
        .map(|k| k.extract().unwrap())
        .collect()
}

#[test]
fn test_write_query_has_no_columns() {
    let mut db = new_database(":memory:");
    let result = raw_query(&mut db, "CREATE (:Person {name: 'Alice'})");
    assert!(result.header.columns.is_empty());
    assert_eq!(result.stats.nodes_created, 1);
}

#[test]
fn test_stats_map_has_all_eleven() {
    let mut db = new_database(":memory:");
    let result = raw_query(&mut db, "CREATE (:Person {name: 'Alice'})");
    let map = stats_to_map(&result.stats);
    assert_eq!(map.len(), 11);
    for key in [
        "nodes_created",
        "nodes_deleted",
        "relationships_created",
        "relationships_deleted",
        "properties_set",
        "labels_added",
        "labels_removed",
        "indexes_added",
        "indexes_removed",
        "constraints_added",
        "constraints_removed",
    ] {
        assert!(map.contains_key(key), "missing stat {key}");
    }
    assert_eq!(map["nodes_created"], 1);
}

#[test]
fn test_execute_exposes_engine_column_types() {
    with_py(|py| {
        let mut db = new_database(":memory:");
        raw_query(&mut db, "CREATE (:P)");
        let typed = db
            .execute(py, "MATCH (n:P) RETURN n, n.name AS name, 1 AS i", None)
            .unwrap();
        assert_eq!(typed.columns, ["n", "name", "i"]);
        assert_eq!(typed.column_types, ["NODE", "ANY", "INTEGER"]);
        let empty = db
            .execute(py, "MATCH (n:Missing) RETURN n.name AS name", None)
            .unwrap();
        assert_eq!(empty.column_types, ["ANY"]);
    });
}

#[test]
fn test_node_conversion_preserves_topology() {
    with_py(|py| {
        let mut db = new_database(":memory:");
        raw_query(&mut db, "CREATE (:Person:User {name: 'Alice', age: 30})");
        let bound = query_first_py(py, &mut db, "MATCH (n) RETURN n", "n");
        let id: i64 = bound.getattr("id").unwrap().extract().unwrap();
        assert!(id >= 0);
        let labels: Vec<String> = bound.getattr("labels").unwrap().extract().unwrap();
        assert_eq!(labels, vec!["Person".to_string(), "User".to_string()]);
        let name: String = bound
            .getattr("properties")
            .unwrap()
            .get_item("name")
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(name, "Alice");
    });
}

#[test]
fn test_relationship_conversion_preserves_endpoints() {
    with_py(|py| {
        let mut db = new_database(":memory:");
        raw_query(&mut db, "CREATE (:A)-[:KNOWS {since: 2020}]->(:B)");
        let bound = query_first_py(py, &mut db, "MATCH ()-[r]->() RETURN r", "r");
        let rtype: String = bound.getattr("type").unwrap().extract().unwrap();
        assert_eq!(rtype, "KNOWS");
        let start: i64 = bound.getattr("start_node").unwrap().extract().unwrap();
        let end: i64 = bound.getattr("end_node").unwrap().extract().unwrap();
        assert_ne!(start, end);
    });
}

#[test]
fn database_execute_and_introspection() {
    with_py(|py| {
        let db = new_database(":memory:");
        let created = db
            .execute(py, "CREATE (:Person)-[:KNOWS]->(:Person)", None)
            .unwrap();
        assert_eq!(created.stats["nodes_created"], 2);
        assert_eq!(created.stats.len(), 11);
        assert_eq!(db.node_count(py).unwrap(), 2);
        assert_eq!(db.relationship_count(py).unwrap(), 1);
        assert_eq!(db.labels(py).unwrap(), vec!["Person"]);
        assert_eq!(db.relationship_types(py).unwrap(), vec!["KNOWS"]);
    });
}

#[test]
fn test_point_result_is_point_instance() {
    with_py(|py| {
        let mut db = new_database(":memory:");
        let bound = query_first_py(py, &mut db, "RETURN point({x: 1.0, y: 2.0}) AS p", "p");
        // A point becomes a Point instance, not a dict.
        assert!(bound.is_instance_of::<Point>());
    });
}

#[test]
fn test_returned_map_preserves_engine_key_order() {
    with_py(|py| {
        // Keys deliberately not alphabetical: the Python dict must keep
        // the engine IndexMap's insertion order (ADR-0219).
        let mut map = IndexMap::new();
        map.insert("zeta".to_string(), CypherValue::Int(1));
        map.insert("alpha".to_string(), CypherValue::Int(2));
        map.insert(
            "mid".to_string(),
            CypherValue::Map(IndexMap::from_iter([
                ("y".to_string(), CypherValue::Int(3)),
                ("b".to_string(), CypherValue::Int(4)),
            ])),
        );
        let obj = cypher_value_to_py(py, &CypherValue::Map(map), Namespace::Neo4j).unwrap();
        let dict = obj.into_bound(py);
        assert_eq!(dict_keys(&dict), vec!["zeta", "alpha", "mid"]);
        let nested = dict.get_item("mid").unwrap();
        assert_eq!(dict_keys(&nested), vec!["y", "b"]);
    });
}
