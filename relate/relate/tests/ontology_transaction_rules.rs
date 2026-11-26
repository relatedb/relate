//! The `db.ontology.*` transaction rules (ADR-0121 §3 as amended by
//! ADR-0667), end to end through the facade: `set` and `drop` run in
//! explicit transactions, the no-mixed-writes statement rule (42N03), the
//! transaction-mixing rule (schema vs data updates, either clause order,
//! implicit and explicit transactions alike), `get`'s read-own-writes of
//! the pending document, and the begin-time capture a transaction keeps
//! across another connection's committed `set`. The procedure contracts
//! live in `ontology_procedures.rs`.

mod common;

use std::collections::BTreeMap;

use relate::{Database, Value};

use common::err_text;
use common::ontology::{canonical, get_doc, set_doc, DOC_JSON, MOVIES};

const GET: &str = "CALL db.ontology.get('json') YIELD ontology RETURN ontology";

/// An ontology-previewed in-memory store and a connection to it.
fn open() -> (Database, relate::Connection) {
    common::ontology::preview_ontology();
    let db = Database::open(":memory:").unwrap();
    let conn = db.connect().unwrap();
    (db, conn)
}

/// [`open`], with `DOC_JSON` already committed as the stored document.
fn open_with_document() -> (Database, relate::Connection) {
    let (db, mut conn) = open();
    conn.begin().unwrap();
    set_doc(&mut conn, DOC_JSON).unwrap();
    conn.commit().unwrap();
    (db, conn)
}

/// `set` and `drop` run in explicit transactions (ADR-0121 §3); a rolled-back `drop` keeps the committed document.
#[test]
fn set_and_drop_run_in_explicit_transactions() {
    let (_db, mut conn) = open();

    // set and drop run in explicit transactions.
    conn.begin().unwrap();
    set_doc(&mut conn, DOC_JSON).unwrap();
    conn.commit().unwrap();
    assert_eq!(
        get_doc(&mut conn, GET),
        Value::String(canonical(DOC_JSON).into())
    );
    conn.begin().unwrap();
    conn.query("CALL db.ontology.drop()").unwrap();
    conn.rollback().unwrap();
    assert_eq!(
        get_doc(&mut conn, GET),
        Value::String(canonical(DOC_JSON).into()),
        "the rolled-back drop keeps the committed document"
    );
}

/// A statement calling `set` or `drop` may not write anything else (42N03, ADR-0121 §3).
#[test]
fn a_set_or_drop_statement_may_not_write_anything_else() {
    let (_db, mut conn) = open_with_document();

    // A statement calling `set` or `drop` may not write anything else
    // (42N03, ADR-0121 §3) — in one auto-commit statement.
    let err = err_text(conn.query_with_params(
        "CREATE (n) CALL db.ontology.set($ontology)",
        BTreeMap::from([("ontology".to_string(), Value::String(DOC_JSON.into()))]),
    ));
    assert!(
        err.contains("Writing to more than one database per transaction is not allowed."),
        "{err}"
    );
    let err = err_text(conn.query("CREATE (n) CALL db.ontology.drop()"));
    assert!(
        err.contains("Writing to more than one database per transaction is not allowed."),
        "{err}"
    );
}

/// A statement may not both call an ontology write and perform data updates, either clause order, implicit and explicit (ADR-0667).
#[test]
fn a_statement_may_not_mix_an_ontology_write_with_data_updates() {
    let (_db, mut conn) = open_with_document();

    // A statement may not both call an ontology write and perform data
    // updates (ADR-0667) — the mixing rule within one statement, either
    // clause order, in implicit and explicit transactions alike. (The
    // apply call names its results with YIELD in both orders: a
    // row-producing `CALL` inside a query must, 42N25 — the void
    // `set`/`drop` calls above need none.)
    for cypher in [
        "CREATE (n:Thing) CALL db.ontology.apply() YIELD ontology RETURN ontology",
        "CALL db.ontology.apply() YIELD ontology CREATE (n:Thing)",
    ] {
        let err = err_text(conn.query(cypher));
        assert!(
            err.contains(
                "Cannot perform schema updates in a transaction that has performed data updates"
            ),
            "{cypher} (implicit): {err}"
        );
        conn.begin().unwrap();
        let err = err_text(conn.query(cypher));
        assert!(
            err.contains(
                "Cannot perform schema updates in a transaction that has performed data updates"
            ),
            "{cypher} (explicit): {err}"
        );
        conn.rollback().unwrap();
    }
    // The refused statements wrote nothing.
    assert_eq!(
        common::scalar_one(&mut conn, "MATCH (n) RETURN count(n) AS c"),
        Value::Int(0)
    );
}

/// A `set` mixes with a schema command in one transaction, in both orders (ADR-0667).
#[test]
fn a_set_mixes_with_a_schema_command_in_both_orders() {
    let (_db, mut conn) = open_with_document();

    // A set mixes with a schema command in one transaction, in both
    // orders — both land at commit.
    conn.begin().unwrap();
    conn.query("CREATE CONSTRAINT thing_name FOR (t:Thing) REQUIRE t.name IS NOT NULL")
        .unwrap();
    set_doc(&mut conn, MOVIES).unwrap();
    conn.commit().unwrap();
    assert_eq!(
        get_doc(&mut conn, GET),
        Value::String(canonical(MOVIES).into())
    );
    conn.begin().unwrap();
    set_doc(&mut conn, DOC_JSON).unwrap();
    conn.query("CREATE CONSTRAINT thing_id FOR (t:Thing) REQUIRE t.id IS NOT NULL")
        .unwrap();
    conn.commit().unwrap();
    assert_eq!(
        get_doc(&mut conn, GET),
        Value::String(canonical(DOC_JSON).into())
    );
    let result = conn
        .query("SHOW CONSTRAINTS YIELD name RETURN collect(name) AS names")
        .unwrap();
    let names = common::ontology::string_list(&result.rows[0][0]);
    assert!(names.contains(&"thing_name".to_string()));
    assert!(names.contains(&"thing_id".to_string()));
}

/// Data after a schema update (the `set`) fails with the exact text and nothing lands (ADR-0667).
#[test]
fn data_after_a_schema_update_is_refused() {
    let (_db, mut conn) = open_with_document();

    // Data after a schema update (the set): the exact refusal, and
    // nothing landed. A schema command between the two records no class
    // of its own — the history the data update is refused against is
    // still the set's.
    conn.begin().unwrap();
    set_doc(&mut conn, MOVIES).unwrap();
    conn.query("CREATE CONSTRAINT thing_extra FOR (t:Thing) REQUIRE t.extra IS NOT NULL")
        .unwrap();
    let err = err_text(conn.query("CREATE (t:Thing {name: 'x'})"));
    assert!(
        err.contains(
            "Cannot perform data updates in a transaction that has performed schema updates"
        ),
        "{err}"
    );
    conn.rollback().unwrap();
    assert_eq!(
        common::scalar_one(&mut conn, "MATCH (t:Thing) RETURN count(t) AS c"),
        Value::Int(0)
    );
}

/// An ontology write (`set`, `apply`) after data updates fails with the exact text (ADR-0667).
#[test]
fn an_ontology_write_after_data_updates_is_refused() {
    let (_db, mut conn) = open_with_document();

    // An ontology write after data updates: the exact refusal, for set
    // and apply alike (apply needs a stored document, so set one first).
    conn.begin().unwrap();
    conn.query("CREATE (t:Thing {name: 'x', id: 'x'})").unwrap();
    let err = err_text(set_doc(&mut conn, MOVIES));
    assert!(
        err.contains(
            "Cannot perform schema updates in a transaction that has performed data updates"
        ),
        "{err}"
    );
    conn.rollback().unwrap();
    conn.begin().unwrap();
    conn.query("CREATE (t:Thing {name: 'x', id: 'x'})").unwrap();
    let err = err_text(conn.query("CALL db.ontology.apply()"));
    assert!(
        err.contains(
            "Cannot perform schema updates in a transaction that has performed data updates"
        ),
        "{err}"
    );
    conn.rollback().unwrap();
    assert_eq!(
        common::scalar_one(&mut conn, "MATCH (t:Thing) RETURN count(t) AS c"),
        Value::Int(0)
    );
}

/// `get` reads the transaction's pending document (read-own-writes); a rollback returns to the committed one (ADR-0667).
#[test]
fn get_reads_the_transactions_pending_document() {
    let (_db, mut conn) = open_with_document();

    // get reads the transaction's pending document (read-own-writes):
    // the pending set, then the pending drop, and the rollback returns
    // the read to the committed document.
    conn.begin().unwrap();
    set_doc(&mut conn, MOVIES).unwrap();
    assert_eq!(
        get_doc(&mut conn, GET),
        Value::String(canonical(MOVIES).into()),
        "the pending set is what the same transaction reads"
    );
    conn.query("CALL db.ontology.drop()").unwrap();
    assert_eq!(
        get_doc(&mut conn, GET),
        Value::Null,
        "the pending drop is what the same transaction reads"
    );
    conn.rollback().unwrap();
    assert_eq!(
        get_doc(&mut conn, GET),
        Value::String(canonical(DOC_JSON).into()),
        "the rollback keeps the committed document"
    );
}

/// A transaction that began before another connection's committed `set` keeps its begin-time view of the document.
#[test]
fn a_transaction_keeps_its_begin_time_document_view() {
    let (db, mut conn) = open_with_document();

    // A transaction that began before another connection's committed set
    // keeps its begin-time view of the committed document; after its own
    // commit, a fresh read sees the newly committed one.
    conn.begin().unwrap();
    let mut writer = db.connect().unwrap();
    set_doc(&mut writer, MOVIES).unwrap();
    assert_eq!(
        get_doc(&mut conn, GET),
        Value::String(canonical(DOC_JSON).into()),
        "the open transaction keeps its begin-time view of the document"
    );
    conn.commit().unwrap();
    assert_eq!(
        get_doc(&mut conn, GET),
        Value::String(canonical(MOVIES).into())
    );
}
