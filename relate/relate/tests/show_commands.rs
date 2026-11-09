//! `SHOW` command surface: the two default lookup indexes every store
//! carries, the catalog's name ordering, and a store-only constraint's
//! position in the `AS GRAPH` view. Expected values are live Neo4j
//! 2026.10.1's (`SHOW INDEXES`, `SHOW ALL FUNCTIONS` and
//! `SHOW CURRENT GRAPH TYPE AS GRAPH` probed against bolt://localhost:7687).

mod common;

use common::{connection, string};
use relate::Value;

/// Live Neo4j 2026.10.1 lists both default lookup indexes on a fresh
/// database, relationship first: `index_1b9dcc97` (RELATIONSHIP) then
/// `index_460996c0` (NODE), both ONLINE at 100.0 %, provider
/// `token-lookup-1.0`, no token lists, no owning constraint, no read
/// statistics.
#[test]
fn default_lookup_indexes_are_listed() {
    let mut c = connection();
    let rows = common::rows(
        &mut c,
        "SHOW INDEXES \
         YIELD name, state, populationPercent, type, entityType, labelsOrTypes, \
         properties, indexProvider, owningConstraint, lastRead, readCount \
         RETURN name, state, populationPercent, type, entityType, labelsOrTypes, \
         properties, indexProvider, owningConstraint, lastRead, readCount",
    );
    assert_eq!(
        rows,
        vec![
            vec![
                string("index_1b9dcc97"),
                string("ONLINE"),
                Value::Float(100.0),
                string("LOOKUP"),
                string("RELATIONSHIP"),
                Value::Null,
                Value::Null,
                string("token-lookup-1.0"),
                Value::Null,
                Value::Null,
                Value::Int(0),
            ],
            vec![
                string("index_460996c0"),
                string("ONLINE"),
                Value::Float(100.0),
                string("LOOKUP"),
                string("NODE"),
                Value::Null,
                Value::Null,
                string("token-lookup-1.0"),
                Value::Null,
                Value::Null,
                Value::Int(0),
            ],
        ]
    );
}

/// `SHOW LOOKUP INDEXES` projects the defaults (the `YIELD *` columns
/// included) and a kind-filtered SHOW of another kind leaves them out.
#[test]
fn show_lookup_indexes_projects_the_defaults() {
    let mut c = connection();
    let rows = common::rows(
        &mut c,
        "SHOW LOOKUP INDEXES YIELD * \
         RETURN name, createStatement, failureMessage",
    );
    assert_eq!(
        rows,
        vec![
            vec![
                string("index_1b9dcc97"),
                string("CREATE LOOKUP INDEX `index_1b9dcc97` FOR ()-[r]-() ON EACH type(r)"),
                string(""),
            ],
            vec![
                string("index_460996c0"),
                string("CREATE LOOKUP INDEX `index_460996c0` FOR (n) ON EACH labels(n)"),
                string(""),
            ],
        ]
    );

    c.query("CREATE RANGE INDEX my_index FOR (n:L) ON (n.p)")
        .unwrap();
    let rows = common::rows(&mut c, "SHOW RANGE INDEXES YIELD name RETURN name");
    assert_eq!(rows, vec![vec![string("my_index")]]);
}

/// Live Neo4j cross-joins `SHOW INDEXES` (three rows on a store with one
/// range index) with `SHOW RANGE INDEXES` (one row): each index row
/// repeats once per right-hand row.
#[test]
fn show_indexes_cross_joined_with_show_range_indexes_repeats_each_row() {
    let mut c = connection();
    c.query("CREATE INDEX my_index FOR (n:L) ON (n.p)").unwrap();
    let rows = common::rows(
        &mut c,
        "SHOW INDEXES YIELD type, entityType \
         SHOW RANGE INDEXES YIELD name AS range \
         RETURN type, entityType, range ORDER BY type, entityType",
    );
    assert_eq!(
        rows,
        vec![
            vec![string("LOOKUP"), string("NODE"), string("my_index")],
            vec![string("LOOKUP"), string("RELATIONSHIP"), string("my_index")],
            vec![string("RANGE"), string("NODE"), string("my_index")],
        ]
    );
}

/// Live Neo4j lists `SHOW FUNCTIONS` in name order (`abs`, `acos`, then
/// `all`…), arity rows of one name adjacent.
#[test]
fn function_catalog_is_name_ordered() {
    let mut c = connection();
    let rows = common::rows(&mut c, "SHOW ALL FUNCTIONS YIELD name RETURN name");
    let names: Vec<&str> = rows
        .iter()
        .map(|row| row[0].as_string().unwrap_or_else(|| panic!("{row:?}")))
        .collect();
    assert_eq!(&names[..2], &["abs", "acos"], "{names:?}");
    assert!(
        names.windows(2).all(|pair| pair[0] <= pair[1]),
        "not name-ordered: {names:?}"
    );
}

/// Live Neo4j renders a plain label's store-only constraints by property,
/// then kind (KEY before the property type), whatever their creation
/// order — here `KEY p1`, `:: INTEGER p1`, `KEY p2`.
#[test]
fn store_constraints_render_by_property_then_kind() {
    let mut c = connection();
    c.query("CREATE CONSTRAINT my_constraint1 FOR (n:L) REQUIRE (n.p1) IS :: INTEGER")
        .unwrap();
    c.query("CREATE CONSTRAINT my_constraint2 FOR (n:L) REQUIRE (n.p1) IS NODE KEY")
        .unwrap();
    c.query("CREATE CONSTRAINT my_constraint3 FOR (n:L) REQUIRE (n.p2) IS NODE KEY")
        .unwrap();

    // The composed form the corpus exercises: LIMIT 1 over SHOW SETTINGS
    // cross-joined with the graph-type node.
    let rows = common::rows(
        &mut c,
        "SHOW SETTINGS YIELD name AS setting LIMIT 1 \
         SHOW CURRENT GRAPH TYPE AS GRAPH YIELD nodes \
         RETURN setting, nodes",
    );
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0][0], string("browser.allow_outgoing_connections"));
    let Value::List(nodes) = &rows[0][1] else {
        panic!("expected a nodes list, got {:?}", rows[0][1]);
    };
    assert_eq!(nodes.len(), 1, "{nodes:?}");
    let Value::Node(node) = &nodes[0] else {
        panic!("expected a virtual node, got {:?}", nodes[0]);
    };
    assert_eq!(node.labels.as_slice(), &["NodeLabel".to_string()]);
    assert_eq!(
        node.properties.get("label"),
        Some(&string("L")),
        "{:?}",
        node.properties
    );
    let Some(Value::List(constraints)) = node.properties.get("constraints") else {
        panic!("expected a constraints list, got {:?}", node.properties);
    };
    let texts: Vec<&str> = constraints
        .iter()
        .map(|value| value.as_string().unwrap_or_else(|| panic!("{value:?}")))
        .collect();
    assert_eq!(
        texts,
        vec![
            "CONSTRAINT my_constraint2 FOR (n:L) REQUIRE n.p1 IS KEY",
            "CONSTRAINT my_constraint1 FOR (n:L) REQUIRE n.p1 IS :: INTEGER",
            "CONSTRAINT my_constraint3 FOR (n:L) REQUIRE n.p2 IS KEY",
        ]
    );
}
