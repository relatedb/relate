//! Safe mode (LABS-680, ADR-0477): the opt-in policy refuses agent-typical
//! dangerous statement shapes. Everything is off by default.

use relate::{Database, OpenOptions};

fn safe_db() -> Database {
    Database::open_with(
        ":memory:",
        OpenOptions {
            safe_mode: true,
            ..Default::default()
        },
    )
    .expect("safe-mode memory open")
}

fn query_error(db: &Database, query: &str) -> String {
    let mut conn = db.connect().unwrap();
    conn.execute(query).unwrap_err().to_string()
}

#[test]
fn safe_mode_is_off_by_default_and_reflects_the_open_option() {
    let db = Database::open(":memory:").unwrap();
    assert!(!db.is_safe_mode());
    assert!(safe_db().is_safe_mode());
}

#[test]
fn a_new_named_node_without_a_label_is_refused_in_safe_mode() {
    let db = safe_db();
    let error = query_error(&db, "CREATE (n)");
    assert!(
        error.starts_with("syntax error: SafeModeViolation: "),
        "{error}"
    );
    assert!(error.contains("without a label"), "{error}");
    assert!(error.contains("CREATE (n:Person)"), "{error}");
    assert!(
        error.contains("This is a policy, not a database limit"),
        "{error}"
    );
    assert!(error.contains("reopen without --safe"), "{error}");

    // MERGE is the same shape.
    let error = query_error(&db, "MERGE (n) RETURN 1");
    assert!(error.contains("SafeModeViolation"), "{error}");

    // The remedy passes, and writes actually land.
    let mut conn = db.connect().unwrap();
    conn.execute("CREATE (n:Person {id: 1})").unwrap();
    conn.execute("MERGE (n:Person {id: 2})").unwrap();
    let result = conn
        .execute("MATCH (n:Person) RETURN count(n) AS n")
        .unwrap();
    assert_eq!(result.rows[0][0].as_int(), Some(2));
}

#[test]
fn relationship_only_anonymous_endpoints_are_exempt() {
    let db = safe_db();
    let mut conn = db.connect().unwrap();
    // The TCK's label-less relationship-only form.
    conn.execute("CREATE ()-[r:X]->()").unwrap();
    conn.execute("MATCH (a:Person) CREATE (a)-[:KNOWS]->()")
        .unwrap();
    let result = conn
        .execute("MATCH ()-[r:X]->() RETURN count(r) AS n")
        .unwrap();
    assert_eq!(result.rows[0][0].as_int(), Some(1));
}

#[test]
fn an_unbounded_read_is_refused_and_bounded_forms_pass() {
    let db = safe_db();
    let mut conn = db.connect().unwrap();
    conn.execute("CREATE (:Person {id: 1})").unwrap();

    for query in ["MATCH (n) RETURN n", "MATCH (n) RETURN collect(n)"] {
        let error = query_error(&db, query);
        assert!(error.contains("SafeModeViolation"), "{query}: {error}");
        assert!(error.contains("unbounded read"), "{query}: {error}");
        assert!(error.contains("LIMIT"), "{query}: {error}");
    }
    // collect() is explicitly named as not bounding the read.
    let error = query_error(&db, "MATCH (n) RETURN collect(n)");
    assert!(error.contains("collect()"), "{error}");

    for query in [
        "MATCH (n) RETURN n LIMIT 10",
        "MATCH (n) RETURN count(n) AS n",
        "MATCH (n) SET n.seen = true",
        "MATCH (n) DELETE n",
        "RETURN 1",
        "SHOW INDEXES",
    ] {
        conn.execute(query)
            .unwrap_or_else(|e| panic!("{query} must be admitted: {e}"));
    }
}

/// A `WITH` drops the old binding, so a later `CREATE (n)` is a new
/// label-less node and must be refused (the additive-fold bypass).
#[test]
fn a_label_less_node_after_with_dropped_the_binding_is_refused() {
    let db = safe_db();
    for query in [
        "MATCH (n) WITH 1 AS x CREATE (n)",
        "MATCH (n) WITH n AS m CREATE (n)",
    ] {
        let error = query_error(&db, query);
        assert!(error.contains("SafeModeViolation"), "{query}: {error}");
        assert!(error.contains("without a label"), "{query}: {error}");
    }
}

/// An `OPTIONAL MATCH` binding may be null and does not prove the variable
/// names an existing node.
#[test]
fn a_label_less_node_over_an_optional_match_binding_is_refused() {
    let error = query_error(&safe_db(), "OPTIONAL MATCH (n) CREATE (n)");
    assert!(error.contains("SafeModeViolation"), "{error}");
    assert!(error.contains("without a label"), "{error}");
}

/// A `collect()` item refuses even beside a reducing aggregate.
#[test]
fn collect_beside_a_reducing_aggregate_is_refused() {
    let db = safe_db();
    for query in [
        "MATCH (n) RETURN count(n), collect(n)",
        "MATCH (n) RETURN collect(n), count(*)",
    ] {
        let error = query_error(&db, query);
        assert!(error.contains("SafeModeViolation"), "{query}: {error}");
        assert!(error.contains("collect()"), "{query}: {error}");
    }
    // count-only still passes.
    let mut conn = db.connect().unwrap();
    conn.execute("MATCH (n) RETURN count(n)").unwrap();
}

#[test]
fn safe_mode_off_admits_every_guarded_shape() {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    for query in [
        "CREATE (n)",
        "MATCH (n) RETURN n",
        "MATCH (n) RETURN collect(n) AS ns",
        "MATCH (n) WITH 1 AS x CREATE (n)",
    ] {
        conn.execute(query)
            .unwrap_or_else(|e| panic!("{query} must be admitted without safe mode: {e}"));
    }
}
