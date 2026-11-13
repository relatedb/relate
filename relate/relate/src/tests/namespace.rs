//! Crate-internal namespace tests (ADR-0116 §5, §10): namespace identity
//! of the slots and the closed `system_ontologies` visibility gate
//! (ADR-0593 — this binary must never opt into a preview feature; the
//! visible-namespace rules, ADR-0229's 42N17 gate included, live in
//! `tests/namespace.rs` under the opt-in).

use relate_core::Namespace;

use crate::{Database, Value};

#[test]
fn namespace_slots_have_their_namespace_identity() {
    let db = Database::open(":memory:").unwrap();
    let shared = db.shared();
    assert_eq!(shared.graph.namespace(), Namespace::Neo4j);
    assert_eq!(shared.system.namespace(), Namespace::System);
}

#[test]
fn system_ontologies_is_invisible_without_the_ontology_opt_in() {
    // ADR-0593: while the `ontology` preview feature is off the namespace
    // is invisible — the connect refuses with the ordinary
    // unknown-database rejection, and every `USE` route fails the same
    // way (before the 42N79 admin gate and the ADR-0229 42N17 gate, so
    // a hidden namespace behaves exactly like a name that never
    // resolved). The 42N17 gate itself is unchanged and only reachable
    // with the opt-in; its pins live in tests/namespace.rs.
    crate::test_opts::assert_preview_unset();
    let db = Database::open(":memory:").unwrap();
    // `.err().expect` over `expect_err`: `Connection` is not `Debug`.
    let err = db
        .connect_namespace("system_ontologies")
        .err()
        .expect("the hidden namespace must refuse the connect");
    assert!(
        err.to_string()
            .ends_with("Graph not found: system_ontologies"),
        "{err}"
    );
    let mut conn = db.connect().unwrap();
    for statement in [
        "USE system_ontologies MATCH (n) RETURN count(n) AS c",
        "USE system_ontologies SHOW DATABASES",
    ] {
        let err = conn.query(statement).expect_err("must fail");
        assert!(
            err.to_string()
                .ends_with("Graph not found: system_ontologies"),
            "{statement}: {err}"
        );
    }
}

#[test]
fn neo4j_writes_are_invisible_to_system_sessions() {
    // The ADR-0116 §1 isolation rule, on the namespace that still holds a
    // graph: `system` sees no user data.
    let db = Database::open(":memory:").unwrap();
    let mut neo4j = db.connect().unwrap();
    neo4j.query("CREATE (:Thing {name: 'data'})").unwrap();

    let mut system = db.connect_namespace("system").unwrap();
    // The system session answers from its own graph (dispatch routes
    // `Namespace::System` to the empty `system` slot), so the node
    // committed to `neo4j` is invisible there: the labels and property
    // keys the system session SEES must not include the committed node's
    // `Thing` label or its `name` property key — asserted as the literal
    // row lists those procedure calls return (data clauses are 42N17 on
    // `system`, ADR-0116 §4, so the visibility checks go through the
    // procedure calls the namespace allows).
    let result = system
        .query("CALL db.labels() YIELD label RETURN label")
        .unwrap();
    assert_eq!(result.header.namespace, Namespace::System);
    assert!(
        result.rows.is_empty(),
        "the system session must see no labels; the neo4j-committed `Thing` label leaked: {rows:?}",
        rows = result.rows
    );
    let result = system
        .query("CALL db.propertyKeys() YIELD propertyKey RETURN propertyKey")
        .unwrap();
    assert!(
        result.rows.is_empty(),
        "the system session must see no property keys; the neo4j-committed `name` key leaked: {rows:?}",
        rows = result.rows
    );
    let result = neo4j.query("MATCH (n) RETURN count(n) AS c").unwrap();
    assert_eq!(result.rows[0][0], Value::Int(1));
}

#[test]
fn registered_procedures_are_shared_with_the_system_graph() {
    let db = Database::open(":memory:").unwrap();
    db.register_procedure(relate_executor::Procedure {
        name: "custom.ping".to_string(),
        arg_names: vec![],
        arg_types: vec![],
        yield_names: vec!["ok".to_string()],
        rows: vec![],
    })
    .unwrap();
    let shared = db.shared();
    for graph in [&shared.graph, &shared.system] {
        assert!(graph.has_procedure("custom.ping"));
    }
}
