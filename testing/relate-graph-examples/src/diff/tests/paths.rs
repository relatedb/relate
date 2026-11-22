use super::*;
use relate_model::CypherValue;

fn neo4j_node(element_id: &str, labels: &[&str], props: Json) -> Json {
    serde_json::json!({
        "elementId": element_id,
        "labels": labels,
        "properties": props,
    })
}

fn neo4j_rel(element_id: &str, rel_type: &str, props: Json) -> Json {
    serde_json::json!({
        "elementId": element_id,
        "startNodeElementId": "4:aaa:0",
        "endNodeElementId": "4:aaa:1",
        "type": rel_type,
        "properties": props,
    })
}

fn cypher_node(id: i64, labels: &[&str], props: Vec<(&str, CypherValue)>) -> CypherValue {
    CypherValue::Node {
        id,
        labels: labels.iter().map(|s| s.to_string()).collect(),
        properties: props.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
    }
}

fn cypher_rel(id: i64, rel_type: &str, props: Vec<(&str, CypherValue)>) -> CypherValue {
    CypherValue::Relationship {
        id,
        start_id: 0,
        end_id: 1,
        rel_type: rel_type.into(),
        properties: props.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
    }
}

/// The LABS-171 case: a 1-hop path is `[node, rel, node]` on the
/// Query API v2 wire but `Path { nodes, relationships }` on the
/// relate side; both must canonicalize to the same form.
#[test]
fn one_hop_paths_compare_equal_across_wire_shapes() {
    let left = row_json(serde_json::json!([
        neo4j_node("4:aaa:0", &["Person"], serde_json::json!({"name": "Tom"})),
        neo4j_rel("5:aaa:0", "KNOWS", serde_json::json!({"since": 2020})),
        neo4j_node("4:aaa:1", &["Person"], serde_json::json!({"name": "Ann"})),
    ]));
    let right = row_cypher(CypherValue::Path {
        nodes: vec![
            cypher_node(
                0,
                &["Person"],
                vec![("name", CypherValue::Str("Tom".into()))],
            ),
            cypher_node(
                1,
                &["Person"],
                vec![("name", CypherValue::Str("Ann".into()))],
            ),
        ],
        relationships: vec![cypher_rel(
            0,
            "KNOWS",
            vec![("since", CypherValue::Int(2020))],
        )],
    });
    assert_eq!(
        compare_rows(&left, &right, StatementOrder::Unordered),
        Relation::Exact
    );
    // rows_subset (the LIMIT/SKIP check) shares the normalization.
    assert!(rows_subset(&left, &right).is_ok());
}

#[test]
fn multi_hop_paths_compare_equal_across_wire_shapes() {
    let left = row_json(serde_json::json!([
        neo4j_node("4:aaa:0", &["A"], serde_json::json!({"n": 1})),
        neo4j_rel("5:aaa:0", "R1", serde_json::json!({})),
        neo4j_node("4:aaa:1", &["B"], serde_json::json!({"n": 2})),
        neo4j_rel("5:aaa:1", "R2", serde_json::json!({"w": 0.5})),
        neo4j_node("4:aaa:2", &["C"], serde_json::json!({"n": 3})),
    ]));
    let right = row_cypher(CypherValue::Path {
        nodes: vec![
            cypher_node(0, &["A"], vec![("n", CypherValue::Int(1))]),
            cypher_node(1, &["B"], vec![("n", CypherValue::Int(2))]),
            cypher_node(2, &["C"], vec![("n", CypherValue::Int(3))]),
        ],
        relationships: vec![
            cypher_rel(0, "R1", vec![]),
            cypher_rel(1, "R2", vec![("w", CypherValue::Float(0.5))]),
        ],
    });
    assert_eq!(
        compare_rows(&left, &right, StatementOrder::Unordered),
        Relation::Exact
    );
}

#[test]
fn differing_paths_still_diff() {
    let wire = serde_json::json!([
        neo4j_node("4:aaa:0", &["Person"], serde_json::json!({"name": "Tom"})),
        neo4j_rel("5:aaa:0", "KNOWS", serde_json::json!({})),
        neo4j_node("4:aaa:1", &["Person"], serde_json::json!({"name": "Ann"})),
    ]);
    let tom = cypher_node(
        0,
        &["Person"],
        vec![("name", CypherValue::Str("Tom".into()))],
    );
    let ann = cypher_node(
        1,
        &["Person"],
        vec![("name", CypherValue::Str("Ann".into()))],
    );

    // Node property differs.
    let right = row_cypher(CypherValue::Path {
        nodes: vec![
            tom.clone(),
            cypher_node(
                1,
                &["Person"],
                vec![("name", CypherValue::Str("Bob".into()))],
            ),
        ],
        relationships: vec![cypher_rel(0, "KNOWS", vec![])],
    });
    assert!(matches!(
        compare_rows(&row_json(wire.clone()), &right, StatementOrder::Unordered),
        Relation::Different(_)
    ));

    // Relationship type differs.
    let right = row_cypher(CypherValue::Path {
        nodes: vec![tom.clone(), ann.clone()],
        relationships: vec![cypher_rel(0, "LIKES", vec![])],
    });
    assert!(matches!(
        compare_rows(&row_json(wire.clone()), &right, StatementOrder::Unordered),
        Relation::Different(_)
    ));

    // Hop count differs.
    let right = row_cypher(CypherValue::Path {
        nodes: vec![tom, ann.clone(), ann],
        relationships: vec![
            cypher_rel(0, "KNOWS", vec![]),
            cypher_rel(1, "KNOWS", vec![]),
        ],
    });
    assert!(matches!(
        compare_rows(&row_json(wire), &right, StatementOrder::Unordered),
        Relation::Different(_)
    ));
}

#[test]
fn non_path_rows_compare_as_before() {
    // Lists stay positional (no reordering) in the strict comparison —
    // a reordered list is an order-only difference, not a diff.
    let left = row_json(serde_json::json!([1, 2]));
    let right = row_cypher(CypherValue::List(vec![
        CypherValue::Int(2),
        CypherValue::Int(1),
    ]));
    assert!(matches!(
        compare_rows(&left, &right, StatementOrder::Unordered),
        Relation::UpToListOrder(_)
    ));
    // …and equal in order.
    let right = row_cypher(CypherValue::List(vec![
        CypherValue::Int(1),
        CypherValue::Int(2),
    ]));
    assert_eq!(
        compare_rows(&left, &right, StatementOrder::Unordered),
        Relation::Exact
    );
    // Scalars and maps are untouched by the path canonicalization.
    let left = row_json(serde_json::json!({"a": 1, "b": "x"}));
    let right = row_cypher(CypherValue::Map(
        [
            ("a".to_string(), CypherValue::Int(1)),
            ("b".to_string(), CypherValue::Str("x".into())),
        ]
        .into_iter()
        .collect(),
    ));
    assert_eq!(
        compare_rows(&left, &right, StatementOrder::Unordered),
        Relation::Exact
    );
}
