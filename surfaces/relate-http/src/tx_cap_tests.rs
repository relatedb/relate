//! The managed-transaction cap: the 429/42N91 refusal, the begin-slot
//! reservation under a concurrent burst, and failure paths returning their
//! slot. Split from the single `tx_tests.rs` by concern; the lease-carrying
//! cap cases (statements in flight) live in `tx_lease_tests` beside their
//! harness, and the shared fixtures live in `query_api::tests`.

use super::tests::*;
use super::*;
use crate::reply::ResponseMediaType;
use crate::typed_params::RequestDialect;
use crate::v2_envelope::tests::body_json;

#[test]
fn the_cap_refuses_a_begin_with_429_42n91_and_a_sweep_frees_room() {
    // ADR-0435 §2/§3: the cap is read after the sweep, the refusal is a
    // 429 carrying 42N91 and naming the cap and the flag that raises it.
    let db = db();
    let txs = registry(std::time::Duration::from_secs(60), 2);
    let first = tx_id(&begin_tx_reply(&db, &txs, "/db/neo4j/query/v2/tx", "{}"));
    begin_tx_reply(&db, &txs, "/db/neo4j/query/v2/tx", "{}");

    let refused = dispatch(&db, &tx_post("/db/neo4j/query/v2/tx", "{}"), OPEN, &txs);
    assert_eq!(refused.status, 429);
    let body = body_json(&refused);
    assert_eq!(body["errors"][0]["code"], json!("42N91"));
    let message = body["errors"][0]["message"].as_str().unwrap();
    assert!(message.contains("2 open transactions"), "{message}");
    assert!(message.contains("--max-open-transactions"), "{message}");
    // The refusal happened before the engine: the open transactions are
    // untouched and still usable.
    let still_open = dispatch(
        &db,
        &tx_post(&format!("/db/neo4j/query/v2/tx/{first}"), ""),
        OPEN,
        &txs,
    );
    assert_eq!(still_open.status, 200);

    // The sweep runs first, so an expired entry frees capacity for the new
    // begin instead of counting against the cap.
    txs.age_past_deadline();
    begin_tx_reply(&db, &txs, "/db/neo4j/query/v2/tx", "{}");
}

/// The burst's begin body: a statement long enough that the whole burst's
/// admitted-but-unregistered window outlives a descheduled poller, so the
/// burst test can read that window directly instead of through a test-only
/// phase gate. The nested 1200 × 1200 UNWIND does 1.44M row evaluations while
/// keeping every ADR-0463 allocation and retaining-container total under the
/// 256 MiB cap. The inner UNWIND's materialized rows are small and the
/// `count` group's `vals` buffer fits; a much larger nested product would
/// cross the retained total and be refused. The assertions are unchanged.
const BURST_STATEMENT: &str = r#"{"statement":"UNWIND range(1, 1200) AS a UNWIND range(1, 1200) AS b WITH a, b WHERE a * b >= 0 RETURN count(b)"}"#;

#[test]
fn a_burst_of_admitted_begins_cannot_overshoot_the_cap() {
    // ADR-0435 §2: a begin's slot is reserved under the registry lock
    // before its connection is opened and only converts into a registered
    // entry once the begin body ran. The burst's begin bodies are
    // [`BURST_STATEMENT`], so the whole burst sits in that
    // admitted-but-unregistered window for the statement's duration once
    // it is spawned together — the state check-then-act admit left
    // unbounded — and the next begin is refused there, so the cap is an
    // exact bound rather than a count concurrent begins can race past.
    let db = db();
    let max_open = 2;
    let txs = registry(std::time::Duration::from_secs(60), max_open);
    let ids = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..max_open)
            .map(|_| {
                scope.spawn(|| {
                    crate::tx::begin(
                        &db,
                        "neo4j",
                        BURST_STATEMENT.as_bytes(),
                        ResponseMediaType::Json,
                        &txs,
                        None,
                        RequestDialect::Plain,
                        relate::TimeoutPolicy::default(),
                    )
                })
            })
            .collect();
        // Poll to the expected occupancy rather than asserting a single
        // read: the burst threads are admitted one at a time, so a
        // descheduled runner could momentarily observe an intermediate
        // pair. `(0, max_open)` is the admitted-but-unregistered state
        // exactly; the registered state is `(max_open, 0)`, which the poll
        // must not mistake for it.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while txs.occupancy() != (0, max_open) {
            assert!(
                std::time::Instant::now() < deadline,
                "the burst never held the cap's slots: {:?}",
                txs.occupancy()
            );
            std::thread::yield_now();
        }
        let refused = dispatch(&db, &tx_post("/db/neo4j/query/v2/tx", "{}"), OPEN, &txs);
        assert_eq!(
            refused.status, 429,
            "a burst of admitted begins cannot exceed the cap"
        );
        assert_eq!(body_json(&refused)["errors"][0]["code"], json!("42N91"));
        handles
            .into_iter()
            .map(|handle| tx_id(&reply_ok(handle.join().expect("a burst begin thread"))))
            .collect::<Vec<_>>()
    });
    assert_eq!(
        txs.occupancy(),
        (max_open, 0),
        "the burst registered inside the cap and gave every reservation back"
    );
    for id in ids {
        let commit = dispatch(
            &db,
            &tx_post(&format!("/db/neo4j/query/v2/tx/{id}/commit"), ""),
            OPEN,
            &txs,
        );
        assert_eq!(commit.status, 200, "{id} commits");
    }
    assert_eq!(txs.occupancy(), (0, 0));
    begin_tx_reply(&db, &txs, "/db/neo4j/query/v2/tx", "{}");
}

#[test]
fn a_begin_that_fails_after_its_reservation_returns_the_slot() {
    // ADR-0435 §2: the reservation spans the begin's whole engine span —
    // connect, `Connection::begin`, and the begin body's statement — so a
    // failure there returns the slot instead of leaking it. A one-slot
    // registry proves it: the failed begin consumes nothing, and the next
    // begin is admitted.
    let db = db();
    let txs = registry(std::time::Duration::from_secs(60), 1);
    let failed = dispatch(
        &db,
        &tx_post("/db/neo4j/query/v2/tx", r#"{"statement":"RETURN"}"#),
        OPEN,
        &txs,
    );
    assert_eq!(failed.status, 400);
    assert_eq!(body_json(&failed)["errors"][0]["code"], json!("42N06"));
    assert_eq!(
        txs.occupancy(),
        (0, 0),
        "the failed begin returned its slot"
    );
    begin_tx_reply(&db, &txs, "/db/neo4j/query/v2/tx", "{}");
}
