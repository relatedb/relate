//! Namespace routing and result metadata (resolved namespace, query type).

use super::*;

#[test]
fn namespace_defaults_to_neo4j() {
    with_py(|py| {
        let db = new_database(":memory:");
        assert_eq!(db.namespace(py).unwrap(), "neo4j");
    });
}

#[test]
fn connect_namespace_is_case_insensitive() {
    with_py(|py| {
        let db = new_database_ns(":memory:", "SYSTEM");
        assert_eq!(db.namespace(py).unwrap(), "system");
    });
}

#[test]
fn connect_unknown_namespace_is_database_not_found() {
    with_py(|py| {
        let err = match try_new_database_ns(":memory:", "nosuch") {
            Ok(_) => panic!("expected connecting with an unknown namespace to fail"),
            Err(e) => e,
        };
        let kind = err_attr(&err, py, "kind");
        assert_eq!(kind, "DatabaseNotFound");
        assert!(
            err.to_string().ends_with("Graph not found: nosuch"),
            "unexpected message: {err}"
        );
    });
}

#[test]
fn set_namespace_switches_between_transactions_only() {
    with_py(|py| {
        let db = new_database(":memory:");
        db.set_namespace(py, "SYSTEM").unwrap();
        assert_eq!(db.namespace(py).unwrap(), "system");
        db.set_namespace(py, "neo4j").unwrap();
        assert_eq!(db.namespace(py).unwrap(), "neo4j");

        db.begin_transaction(py).unwrap();
        let err = db.set_namespace(py, "system").unwrap_err();
        assert!(
            err.to_string()
                .contains("Cannot change the namespace while a transaction is open."),
            "unexpected message: {err}"
        );
        db.rollback_transaction(py).unwrap();
        db.set_namespace(py, "system").unwrap();
        assert_eq!(db.namespace(py).unwrap(), "system");

        let err = db.set_namespace(py, "nosuch").unwrap_err();
        let kind = err_attr(&err, py, "kind");
        assert_eq!(kind, "DatabaseNotFound");
        // A failed switch leaves the namespace unchanged.
        assert_eq!(db.namespace(py).unwrap(), "system");
    });
}

#[test]
fn reset_keeps_namespace() {
    with_py(|py| {
        let mut db = new_database(":memory:");
        db.set_namespace(py, "system").unwrap();
        db.reset(py).unwrap();
        assert_eq!(db.namespace(py).unwrap(), "system");
    });
}

#[test]
fn explain_create_reports_write_type_without_executing() {
    with_py(|py| {
        let db = new_database(":memory:");
        // EXPLAIN reports the wrapped statement's plan-time query type
        // (ADR-0162) without running it.
        let explained = db.execute(py, "EXPLAIN CREATE (n)", None).unwrap();
        assert_eq!(explained.query_type, "w");
        assert_eq!(explained.namespace, "neo4j");
        // The CREATE never executed: the graph is still empty.
        let after = db.execute(py, "MATCH (n) RETURN n", None).unwrap();
        assert!(after.data.is_empty());
        assert_eq!(after.query_type, "r");
        assert_eq!(db.node_count(py).unwrap(), 0);
    });
}

#[test]
fn use_routed_statement_reports_resolved_namespace() {
    with_py(|py| {
        let db = new_database(":memory:");
        // A USE-carrying statement resolves to its USE target (ADR-0117
        // §7)…
        let routed = db
            .execute(py, "USE system SHOW DATABASES YIELD name RETURN name", None)
            .unwrap();
        assert_eq!(routed.namespace, "system");
        // …while the session namespace is unchanged: a following unrouted
        // statement still resolves to neo4j.
        let unrouted = db.execute(py, "RETURN 1 AS n", None).unwrap();
        assert_eq!(unrouted.namespace, "neo4j");
        assert_eq!(db.namespace(py).unwrap(), "neo4j");
    });
}
