//! Namespace / db-routing protocol tests (ADR-0117 §7): the RUN/BEGIN `db`
//! extra, absent/null/empty `db` fallbacks, no carry-over between requests,
//! BEGIN-time transaction namespace inheritance, `db` no-ops while a
//! transaction is open, unknown-db FAILUREs, non-canonical casing,
//! USE-target echo, and DISCARD `db`.
use super::*;
use crate::failures::REQUEST_INVALID_CODE;
use crate::packstream::Pack;
use crate::requests::Message;
use crate::testutil::{
    assert_closes, failure_map, field, read_msg, read_records, send_begin, send_commit, send_pull,
    send_rollback, send_run_at, send_struct, spawn_handler, success_map,
};
use crate::version::BoltVersion;
use std::net::TcpStream;

/// Send HELLO and return its SUCCESS metadata map.
fn hello(client: &mut TcpStream) -> Vec<(Pack, Pack)> {
    send_struct(
        client,
        Message::Hello.signature(),
        crate::testutil::hello_fields(),
    );
    success_map(read_msg(client))
}

/// The [`run_and_pull`] yields: RUN SUCCESS map, RECORD first columns, PULL
/// SUCCESS map.
type RunAndPull = (Vec<(Pack, Pack)>, Vec<Pack>, Vec<(Pack, Pack)>);

/// Send RUN `cypher` at 4.4 — `extra` as its extra map through testutil's
/// [`send_run_at`], no extra map at all when `None` — then PULL -1;
/// return the RUN SUCCESS map, the RECORD first columns, and the final
/// PULL SUCCESS map.
fn run_and_pull(
    client: &mut TcpStream,
    cypher: &str,
    extra: Option<Vec<(Pack, Pack)>>,
) -> RunAndPull {
    send_run_at(client, BoltVersion::V4_4, cypher, extra);
    let run_success = success_map(read_msg(client));
    send_pull(client, Message::Pull, vec![("n", Pack::Int(-1))]);
    let (records, summary) = read_records(client);
    let columns = records
        .into_iter()
        .map(|row| row.into_iter().next().expect("RECORD column"))
        .collect();
    (run_success, columns, success_map(summary))
}

/// The `db` of a PULL/DISCARD SUCCESS map (ADR-0117 §7).
fn success_db(m: &[(Pack, Pack)]) -> &str {
    m.iter()
        .find(|(k, _)| k.as_str() == Some("db"))
        .and_then(|(_, v)| v.as_str())
        .unwrap_or_else(|| panic!("SUCCESS must carry db: {m:?}"))
}

fn db_extra(db: Pack) -> Vec<(Pack, Pack)> {
    vec![(Pack::String("db".into()), db)]
}

/// One fresh server plus one handshook client — the fixture every namespace
/// test in this file starts from.
#[test]
fn run_extra_db_selects_the_namespace_for_that_request() {
    let mut client = crate::testutil::connected_client();

    let (_, records, pull) = run_and_pull(
        &mut client,
        "SHOW DATABASES",
        Some(db_extra(Pack::String("system_ontologies".into()))),
    );
    // ADR-0229: system_ontologies answers administration commands, not
    // graph queries.
    assert_eq!(records.len(), 3);
    assert_eq!(success_db(&pull), "system_ontologies");

    // No carry-over: the next RUN without `db` is back in `neo4j`.
    let (_, _, pull) = run_and_pull(&mut client, "MATCH (n) RETURN count(n)", None);
    assert_eq!(success_db(&pull), "neo4j");
}

#[test]
fn run_extra_db_absent_null_or_empty_means_neo4j() {
    let mut client = crate::testutil::connected_client();

    // No extra map at all.
    let (_, _, pull) = run_and_pull(&mut client, "RETURN 1 AS n", None);
    assert_eq!(success_db(&pull), "neo4j");
    // Extra map without a `db` key.
    let (_, _, pull) = run_and_pull(&mut client, "RETURN 1 AS n", Some(vec![]));
    assert_eq!(success_db(&pull), "neo4j");
    // `db: null`.
    let (_, _, pull) = run_and_pull(&mut client, "RETURN 1 AS n", Some(db_extra(Pack::Null)));
    assert_eq!(success_db(&pull), "neo4j");
    // `db: ""`.
    let (_, _, pull) = run_and_pull(
        &mut client,
        "RETURN 1 AS n",
        Some(db_extra(Pack::String(String::new()))),
    );
    assert_eq!(success_db(&pull), "neo4j");
}

#[test]
fn begin_extra_db_selects_the_transaction_namespace() {
    let mut client = crate::testutil::connected_client();

    send_begin(
        &mut client,
        db_extra(Pack::String("system_ontologies".into())),
    );
    success_map(read_msg(&mut client));

    // A RUN inside the transaction carries no `db` and runs in the
    // BEGIN-time namespace.
    let (_, records, pull) = run_and_pull(&mut client, "SHOW DATABASES", None);
    assert_eq!(records.len(), 3);
    assert_eq!(success_db(&pull), "system_ontologies");

    send_commit(&mut client);
    success_map(read_msg(&mut client));

    // After COMMIT an auto-commit RUN without `db` is back in `neo4j`.
    let (_, _, pull) = run_and_pull(&mut client, "MATCH (n) RETURN count(n)", None);
    assert_eq!(success_db(&pull), "neo4j");
}

#[test]
fn unknown_db_on_run_or_begin_fails_with_the_ce_text() {
    let mut client = crate::testutil::connected_client();

    let expected_suffix = "Graph not found: nosuch";

    // Unknown db on an auto-commit RUN.
    send_run_at(
        &mut client,
        BoltVersion::V4_4,
        "RETURN 1 AS n",
        Some(db_extra(Pack::String("nosuch".into()))),
    );
    let fields = failure_map(read_msg(&mut client));
    assert_eq!(
        field(&fields, "code"),
        "Neo.ClientError.Database.DatabaseNotFound"
    );
    assert!(
        field(&fields, "message").ends_with(expected_suffix),
        "FAILURE message must end with the CE text: {}",
        field(&fields, "message")
    );

    // RESET clears the failed state; an unknown db on BEGIN fails the same.
    send_struct(&mut client, Message::Reset.signature(), vec![]);
    success_map(read_msg(&mut client));
    send_begin(&mut client, db_extra(Pack::String("nosuch".into())));
    let fields = failure_map(read_msg(&mut client));
    assert_eq!(
        field(&fields, "code"),
        "Neo.ClientError.Database.DatabaseNotFound"
    );
    assert!(
        field(&fields, "message").ends_with(expected_suffix),
        "FAILURE message must end with the CE text: {}",
        field(&fields, "message")
    );

    // RESET, then the session is usable again in `neo4j`.
    send_struct(&mut client, Message::Reset.signature(), vec![]);
    success_map(read_msg(&mut client));
    let (_, _, pull) = run_and_pull(&mut client, "RETURN 1 AS n", None);
    assert_eq!(success_db(&pull), "neo4j");
}

#[test]
fn non_canonical_db_casing_echoes_the_canonical_namespace() {
    let mut client = crate::testutil::connected_client();

    // The driver sends non-canonical casing; the engine canonicalizes.
    send_run_at(
        &mut client,
        BoltVersion::V4_4,
        "SHOW DATABASES",
        Some(db_extra(Pack::String("SYSTEM".into()))),
    );
    success_map(read_msg(&mut client));

    // DISCARD with a pending result reports the engine's canonical namespace.
    send_pull(&mut client, Message::Discard, vec![]);
    let discard = success_map(read_msg(&mut client));
    assert_eq!(success_db(&discard), "system");

    // The ending PULL SUCCESS is canonical too — not the raw driver string.
    // (A DISCARD with no open result is a protocol breach, ADR-0314.)
    let (_, _, pull) = run_and_pull(
        &mut client,
        "SHOW DATABASES",
        Some(db_extra(Pack::String("SYSTEM".into()))),
    );
    assert_eq!(success_db(&pull), "system");
}

#[test]
fn use_target_query_echoes_the_namespace_the_query_ran_in() {
    // Regression (ADR-0117 §7): PULL/DISCARD SUCCESS must echo the
    // namespace the query RAN in — the statement's resolved USE target —
    // not the live session namespace.
    let mut client = crate::testutil::connected_client();

    // No db extra: the session namespace is `neo4j`, but the statement
    // runs in `system` via USE.
    let (_, records, pull) = run_and_pull(
        &mut client,
        "USE system CALL db.info() YIELD name RETURN name",
        None,
    );
    assert_eq!(records, vec![Pack::String("system".into())]);
    assert_eq!(success_db(&pull), "system");

    // ADR-0229 closed `system_ontologies` to statement-level USE probes
    // (nothing composes there — even SHOW fails the USE composition), so
    // the `system` half above is the pin.

    // DISCARD with a pending result also echoes the USE target, not the
    // session namespace.
    send_run_at(
        &mut client,
        BoltVersion::V4_4,
        "USE system CALL db.info() YIELD name RETURN name",
        None,
    );
    success_map(read_msg(&mut client));
    send_pull(&mut client, Message::Discard, vec![]);
    let discard = success_map(read_msg(&mut client));
    assert_eq!(success_db(&discard), "system");

    // A statement without USE echoes the session namespace on DISCARD.
    send_run_at(&mut client, BoltVersion::V4_4, "RETURN 1 AS n", None);
    success_map(read_msg(&mut client));
    send_pull(&mut client, Message::Discard, vec![]);
    let discard = success_map(read_msg(&mut client));
    assert_eq!(success_db(&discard), "neo4j");
}

#[test]
fn discard_success_carries_db_and_hello_is_unchanged() {
    // The fixture's own store (its opt-in is the fixture's precondition,
    // ADR-0593) — a bespoke open would miss it under nextest's
    // one-process-per-test model.
    let addr = spawn_handler(crate::testutil::memory_db(), ConnConfig::default());
    let mut client = crate::testutil::connect(addr);
    crate::testutil::handshake(&mut client);

    // HELLO is unchanged (ADR-0117 §7): `server`, no `db`.
    let hello = hello(&mut client);
    assert!(
        hello.iter().any(|(k, v)| k.as_str() == Some("server")
            && v.as_str().is_some_and(|s| s.starts_with("Relate/"))),
        "HELLO must report the server: {hello:?}"
    );
    assert!(
        !hello.iter().any(|(k, _)| k.as_str() == Some("db")),
        "HELLO must not carry db: {hello:?}"
    );

    send_run_at(
        &mut client,
        BoltVersion::V4_4,
        "SHOW DATABASES",
        Some(db_extra(Pack::String("system_ontologies".into()))),
    );
    success_map(read_msg(&mut client));

    send_pull(&mut client, Message::Discard, vec![]);
    let discard = success_map(read_msg(&mut client));
    assert_eq!(success_db(&discard), "system_ontologies");

    // ROUTE (added on main by #144) answers the single-server routing
    // table; with no extra `db` it reports the home database "neo4j"
    // (ADR-0318).
    send_struct(
        &mut client,
        Message::Route.signature(),
        vec![Pack::Map(vec![])],
    );
    let route = success_map(read_msg(&mut client));
    let rt = route
        .iter()
        .find(|(k, _)| k.as_str() == Some("rt"))
        .and_then(|(_, v)| v.as_map())
        .expect("ROUTE answers a routing table");
    let db = rt
        .iter()
        .find(|(k, _)| k.as_str() == Some("db"))
        .and_then(|(_, v)| v.as_str())
        .expect("routing table carries db");
    assert_eq!(db, "neo4j");
}

#[test]
fn run_extra_db_inside_a_transaction_is_ignored() {
    let mut client = crate::testutil::connected_client();

    send_begin(&mut client, db_extra(Pack::String("system".into())));
    success_map(read_msg(&mut client));

    // A RUN inside the transaction keeps the BEGIN-time namespace even when
    // it carries a `db` (ADR-0117 §7): the namespace cannot change while a
    // transaction is open, so the extra is ignored rather than applied.
    // (The probe is an administration command: `system` answers those, not
    // graph queries — ADR-0116/ADR-0229.)
    let (_, records, pull) = run_and_pull(
        &mut client,
        "SHOW DATABASES",
        Some(db_extra(Pack::String("neo4j".into()))),
    );
    assert_eq!(records.len(), 3);
    assert_eq!(success_db(&pull), "system");

    send_rollback(&mut client);
    success_map(read_msg(&mut client));
}

#[test]
fn nested_begin_is_a_breach_before_its_extra_db_is_applied() {
    let mut client = crate::testutil::connected_client();

    send_begin(
        &mut client,
        db_extra(Pack::String("system_ontologies".into())),
    );
    success_map(read_msg(&mut client));

    // A nested BEGIN is a protocol breach (ADR-0314), detected from the
    // session state before its extra's `db` or the engine is touched: one
    // FAILURE, then the server closes the connection.
    send_begin(&mut client, db_extra(Pack::String("neo4j".into())));
    let fields = failure_map(read_msg(&mut client));
    assert_eq!(field(&fields, "code"), REQUEST_INVALID_CODE);
    assert_eq!(
        field(&fields, "message"),
        "Message of type BeginMessage cannot be handled by a session in the IN_TRANSACTION state."
    );
    assert_closes(&mut client, "a nested BEGIN closes the connection");
}
