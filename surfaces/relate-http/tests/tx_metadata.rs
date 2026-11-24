//! ADR-0610's `tx_metadata` request field on the Query API v2 routes, end
//! to end over a real loopback server behind the `value-provenance`
//! opt-in: the auto-commit statement route and the managed-transaction
//! routes seed the field's map onto the connection, the commit's
//! transaction record serializes it (merged in call order, later keys
//! winning), and a rollback discards it — read back through the engine's
//! retained-record API ([`Database::transaction_records`]).
//!
//! This file is its own test binary: `RELATE_PREVIEW` is process-global
//! and read once per `Database::open` (ADR-0371), so the opt-in is set
//! before the first open and every test here runs behind the gate. The
//! closed side — the authored preview error a request carrying the field
//! answers without the opt-in — lives in the unit suite
//! (`src/query_api_tests.rs`).

use std::net::TcpListener;
use std::sync::Arc;

use relate::{Database, Value};
use relate_auth::AuthConfig;
use relate_http::{serve, HttpConfig};
use relate_test_support::loopback::{exchange, post, request, LOOPBACK_HOST};
use serde_json::Value as Json;

/// Opt this process into `value-provenance` before the first open — the
/// Once-shared form, so sibling threads opening stores race no in-flight
/// `set_var`.
fn gated() -> Arc<Database> {
    relate_test_support::preview::set_preview_once("value-provenance");
    Arc::new(Database::open(":memory:").unwrap())
}

/// A server on an ephemeral port over `db`. The accept loop diverges by
/// design; the thread lives until the test process exits.
fn start(db: Arc<Database>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        serve(db, listener, HttpConfig::new(AuthConfig::None)).expect("serve starts")
    });
    port
}

/// One POST exchange: the status and the parsed JSON body.
fn call(port: u16, path: &str, body: &str) -> (u16, Json) {
    let (status, body) = exchange(port, post(path, body, None).as_bytes());
    (status, serde_json::from_str(&body).unwrap())
}

/// The transaction id a BEGIN reply carries.
fn tx_id(body: &Json) -> String {
    body["transaction"]["id"]
        .as_str()
        .expect("a begin reply carries the transaction id")
        .to_string()
}

/// The store's retained transaction records' metadata maps, in commit
/// order — the audit side of ADR-0610 the seeds above feed.
fn metadata_records(db: &Database) -> Vec<Option<std::collections::BTreeMap<String, Value>>> {
    db.transaction_records()
        .unwrap()
        .into_iter()
        .map(|record| record.metadata)
        .collect()
}

/// The `by: app, run: 7` map the seeds below share, spelled once.
fn by_app() -> std::collections::BTreeMap<String, Value> {
    std::collections::BTreeMap::from([
        ("by".to_string(), Value::String("app".into())),
        ("run".to_string(), Value::Int(7)),
    ])
}

/// The auto-commit route: a write carrying `tx_metadata` commits with the
/// map in its transaction record — the request's own statement run is the
/// transaction boundary that serializes it.
#[test]
fn auto_commit_tx_metadata_reaches_the_commit() {
    let db = gated();
    let port = start(db.clone());
    let (status, body) = call(
        port,
        "/db/neo4j/query/v2",
        r#"{"statement": "CREATE (:City {name: 'Oslo'})",
            "tx_metadata": {"by": "app", "run": 7}}"#,
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["counters"]["nodes_created"], 1);
    assert_eq!(metadata_records(&db), vec![Some(by_app())]);
}

/// The managed-transaction routes: a BEGIN, a run and a COMMIT may each
/// carry `tx_metadata`; the seeds merge in call order (later keys win) and
/// the one commit of the explicit transaction serializes the merged map.
#[test]
fn managed_tx_metadata_merges_in_call_order_into_the_commit() {
    let db = gated();
    let port = start(db.clone());
    let (status, body) = call(
        port,
        "/db/neo4j/query/v2/tx",
        r#"{"statement": "CREATE (:A)", "tx_metadata": {"by": "begin", "keep": true}}"#,
    );
    assert_eq!(status, 201, "{body}");
    let id = tx_id(&body);

    let (status, body) = call(
        port,
        &format!("/db/neo4j/query/v2/tx/{id}"),
        r#"{"statement": "CREATE (:B)", "tx_metadata": {"by": "run"}}"#,
    );
    assert_eq!(status, 200, "{body}");

    // A statement-less COMMIT body still seeds: the final call's map
    // reaches the commit that is about to happen.
    let (status, body) = call(
        port,
        &format!("/db/neo4j/query/v2/tx/{id}/commit"),
        r#"{"tx_metadata": {"run": 7}}"#,
    );
    assert_eq!(status, 200, "{body}");
    // The three seeds merged in call order, later keys winning: `keep`
    // from the begin, `by` overwritten by the run, `run` added by the
    // commit — one record for the one commit of the explicit transaction.
    let merged = std::collections::BTreeMap::from([
        ("by".to_string(), Value::String("run".into())),
        ("keep".to_string(), Value::Bool(true)),
        ("run".to_string(), Value::Int(7)),
    ]);
    assert_eq!(metadata_records(&db), vec![Some(merged)]);
}

/// A rolled-back transaction commits nothing — the seed is discarded with
/// the transaction, never retained as a record of its own.
#[test]
fn a_rolled_back_tx_discards_its_metadata() {
    let db = gated();
    let port = start(db.clone());
    let (status, body) = call(
        port,
        "/db/neo4j/query/v2/tx",
        r#"{"statement": "CREATE (:A)", "tx_metadata": {"by": "app"}}"#,
    );
    assert_eq!(status, 201, "{body}");
    let id = tx_id(&body);

    let (status, raw) = exchange(
        port,
        request(
            "DELETE",
            &format!("/db/neo4j/query/v2/tx/{id}"),
            LOOPBACK_HOST,
            &[],
        )
        .as_bytes(),
    );
    assert_eq!(status, 202, "{raw}");
    assert_eq!(metadata_records(&db), Vec::new());
}

/// The field name is the ADR's own spelling: the camelCase form is an
/// unknown member (ignored, no metadata recorded), so the snake_case name
/// is the one and only way the map rides the wire.
#[test]
fn the_camel_case_spelling_is_not_the_field() {
    let db = gated();
    let port = start(db.clone());
    let (status, body) = call(
        port,
        "/db/neo4j/query/v2",
        r#"{"statement": "CREATE (:City {name: 'Oslo'})", "txMetadata": {"by": "app"}}"#,
    );
    assert_eq!(status, 200, "{body}");
    // One record — changed keys retained — but its metadata is empty.
    assert_eq!(metadata_records(&db), vec![None]);
}

/// The typed request dialect folds the metadata map like its parameters:
/// a `v1.2` request may carry typed envelope values inside `tx_metadata`.
#[test]
fn the_typed_dialect_decodes_the_metadata_values() {
    let db = gated();
    let port = start(db.clone());
    let body = r#"{"statement": "CREATE (:City {name: 'Oslo'})",
        "tx_metadata": {"by": {"$type": "String", "_value": "app"},
                        "run": {"$type": "Integer", "_value": "7"}}}"#;
    let (status, body) = exchange(
        port,
        relate_test_support::loopback::post_with_content_type(
            "/db/neo4j/query/v2",
            "application/vnd.neo4j.query.v1.2",
            body,
            &[],
        )
        .as_bytes(),
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(metadata_records(&db), vec![Some(by_app())]);
    // The typed form stays out of the plain dialect: the same envelope
    // object is a user map under application/json.
    let (status, body) = call(
        port,
        "/db/neo4j/query/v2",
        r#"{"statement": "CREATE (:City {name: 'Bergen'})",
            "tx_metadata": {"by": {"$type": "String", "_value": "app"}}}"#,
    );
    assert_eq!(status, 200, "{body}");
    let records = metadata_records(&db);
    assert_eq!(records.len(), 2);
    // The plain dialect keeps the envelope-shaped object as a user map —
    // both of its entries, byte-for-byte what a real server delivers.
    assert_eq!(
        records[1].as_ref().unwrap()["by"],
        Value::Map(
            std::collections::BTreeMap::from([
                ("$type".to_string(), Value::String("String".into())),
                ("_value".to_string(), Value::String("app".into())),
            ])
            .into()
        )
    );
}
