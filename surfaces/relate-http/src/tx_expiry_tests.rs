//! The managed-transaction expiry flow: the honest `expires` deadline, the
//! typed first-observer expiry, and the sweep's pin release. Split from the
//! single `tx_tests.rs` by concern; the shared fixtures (the registry
//! builder, the reply helpers, `expires`, and the first-observer assertion)
//! live in `query_api::tests`.

use super::tests::*;
use super::*;
use crate::reply::ResponseMediaType;
use crate::tx_limits::TxLimits;
use crate::typed_params::RequestDialect;
use crate::v2_envelope::tests::body_json;

#[test]
fn a_registry_defaults_to_the_adr_0435_limits_and_takes_configured_ones() {
    // `TxRegistry::new()` keeps the ADR-0435 defaults; `with_limits` is
    // the configured constructor product `serve` uses, and the limits it
    // stores are the enforcement's one copy (not an `HttpConfig` mirror).
    assert_eq!(TxRegistry::new().limits(), TxLimits::default());
    let configured = TxLimits::new(std::time::Duration::from_secs(5), 3)
        .expect("the fixture limits are renderable");
    assert_eq!(TxRegistry::with_limits(configured).limits(), configured);
}

#[test]
fn an_unknown_access_mode_is_a_400_not_a_writable_tx() {
    // accessMode is typed at the body boundary: a misspelled value never
    // silently opens a writable transaction.
    let reply = dispatch(
        &db(),
        &tx_post("/db/neo4j/query/v2/tx", r#"{"accessMode":"red"}"#),
        OPEN,
        &txs(),
    );
    assert_eq!(reply.status, 400);
    assert_eq!(body_json(&reply)["errors"][0]["code"], json!("42N06"));
    // The wire values are READ and WRITE (uppercase, per the Query API v2).
    let reply = dispatch(
        &db(),
        &tx_post("/db/neo4j/query/v2/tx", r#"{"accessMode":"WRITE"}"#),
        OPEN,
        &txs(),
    );
    assert_eq!(reply.status, 201, "WRITE opens a writable transaction");
}

#[test]
fn tx_begin_run_commit_and_rollback_flow() {
    let db = db();
    let txs = TxRegistry::new();
    // Pure BEGIN on the system namespace (Browser's shape:
    // {"accessMode":"READ"}).
    let begin = begin_tx_reply(
        &db,
        &txs,
        "/db/system/query/v2/tx",
        r#"{"accessMode":"READ"}"#,
    );
    let id = tx_id(&begin);
    assert!(id.starts_with("tx-"));
    assert!(body_json(&begin)["transaction"]["expires"].is_string());

    // A read runs in it…
    let run = dispatch(
        &db,
        &tx_post(
            &format!("/db/system/query/v2/tx/{id}"),
            r#"{"statement":"CALL db.ping()"}"#,
        ),
        OPEN,
        &txs,
    );
    assert_eq!(run.status, 200);
    let run_body = body_json(&run);
    assert_eq!(run_body["data"]["values"], json!([[true]]));
    assert_eq!(run_body["transaction"]["id"], json!(id));
    // … but a write is rejected on a READ transaction (and the tx
    // survives for the client to close). On the neo4j namespace, where
    // CREATE is otherwise legal, so the guard's own message shows.
    let read_tx = begin_tx_reply(
        &db,
        &txs,
        "/db/neo4j/query/v2/tx",
        r#"{"accessMode":"READ"}"#,
    );
    let read_tx_id = tx_id(&read_tx);
    let write = dispatch(
        &db,
        &tx_post(
            &format!("/db/neo4j/query/v2/tx/{read_tx_id}"),
            r#"{"statement":"CREATE (:X)"}"#,
        ),
        OPEN,
        &txs,
    );
    assert_eq!(write.status, 400);
    let write_body = body_json(&write);
    assert!(
        write_body["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("READ transaction"),
        "{write_body}"
    );
    // Commit closes it; a second use is a 404.
    let commit = dispatch(
        &db,
        &tx_post(&format!("/db/system/query/v2/tx/{id}/commit"), ""),
        OPEN,
        &txs,
    );
    assert_eq!(commit.status, 200);
    let commit_body = body_json(&commit);
    assert_eq!(commit_body["bookmarks"], json!([]));
    assert!(commit_body.get("transaction").is_none());
    let gone = dispatch(
        &db,
        &tx_post(&format!("/db/system/query/v2/tx/{id}"), "{}"),
        OPEN,
        &txs,
    );
    assert_eq!(gone.status, 404);

    // Rollback actually discards a write: a write tx…
    let begin = begin_tx_reply(
        &db,
        &txs,
        "/db/neo4j/query/v2/tx",
        r#"{"statement":"CREATE (:Tmp {v: 1})"}"#,
    );
    assert_eq!(body_json(&begin)["counters"]["nodes_created"], 1);
    let id = tx_id(&begin);
    // … rolled back …
    let rb = dispatch(&db, &rollback_request(&id), OPEN, &txs);
    assert_eq!(rb.status, 202);
    // … leaves no trace.
    let check = dispatch(
        &db,
        &post(
            "neo4j",
            r#"{"statement": "MATCH (t:Tmp) RETURN count(t) AS c"}"#,
        ),
        OPEN,
        &TxRegistry::new(),
    );
    assert_eq!(body_json(&check)["data"]["values"], json!([[0]]));
}

#[test]
fn an_expired_transaction_answers_42n90_to_its_first_observer_only() {
    // ADR-0435 §1/§3: the operation whose own sweep reclaims an idle
    // transaction answers the typed expiry; once the entry is gone, later
    // requests get the unchanged generic not-found.
    let db = db();
    let txs = registry(std::time::Duration::from_secs(60), 128);
    let begin = begin_tx_reply(
        &db,
        &txs,
        "/db/system/query/v2/tx",
        r#"{"accessMode":"READ"}"#,
    );
    let id = tx_id(&begin);
    txs.age_past_deadline();
    let run = dispatch(
        &db,
        &tx_post(&format!("/db/system/query/v2/tx/{id}"), ""),
        OPEN,
        &txs,
    );
    assert_expired_first_observer(&id, &run);

    // The entry left the registry with that sweep: every later request on
    // the id is the generic not-found (the accepted degradation, §1).
    let again = dispatch(
        &db,
        &tx_post(&format!("/db/system/query/v2/tx/{id}"), ""),
        OPEN,
        &txs,
    );
    assert_eq!(again.status, 404);
    assert_eq!(body_json(&again)["errors"][0]["code"], json!("42000"));
}

#[test]
fn commit_and_rollback_of_an_expired_transaction_answer_42n90() {
    // ADR-0435 §3: the typed expiry answers *any* route naming the id, not
    // just the run route — a commit or rollback whose own sweep reclaims
    // the transaction answers 404/42N90, and only its first observer does.
    let db = db();
    let txs = registry(std::time::Duration::from_secs(60), 128);
    let commit_id = begin_tx(&db, &txs);
    txs.age_past_deadline();
    let commit = dispatch(
        &db,
        &tx_post(&format!("/db/neo4j/query/v2/tx/{commit_id}/commit"), ""),
        OPEN,
        &txs,
    );
    assert_expired_first_observer(&commit_id, &commit);
    // The entry left with that sweep: a later commit on the id is the
    // unchanged generic not-found, exactly like an unknown id.
    let again = dispatch(
        &db,
        &tx_post(&format!("/db/neo4j/query/v2/tx/{commit_id}/commit"), ""),
        OPEN,
        &txs,
    );
    assert_eq!(again.status, 404);
    assert_eq!(body_json(&again)["errors"][0]["code"], json!("42000"));

    // Rollback takes the same hand-off, so it answers the typed expiry on
    // its own first observation too.
    let rollback_id = begin_tx(&db, &txs);
    txs.age_past_deadline();
    let rollback = dispatch(&db, &rollback_request(&rollback_id), OPEN, &txs);
    assert_expired_first_observer(&rollback_id, &rollback);
    let again = dispatch(&db, &rollback_request(&rollback_id), OPEN, &txs);
    assert_eq!(again.status, 404);
    assert_eq!(body_json(&again)["errors"][0]["code"], json!("42000"));
}

#[test]
fn expiry_never_discloses_another_callers_transaction() {
    // ADR-0428 owner scoping survives expiry (ADR-0435 §3): a caller that
    // does not own the transaction gets the generic not-found, so the
    // typed code never discloses that the id existed.
    let db = db();
    let txs = registry(std::time::Duration::from_secs(60), 128);
    let open = |owner: &str| {
        let reply = reply_ok(crate::tx::begin(
            &db,
            "neo4j",
            b"{}",
            ResponseMediaType::Json,
            &txs,
            Some(owner),
            RequestDialect::Plain,
            relate::TimeoutPolicy::default(),
        ));
        tx_id(&reply)
    };
    let stranger_id = open("alice");
    txs.age_past_deadline();
    let stranger = reply_err(crate::tx::run(
        &stranger_id,
        b"",
        ResponseMediaType::Json,
        &txs,
        Some("mallory"),
        RequestDialect::Plain,
        relate::TimeoutPolicy::default(),
    ));
    assert_eq!(stranger.status, 404);
    assert_eq!(body_json(&stranger)["errors"][0]["code"], json!("42000"));

    // The owner's own first observation of an expiry is the typed one.
    let owner_id = open("alice");
    txs.age_past_deadline();
    let owner = reply_err(crate::tx::run(
        &owner_id,
        b"",
        ResponseMediaType::Json,
        &txs,
        Some("alice"),
        RequestDialect::Plain,
        relate::TimeoutPolicy::default(),
    ));
    assert_eq!(owner.status, 404);
    assert_eq!(body_json(&owner)["errors"][0]["code"], json!("42N90"));
}

#[test]
fn expires_is_the_configured_idle_window_not_a_fixed_60_seconds() {
    // ADR-0435 §1: `expires` is `last_activity + idle_timeout`, so the
    // configured 5 s window is what a reply renders — not the old
    // fabricated now + 60 s. The refreshed-after-a-statement half of the
    // contract is pinned end to end by the wire test in `server_tests`.
    let db = db();
    let txs = registry(std::time::Duration::from_secs(5), 0);
    let begin = begin_tx_reply(&db, &txs, "/db/neo4j/query/v2/tx", "{}");
    let deadline = parse_expires(&body_json(&begin));
    let remaining = deadline - chrono::Utc::now();
    assert!(
        (4_000..=5_100).contains(&remaining.num_milliseconds()),
        "expires is the configured 5 s deadline, not 60 s: {remaining}"
    );
}

#[test]
fn a_disabled_idle_timeout_omits_expires_and_never_reclaims() {
    // ADR-0435 §1: with expiry disabled there is no deadline to report,
    // so the transaction block is just the id — and nothing is swept. The
    // disabled fact is the typed limits' `None`, not a zero read here.
    let db = db();
    let txs = registry(std::time::Duration::ZERO, 0);
    let begin = begin_tx_reply(
        &db,
        &txs,
        "/db/system/query/v2/tx",
        r#"{"accessMode":"READ"}"#,
    );
    let id = tx_id(&begin);
    assert_eq!(body_json(&begin)["transaction"], json!({ "id": id }));

    txs.age_past_deadline();
    let run = dispatch(
        &db,
        &tx_post(&format!("/db/system/query/v2/tx/{id}"), ""),
        OPEN,
        &txs,
    );
    assert_eq!(run.status, 200);
    assert!(body_json(&run)["transaction"].get("expires").is_none());
}

#[test]
fn the_sweep_drops_the_expired_connection_and_frees_its_pin() {
    // A swept entry owns its engine Connection; the drop is what rolls the
    // transaction back and releases its snapshot pin (ADR-0435 §1). The
    // pin table is the observable: fill every slot through this registry,
    // prove the next begin is refused by pin exhaustion, then expire the
    // entries and prove the next begin's sweep freed the table.
    let db = db();
    let txs = registry(std::time::Duration::from_secs(60), 0);
    let mut opened = 0;
    loop {
        let reply = dispatch(&db, &tx_post("/db/neo4j/query/v2/tx", "{}"), OPEN, &txs);
        if reply.status != 201 {
            let body = body_json(&reply);
            let message = body["errors"][0]["message"].as_str().unwrap();
            assert!(message.contains("snapshot pin"), "{body}");
            break;
        }
        opened += 1;
        assert!(opened < 1024, "the snapshot pin table never filled");
    }
    assert!(opened > 0, "no pin was ever taken");

    // Without dropping the swept Connections the pins would stay held and
    // this begin would fail exactly as the loop's last one did.
    txs.age_past_deadline();
    begin_tx_reply(&db, &txs, "/db/neo4j/query/v2/tx", "{}");
}
