//! The statement deadline on the Query API routes (ADR-0692): the server
//! default, the request's `maxExecutionTime` in both directions, the
//! operator ceiling, and the transaction routes' per-statement budget.

use super::tests::*;
use super::*;
use crate::v2_envelope::tests::body_json;
use relate::TimeoutPolicy;
use std::time::Duration;

const LONG: &str = "UNWIND range(1, 2000000000) AS x RETURN count(x) AS n";

fn run(policy: TimeoutPolicy, txs: &TxRegistry, request: &Request) -> Reply {
    let db = db();
    handle_pending(
        &db,
        request,
        &relate_auth::AuthConfig::None,
        None,
        txs,
        CredentialGate::Accepted,
        None,
        policy,
    )
    .materialize()
}

fn statement(body: &str) -> Request {
    post("neo4j", body)
}

fn message(reply: &Reply) -> String {
    body_json(reply)["errors"][0]["message"]
        .as_str()
        .expect("the error body carries a message")
        .to_string()
}

fn policy(default_ms: u64, ceiling_ms: Option<u64>) -> TimeoutPolicy {
    TimeoutPolicy {
        default: Some(Duration::from_millis(default_ms)),
        ceiling: ceiling_ms.map(Duration::from_millis),
    }
}

#[test]
fn the_server_default_stops_a_long_statement_with_a_408() {
    let reply = run(
        policy(200, None),
        &txs(),
        &statement(&format!(r#"{{"statement":"{LONG}"}}"#)),
    );
    assert_eq!(reply.status, 408);
    assert!(message(&reply).contains("query timed out after 200ms"));
}

#[test]
fn a_request_max_execution_time_shortens_the_default() {
    let reply = run(
        policy(60_000, None),
        &txs(),
        &statement(&format!(
            r#"{{"statement":"{LONG}","maxExecutionTime":0.2}}"#
        )),
    );
    assert_eq!(reply.status, 408);
    assert!(message(&reply).contains("query timed out after 200ms"));
}

#[test]
fn a_request_max_execution_time_extends_the_default() {
    // A 1 ms default would stop this statement; the request lifts it.
    let reply = run(
        policy(1, None),
        &txs(),
        &statement(
            r#"{"statement":"UNWIND range(1, 200000) AS x RETURN count(x) AS n","maxExecutionTime":60}"#,
        ),
    );
    assert_eq!(reply.status, 200, "{}", message(&reply));
}

#[test]
fn the_ceiling_bounds_a_request_for_no_timeout() {
    let reply = run(
        policy(60_000, Some(200)),
        &txs(),
        &statement(&format!(r#"{{"statement":"{LONG}","maxExecutionTime":0}}"#)),
    );
    assert_eq!(reply.status, 408);
    assert!(message(&reply).contains("query timed out after 200ms"));
}

#[test]
fn a_negative_or_non_numeric_max_execution_time_is_a_400() {
    let negative = run(
        TimeoutPolicy::default(),
        &txs(),
        &statement(r#"{"statement":"RETURN 1","maxExecutionTime":-1}"#),
    );
    assert_eq!(negative.status, 400);
    assert!(message(&negative).contains("maxExecutionTime"));
    let text = run(
        TimeoutPolicy::default(),
        &txs(),
        &statement(r#"{"statement":"RETURN 1","maxExecutionTime":"soon"}"#),
    );
    assert_eq!(text.status, 400);
}

#[test]
fn a_begin_max_execution_time_bounds_each_statement_of_the_transaction() {
    let db = db();
    let txs = txs();
    let policy = policy(60_000, None);
    let begin = handle_pending(
        &db,
        &tx_post("/db/neo4j/query/v2/tx", r#"{"maxExecutionTime":0.2}"#),
        &relate_auth::AuthConfig::None,
        None,
        &txs,
        CredentialGate::Accepted,
        None,
        policy,
    )
    .materialize();
    assert_eq!(begin.status, 201);
    let id = tx_id(&begin);
    let reply = handle_pending(
        &db,
        &tx_post(
            &format!("/db/neo4j/query/v2/tx/{id}"),
            &format!(r#"{{"statement":"{LONG}"}}"#),
        ),
        &relate_auth::AuthConfig::None,
        None,
        &txs,
        CredentialGate::Accepted,
        None,
        policy,
    )
    .materialize();
    assert_eq!(reply.status, 408);
    assert!(message(&reply).contains("query timed out after 200ms"));
}
