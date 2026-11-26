//! The managed-transaction lease: statements run outside the registry
//! mutex, leased entries keep counting against the cap, and a leased entry
//! is invisible to a racing close. The tests and their `start_slow` harness
//! live together; the shared fixtures (registry, reply helpers) live in
//! `query_api::tests`.

use super::tests::*;
use super::*;
use crate::reply::ResponseMediaType;
use crate::typed_params::RequestDialect;
use crate::v2_envelope::tests::body_json;

/// Wait until a route has leased transaction `id` out of the registry — its
/// statement is in flight with the registry mutex released. Under the
/// pre-lease shape the mutex was held for the statement's whole duration
/// and the entry stayed in the map, so this wait times out.
fn wait_leased(txs: &TxRegistry, id: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while txs.holds(id) {
        assert!(
            std::time::Instant::now() < deadline,
            "transaction {id} never left the registry: the statement ran under the mutex"
        );
        // Yield rather than sleep: the lease is a few instructions away and
        // yields let its thread take the registry lock without the test
        // waiting on a wall-clock timer.
        std::thread::yield_now();
    }
}

/// A tx route that takes one statement — the run and commit routes share
/// this signature exactly, so [`start_slow`] takes either as a value.
type TxRoute = fn(
    &str,
    &[u8],
    ResponseMediaType,
    &TxRegistry,
    Option<&str>,
    RequestDialect,
    relate::TimeoutPolicy,
) -> Result<Reply, Reply>;

/// Start [`SLOW_STATEMENT`] in transaction `id` on a scoped thread through
/// `route` (the run route, or the commit route whose final statement it
/// is), and wait until the route has leased the entry out of the registry —
/// the statement is in flight, the entry is out of the map, and its pin
/// still counts.
fn start_slow<'scope>(
    scope: &'scope std::thread::Scope<'scope, '_>,
    txs: &'scope TxRegistry,
    id: &'scope str,
    route: TxRoute,
) -> (
    std::sync::mpsc::Receiver<Result<Reply, Reply>>,
    std::thread::ScopedJoinHandle<'scope, ()>,
) {
    let (sender, receiver) = std::sync::mpsc::channel();
    let handle = scope.spawn(move || {
        let outcome = route(
            id,
            SLOW_STATEMENT.as_bytes(),
            ResponseMediaType::Json,
            txs,
            None,
            RequestDialect::Plain,
            relate::TimeoutPolicy::default(),
        );
        let _ = sender.send(outcome);
    });
    wait_leased(txs, id);
    (receiver, handle)
}

#[test]
fn a_slow_statement_does_not_block_another_transactions_begin_and_commit() {
    // The registry mutex is never held across an engine statement. One
    // transaction runs a deliberately slow statement while a second
    // transaction begins, runs, and commits — all of it completes
    // while the slow statement is still in flight, bounded by a generous
    // timeout and never by a real idle deadline.
    let db = db();
    let txs = TxRegistry::new();
    let slow = tx_id(&begin_tx_reply(
        &db,
        &txs,
        "/db/neo4j/query/v2/tx",
        r#"{"accessMode":"READ"}"#,
    ));
    std::thread::scope(|scope| {
        let (slow_done, slow_handle) = start_slow(scope, &txs, &slow, crate::tx::run);
        let started = std::time::Instant::now();
        let second = begin_tx_reply(&db, &txs, "/db/neo4j/query/v2/tx", "{}");
        let second_id = tx_id(&second);
        let commit = dispatch(
            &db,
            &tx_post(&format!("/db/neo4j/query/v2/tx/{second_id}/commit"), ""),
            OPEN,
            &txs,
        );
        let elapsed = started.elapsed();
        assert_eq!(commit.status, 200, "the second transaction committed");
        assert!(
            elapsed < std::time::Duration::from_secs(20),
            "a whole begin + commit inside another transaction's slow \
             statement took {elapsed:?}: the registry mutex was held across it"
        );
        assert!(
            slow_done.try_recv().is_err(),
            "the slow statement finished before the second transaction did — \
             the two never overlapped"
        );
        let slow_reply = reply_ok(slow_done.recv().expect("the slow run reports"));
        assert_eq!(slow_reply.status, 200);
        assert_eq!(body_json(&slow_reply)["transaction"]["id"], json!(slow));
        slow_handle.join().expect("the slow run thread");
    });
}

#[test]
fn the_cap_counts_transactions_leased_out_for_a_running_statement() {
    // ADR-0435 §2: a leased transaction is out of the map but still holds
    // its snapshot pin, so it keeps counting against
    // the cap. Concurrent long statements therefore cannot double the
    // registry's footprint (2 x max_open) — they are bounded by max_open in
    // total — and the count is given back when each statement returns.
    let db = db();
    let txs = registry(std::time::Duration::from_secs(60), 2);
    let first = begin_tx(&db, &txs);
    let second = begin_tx(&db, &txs);
    std::thread::scope(|scope| {
        let (first_done, first_handle) = start_slow(scope, &txs, &first, crate::tx::run);
        let (second_done, second_handle) = start_slow(scope, &txs, &second, crate::tx::run);
        // Both entries are out of the map and both still count: the cap is
        // full, so a third begin is refused exactly as if they were
        // registered.
        let refused = dispatch(&db, &tx_post("/db/neo4j/query/v2/tx", "{}"), OPEN, &txs);
        assert_eq!(refused.status, 429, "a leased entry still counts");
        assert_eq!(body_json(&refused)["errors"][0]["code"], json!("42N91"));
        assert!(
            first_done.try_recv().is_err() && second_done.try_recv().is_err(),
            "both slow statements finished before the cap was read"
        );
        assert_eq!(
            reply_ok(first_done.recv().expect("the first slow run reports")).status,
            200
        );
        assert_eq!(
            reply_ok(second_done.recv().expect("the second slow run reports")).status,
            200
        );
        first_handle.join().expect("the first slow run thread");
        second_handle.join().expect("the second slow run thread");
    });
    // Both slots came back: closing the two transactions empties the cap,
    // so a leaked in-flight count would refuse this begin with 429.
    for id in [&first, &second] {
        let commit = dispatch(
            &db,
            &tx_post(&format!("/db/neo4j/query/v2/tx/{id}/commit"), ""),
            OPEN,
            &txs,
        );
        assert_eq!(commit.status, 200, "{id} commits");
    }
    begin_tx_reply(&db, &txs, "/db/neo4j/query/v2/tx", "{}");
}

#[test]
fn the_cap_counts_transactions_leased_out_for_a_commit() {
    // ADR-0435 §2: a commit's final statement runs with the transaction out
    // of the map but still holding its snapshot pin, so the commit lease
    // counts against the cap exactly like a run's. One registered
    // transaction plus one committing one fills a cap of two: a begin is
    // refused while the commit's statement is in flight, and admitted once
    // the commit hands its entry out.
    let db = db();
    let txs = registry(std::time::Duration::from_secs(60), 2);
    let first = begin_tx(&db, &txs);
    let second = begin_tx(&db, &txs);
    std::thread::scope(|scope| {
        let (commit_done, commit_handle) = start_slow(scope, &txs, &first, crate::tx::commit);
        assert!(
            commit_done.try_recv().is_err(),
            "the commit's final statement finished before the cap was read"
        );
        assert_eq!(
            txs.occupancy(),
            (1, 1),
            "the committing entry is out of the map and counted in flight"
        );
        // One registered + one committing = the cap, so this begin is
        // refused exactly as if the committing entry were still registered.
        let refused = dispatch(&db, &tx_post("/db/neo4j/query/v2/tx", "{}"), OPEN, &txs);
        assert_eq!(refused.status, 429, "a committing entry still counts");
        assert_eq!(body_json(&refused)["errors"][0]["code"], json!("42N91"));
        assert_eq!(
            reply_ok(commit_done.recv().expect("the commit reports")).status,
            200
        );
        commit_handle.join().expect("the commit thread");
    });
    assert_eq!(
        txs.occupancy(),
        (1, 0),
        "the commit left the registry and gave its slot back"
    );
    // The slot the commit returned is admitted: the cap is reachable again.
    let third = begin_tx(&db, &txs);
    assert_eq!(txs.occupancy(), (2, 0));
    for id in [&second, &third] {
        let commit = dispatch(
            &db,
            &tx_post(&format!("/db/neo4j/query/v2/tx/{id}/commit"), ""),
            OPEN,
            &txs,
        );
        assert_eq!(commit.status, 200, "{id} commits");
    }
    assert_eq!(txs.occupancy(), (0, 0));
}

#[test]
fn a_commit_whose_final_statement_fails_rolls_the_transaction_back() {
    // ADR-0435 §2 / ADR-0237: the commit lease's unwind path is the
    // rollback. A failing final statement answers its own 4xx, drops the
    // leased entry (the `Connection` drop rolls the transaction back), and
    // gives the cap slot back — the uncommitted write leaves no trace and
    // the id is gone.
    let db = db();
    let txs = TxRegistry::new();
    let begin = begin_tx_reply(
        &db,
        &txs,
        "/db/neo4j/query/v2/tx",
        r#"{"statement":"CREATE (:Uncommitted {v: 1})"}"#,
    );
    let id = tx_id(&begin);
    // The write is visible inside its transaction…
    let inside = dispatch(
        &db,
        &tx_post(
            &format!("/db/neo4j/query/v2/tx/{id}"),
            r#"{"statement":"MATCH (n:Uncommitted) RETURN count(n) AS c"}"#,
        ),
        OPEN,
        &txs,
    );
    assert_eq!(
        body_json(&inside)["data"]["values"],
        json!([[1]]),
        "the write is visible inside its transaction"
    );
    // … the final statement fails, so the commit answers its 4xx and the
    // lease unwinds into the rollback.
    let failed = dispatch(
        &db,
        &tx_post(
            &format!("/db/neo4j/query/v2/tx/{id}/commit"),
            r#"{"statement":"RETURN"}"#,
        ),
        OPEN,
        &txs,
    );
    assert_eq!(failed.status, 400);
    assert_eq!(body_json(&failed)["errors"][0]["code"], json!("42N06"));
    assert_eq!(
        txs.occupancy(),
        (0, 0),
        "the failed commit left nothing registered or in flight"
    );
    // The entry is gone: a later close on the id is the generic not-found.
    let again = dispatch(&db, &rollback_request(&id), OPEN, &txs);
    assert_eq!(again.status, 404);
    assert_eq!(body_json(&again)["errors"][0]["code"], json!("42000"));
    // The rollback discarded the write: a fresh read cannot see it.
    let check = dispatch(
        &db,
        &post(
            "neo4j",
            r#"{"statement": "MATCH (n:Uncommitted) RETURN count(n) AS c"}"#,
        ),
        OPEN,
        &TxRegistry::new(),
    );
    assert_eq!(body_json(&check)["data"]["values"], json!([[0]]));
}

#[test]
fn a_leased_transaction_answers_a_generic_not_found_and_survives() {
    // The lease's documented semantics: while one transaction's statement
    // holds its connection, a commit or rollback
    // naming that id finds no entry and answers the unchanged generic
    // 404/42000 — the transaction is never closed under a running
    // statement. Once the statement completes and the entry is back, the
    // transaction is intact and closable.
    let db = db();
    let txs = TxRegistry::new();
    let id = tx_id(&begin_tx_reply(&db, &txs, "/db/neo4j/query/v2/tx", "{}"));
    std::thread::scope(|scope| {
        let (slow_done, slow_handle) = start_slow(scope, &txs, &id, crate::tx::run);
        let commit = dispatch(
            &db,
            &tx_post(&format!("/db/neo4j/query/v2/tx/{id}/commit"), ""),
            OPEN,
            &txs,
        );
        assert_eq!(commit.status, 404);
        assert_eq!(body_json(&commit)["errors"][0]["code"], json!("42000"));
        let rollback = dispatch(&db, &rollback_request(&id), OPEN, &txs);
        assert_eq!(rollback.status, 404);
        assert_eq!(body_json(&rollback)["errors"][0]["code"], json!("42000"));
        assert_eq!(
            reply_ok(slow_done.recv().expect("the slow run reports")).status,
            200
        );
        slow_handle.join().expect("the slow run thread");
    });
    let commit = dispatch(
        &db,
        &tx_post(&format!("/db/neo4j/query/v2/tx/{id}/commit"), ""),
        OPEN,
        &txs,
    );
    assert_eq!(commit.status, 200, "the transaction survived the lease");
}
