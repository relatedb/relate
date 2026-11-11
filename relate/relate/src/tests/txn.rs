use crate::{Database, Error, Value};

#[test]
fn explicit_txn_read_your_writes_and_isolation() {
    let db = Database::open(":memory:").unwrap();
    let mut w = db.connect().unwrap();
    let mut r = db.connect().unwrap();
    w.begin().unwrap();
    w.query("CREATE (n:Person {name: 'Ada'})").unwrap();
    let seen = w.query("MATCH (n:Person) RETURN n.name AS name").unwrap();
    assert_eq!(seen.rows[0][0].as_string(), Some("Ada"));
    let other = r.query("MATCH (n:Person) RETURN n.name AS name").unwrap();
    assert!(other.rows.is_empty());
    w.commit().unwrap();
    assert!(!w.in_transaction());
    let after = r.query("MATCH (n:Person) RETURN n.name AS name").unwrap();
    assert_eq!(after.rows[0][0].as_string(), Some("Ada"));
}

#[test]
fn explicit_txn_rollback_discards_writes() {
    let db = Database::open(":memory:").unwrap();
    let mut c = db.connect().unwrap();
    c.begin().unwrap();
    c.query("CREATE (n:Person {name: 'Ada'})").unwrap();
    c.rollback().unwrap();
    assert!(!c.in_transaction());
    let r = c.query("MATCH (n:Person) RETURN count(n) AS n").unwrap();
    assert_eq!(r.rows[0][0], Value::Int(0));
}

#[test]
fn explicit_txn_drop_is_rollback() {
    let db = Database::open(":memory:").unwrap();
    {
        let mut c = db.connect().unwrap();
        c.begin().unwrap();
        c.query("CREATE (n:Person {name: 'Ada'})").unwrap();
    }
    let mut c = db.connect().unwrap();
    let r = c.query("MATCH (n:Person) RETURN count(n) AS n").unwrap();
    assert_eq!(r.rows[0][0], Value::Int(0));
}

#[test]
fn explicit_txn_uncommitted_not_durable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_str().unwrap();
    {
        let db = Database::open(path).unwrap();
        let mut c = db.connect().unwrap();
        c.begin().unwrap();
        c.query("CREATE (n:Person {name: 'Ada'})").unwrap();
        db.leak_without_checkpoint();
    }
    let db = Database::open(path).unwrap();
    let mut c = db.connect().unwrap();
    let r = c.query("MATCH (n:Person) RETURN count(n) AS n").unwrap();
    assert_eq!(r.rows[0][0], Value::Int(0));
}

/// ADR-0570: a statement carrying `CALL { … } IN TRANSACTIONS` owns its
/// batch commits, so an open explicit transaction refuses it — live Neo4j
/// 2026.10.1 raises GQLSTATUS 25N17 (`TransactionStartFailed`). The same
/// statement runs in an implicit transaction.
#[test]
fn call_in_transactions_is_refused_inside_an_explicit_transaction() {
    let db = Database::open(":memory:").unwrap();
    let mut c = db.connect().unwrap();
    let statement = "UNWIND range(1, 3) AS i WITH [toString(i)] AS line \
                     CALL { CREATE (n) RETURN n } IN TRANSACTIONS RETURN n";
    c.query(statement).unwrap();
    c.begin().unwrap();
    let err = c.query(statement).unwrap_err();
    assert!(
        err.to_string()
            .contains("can only be executed in an implicit transaction"),
        "{err}"
    );
    // The refused statement leaves the transaction open and rollback-able.
    c.rollback().unwrap();
}

#[test]
fn explicit_txn_nested_begin_and_commit_without_begin() {
    let db = Database::open(":memory:").unwrap();
    let mut c = db.connect().unwrap();
    assert_eq!(
        c.commit().unwrap_err().to_string(),
        "semantic error: no transaction"
    );
    assert_eq!(
        c.rollback().unwrap_err().to_string(),
        "semantic error: no transaction"
    );
    c.begin().unwrap();
    assert_eq!(
        c.begin().unwrap_err().to_string(),
        "semantic error: already in a transaction"
    );
    c.rollback().unwrap();
}

#[test]
fn explicit_txn_failed_requires_rollback() {
    let db = Database::open(":memory:").unwrap();
    let mut c = db.connect().unwrap();
    c.begin().unwrap();
    c.query("CREATE (n:Person {name: 'Ada'})").unwrap();
    assert!(c.query("RETURN nosuchvar").is_err());
    assert!(c.in_transaction());
    match c.query("MATCH (n:Person) RETURN n") {
        Ok(_) => panic!("expected failed transaction"),
        Err(e) => assert_eq!(
            e.to_string(),
            "semantic error: transaction failed; rollback required"
        ),
    }
    assert_eq!(
        c.commit().unwrap_err().to_string(),
        "semantic error: transaction failed; rollback required"
    );
    c.rollback().unwrap();
    let r = c.query("MATCH (n:Person) RETURN count(n) AS n").unwrap();
    assert_eq!(r.rows[0][0], Value::Int(0));
}

#[test]
fn explicit_txn_read_only_commit_is_noop() {
    let db = Database::open(":memory:").unwrap();
    let mut c = db.connect().unwrap();
    c.query("CREATE (n:Person {name: 'Ada'})").unwrap();
    c.begin().unwrap();
    let r = c.query("MATCH (n:Person) RETURN n.name AS name").unwrap();
    assert_eq!(r.rows[0][0].as_string(), Some("Ada"));
    c.commit().unwrap();
    assert!(!c.in_transaction());
}

#[test]
fn explicit_txn_conflict_at_commit() {
    let db = Database::open(":memory:").unwrap();
    {
        let mut c = db.connect().unwrap();
        c.query("CREATE (n:Person {name: 'Ada', n: 0})").unwrap();
    }
    let mut a = db.connect().unwrap();
    let mut b = db.connect().unwrap();
    a.begin().unwrap();
    b.begin().unwrap();
    a.query("MATCH (n:Person {name: 'Ada'}) SET n.n = 1")
        .unwrap();
    b.query("MATCH (n:Person {name: 'Ada'}) SET n.n = 2")
        .unwrap();
    a.commit().unwrap();
    assert!(matches!(b.commit(), Err(Error::Conflict)));
    assert!(!b.in_transaction());
    let mut c = db.connect().unwrap();
    let r = c.query("MATCH (n:Person) RETURN n.n AS n").unwrap();
    assert_eq!(r.rows[0][0], Value::Int(1));
}
#[test]
fn named_index_in_explicit_txn() {
    let db = Database::open(":memory:").unwrap();
    let mut c = db.connect().unwrap();
    c.begin().unwrap();
    c.query("CREATE INDEX person_name FOR (n:Person) ON (n.name)")
        .unwrap();
    let shown = c
        .query("SHOW INDEXES YIELD name WHERE name = 'person_name' RETURN name")
        .unwrap();
    assert_eq!(shown.rows[0][0].as_string(), Some("person_name"));
    c.commit().unwrap();
    let mut other = db.connect().unwrap();
    assert_eq!(
        other
            .query("SHOW INDEXES YIELD name WHERE name = 'person_name' RETURN name")
            .unwrap()
            .rows[0][0]
            .as_string(),
        Some("person_name")
    );
}
