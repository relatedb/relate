//! LABS-1605: a filtered list comprehension's map expression may be a map
//! projection — the pipe-ambiguity dead-end set admits `{` beside `.`.

use crate::common;

use common::connection;
use relate::Value;

/// The repro's exact shape, executing: the WHERE keeps its label
/// predicate, the map expression is the projection, and each surviving
/// element projects through `n{.p1}`.
#[test]
fn a_filtered_list_comprehension_projects_through_a_map_projection() {
    let mut c = connection();
    c.query("CREATE (:L1 {p1: 7})-[:R]->(:L2 {p1: 8})").unwrap();
    let result = c
        .query(
            "MATCH p=(a:L1)-[:R]->(b:L2) \
             RETURN [n IN nodes(p) WHERE n:L1 | n{.p1}] AS x",
        )
        .unwrap();
    assert_eq!(result.rows.len(), 1);
    let Value::List(items) = &result.rows[0][0] else {
        panic!("expected a list, got {:?}", result.rows[0][0]);
    };
    assert_eq!(items.len(), 1);
    let Value::Map(projected) = &items[0] else {
        panic!("expected a projected map, got {:?}", items[0]);
    };
    assert_eq!(projected.len(), 1);
    assert_eq!(projected.get("p1"), Some(&Value::Int(7)));
    // The unfiltered projection over both nodes, for the contrast: the
    // :L2 node's map carries its own p1.
    let result = c
        .query(
            "MATCH p=(a:L1)-[:R]->(b:L2) \
             RETURN [n IN nodes(p) | n{.p1}] AS x",
        )
        .unwrap();
    let Value::List(items) = &result.rows[0][0] else {
        panic!("expected a list, got {:?}", result.rows[0][0]);
    };
    assert_eq!(items.len(), 2);
}
