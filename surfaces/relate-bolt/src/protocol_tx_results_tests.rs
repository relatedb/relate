//! Open results addressed by qid in explicit transactions (ADR-0311).

use std::net::TcpStream;
use std::sync::Arc;

use super::*;
use crate::failures::REQUEST_INVALID_CODE;
use crate::packstream::Pack;
use crate::requests::Message;
use crate::testutil::{
    assert_ignored, bookmark_sequence, connected_client, connected_client_with, drain_all,
    failure_map, field, map, pull_ending, read_msg, send_begin, send_commit, send_pull, send_reset,
    send_run, success_map,
};

fn begin(client: &mut TcpStream) {
    send_begin(client, vec![]);
    success_map(read_msg(client));
}

/// RUN `cypher` and return the RUN SUCCESS `qid`.
fn run_qid(client: &mut TcpStream, cypher: &str) -> Option<i64> {
    send_run(client, cypher);
    Pack::Map(success_map(read_msg(client)))
        .get("qid")
        .and_then(Pack::as_int)
}

/// PULL/DISCARD (`message`) with a caller's `extra` (a qid or a bounded
/// `n` — the plain `n = -1` drain is [`crate::testutil::pull_ending`]);
/// the records' first Int columns and the SUCCESS map.
fn consume(client: &mut TcpStream, message: Message, extra: Vec<(&str, Pack)>) -> (Vec<i64>, Pack) {
    let (records, ending) = pull_ending(client, message, extra);
    (int_column(&records), ending)
}

/// The first Int column of each drained record — the one spelling every
/// Int-column read in this module uses, over the drained record values
/// (`record[0]`, the same cell `consume` reads).
fn int_column(records: &[Vec<Pack>]) -> Vec<i64> {
    records
        .iter()
        .map(|record| record[0].as_int().expect("an Int column"))
        .collect()
}

fn has_more(m: &Pack) -> bool {
    m.get("has_more") == Some(&Pack::Bool(true))
}

/// F-23, F-24: two RUNs in one transaction get qids 0 and 1 and keep their
/// own results; `qid` absent addresses the most recent RUN.
#[test]
fn each_run_in_a_transaction_keeps_its_own_result() {
    let mut client = connected_client();
    begin(&mut client);
    assert_eq!(run_qid(&mut client, "UNWIND [1, 2] AS a RETURN a"), Some(0));
    assert_eq!(
        run_qid(&mut client, "UNWIND [10, 20] AS b RETURN b"),
        Some(1)
    );
    let (rows, ending) = consume(
        &mut client,
        Message::Pull,
        vec![("n", Pack::Int(-1)), ("qid", Pack::Int(0))],
    );
    assert_eq!(rows, [1, 2]);
    assert_eq!(ending.get("type"), Some(&Pack::String("r".into())));
    assert!(ending.get("bookmark").is_none(), "{ending:?}");
    let (records, _) = drain_all(&mut client, Message::Pull);
    let rows = int_column(&records);
    assert_eq!(rows, [10, 20]);
    send_commit(&mut client);
    success_map(read_msg(&mut client));
}

/// D-41: interleaved `n = 1` pulls on two open results each get only their
/// own rows, and `qid = -1` means the most recent RUN.
#[test]
fn interleaved_pulls_get_each_result_its_own_rows() {
    let mut client = connected_client();
    begin(&mut client);
    run_qid(&mut client, "UNWIND [1, 2, 3] AS a RETURN a");
    run_qid(&mut client, "UNWIND [10, 20] AS b RETURN b");
    let mut pulled = Vec::new();
    for qid in [0, -1, 0, 1, 0] {
        let (rows, end) = consume(
            &mut client,
            Message::Pull,
            vec![("n", Pack::Int(1)), ("qid", Pack::Int(qid))],
        );
        pulled.push((qid, rows, has_more(&end)));
    }
    assert_eq!(
        pulled,
        [
            (0, vec![1], true),
            (-1, vec![10], true),
            (0, vec![2], true),
            (1, vec![20], false),
            (0, vec![3], false),
        ]
    );
}

/// DISCARD honours `n` like PULL and ends the result like PULL does.
#[test]
fn discard_honours_n_and_its_qid() {
    let mut client = connected_client();
    begin(&mut client);
    run_qid(&mut client, "UNWIND [1, 2, 3] AS a RETURN a");
    run_qid(&mut client, "UNWIND [10, 20] AS b RETURN b");
    let (rows, page) = consume(
        &mut client,
        Message::Discard,
        vec![("n", Pack::Int(2)), ("qid", Pack::Int(0))],
    );
    assert!(rows.is_empty());
    assert_eq!(page, map(vec![("has_more", Pack::Bool(true))]));
    let (_, ending) = consume(&mut client, Message::Discard, vec![("qid", Pack::Int(0))]);
    assert_eq!(ending.get("type"), Some(&Pack::String("r".into())));
    // The other result is untouched.
    let (rows, _) = consume(&mut client, Message::Pull, vec![("qid", Pack::Int(1))]);
    assert_eq!(rows, [10, 20]);
}

/// A qid that names no open result, while another is open, fails with
/// `Neo.ClientError.Request.Invalid` and the failed state; RESET recovers.
#[test]
fn an_unknown_qid_is_a_request_invalid_failure() {
    let mut client = connected_client();
    begin(&mut client);
    run_qid(&mut client, "RETURN 1 AS a");
    send_pull(&mut client, Message::Pull, vec![("qid", Pack::Int(7))]);
    let failure = failure_map(read_msg(&mut client));
    assert_eq!(field(&failure, "code"), REQUEST_INVALID_CODE);
    assert_eq!(field(&failure, "message"), "no open result with qid 7");
    send_run(&mut client, "RETURN 2 AS b");
    assert_ignored(&mut client, "a RUN in the failed state");
    send_reset(&mut client);
    success_map(read_msg(&mut client));
}

/// ADR-0342: a `qid` that is present but not an Int is a request failure
/// like an unknown qid — never the `-1` default, which would silently
/// address the most recent result.
#[test]
fn a_malformed_qid_is_a_request_invalid_failure() {
    let mut client = connected_client();
    begin(&mut client);
    run_qid(&mut client, "RETURN 1 AS a");
    send_pull(
        &mut client,
        Message::Pull,
        vec![("qid", Pack::String("0".into()))],
    );
    let failure = failure_map(read_msg(&mut client));
    assert_eq!(field(&failure, "code"), REQUEST_INVALID_CODE);
    assert_eq!(
        field(&failure, "message"),
        "unsupported extra value for qid: String (must be an Int)"
    );
    send_run(&mut client, "RETURN 2 AS b");
    assert_ignored(&mut client, "a RUN in the failed state");
    send_reset(&mut client);
    success_map(read_msg(&mut client));
}

/// ADR-0342: a `n` that is present but not an Int fails the same way
/// instead of silently draining the result.
#[test]
fn a_malformed_n_is_a_request_invalid_failure() {
    let mut client = connected_client();
    send_run(&mut client, "UNWIND [1, 2] AS x RETURN x");
    success_map(read_msg(&mut client));
    send_pull(&mut client, Message::Pull, vec![("n", Pack::Float(2.5))]);
    let failure = failure_map(read_msg(&mut client));
    assert_eq!(field(&failure, "code"), REQUEST_INVALID_CODE);
    assert_eq!(
        field(&failure, "message"),
        "unsupported extra value for n: Float (must be an Int)"
    );
    // The malformed PULL emitted no RECORD; RESET recovers the session.
    send_reset(&mut client);
    success_map(read_msg(&mut client));
}

/// ADR-0342: a present `n` outside `-1` and the non-negative Ints is
/// malformed too — it must fail, not fall into the `-1` drain. The
/// malformed PULL emits no RECORD, and RESET recovers.
#[test]
fn an_out_of_range_n_is_a_request_invalid_failure() {
    for n in [-5, i64::MIN] {
        let mut client = connected_client();
        send_run(&mut client, "UNWIND [1, 2] AS x RETURN x");
        success_map(read_msg(&mut client));
        send_pull(&mut client, Message::Pull, vec![("n", Pack::Int(n))]);
        let failure = failure_map(read_msg(&mut client));
        assert_eq!(field(&failure, "code"), REQUEST_INVALID_CODE, "n = {n}");
        assert_eq!(
            field(&failure, "message"),
            format!("unsupported extra value for n: {n} (must be -1 or a non-negative Int)"),
            "n = {n}"
        );
        send_reset(&mut client);
        success_map(read_msg(&mut client));
    }
}

/// ADR-0342: `n` above the rows left caps at the rows left, so a huge
/// positive `n` (i64::MAX, above `usize::MAX` on a 32-bit target) drains
/// the result like `-1` — it must neither fail nor panic the connection.
#[test]
fn a_huge_n_drains_the_result() {
    let mut client = connected_client();
    send_run(&mut client, "UNWIND [1, 2, 3] AS x RETURN x");
    success_map(read_msg(&mut client));
    let (rows, ending) = consume(&mut client, Message::Pull, vec![("n", Pack::Int(i64::MAX))]);
    assert_eq!(rows, [1, 2, 3]);
    assert_eq!(ending.get("type"), Some(&Pack::String("r".into())));
}

/// COMMIT drops the results still open; a later PULL finds none.
#[test]
fn commit_drops_every_open_result() {
    let mut client = connected_client();
    begin(&mut client);
    run_qid(&mut client, "RETURN 1 AS a");
    send_commit(&mut client);
    success_map(read_msg(&mut client));
    send_pull(&mut client, Message::Pull, vec![("n", Pack::Int(-1))]);
    let failure = failure_map(read_msg(&mut client));
    assert_eq!(
        field(&failure, "message"),
        "Message of type PullMessage cannot be handled by a session in the READY state."
    );
}

/// Auto-commit keeps one result: no qid, and a new RUN replaces the
/// previous result.
#[test]
fn an_auto_commit_run_replaces_the_previous_result() {
    let mut client = connected_client();
    assert_eq!(run_qid(&mut client, "RETURN 1 AS a"), None);
    assert_eq!(run_qid(&mut client, "RETURN 2 AS b"), None);
    let (records, _) = drain_all(&mut client, Message::Pull);
    assert_eq!(int_column(&records), [2]);
}

/// The result byte cap bounds the sum of every open result on the
/// connection: each RUN fits alone, the second pushes the sum over. The
/// reservation counts the retained RUN statement text (ADR-0357) beside
/// the rows, so both shares appear in the arithmetic.
#[test]
fn the_result_cap_bounds_the_sum_of_open_results() {
    let db = Arc::new(Database::open(":memory:").unwrap());
    // The statement is 54 bytes; its result is one 40-character string,
    // 16 + 40 = 56 logical bytes — one open result reserves 110.
    let config = ConnConfig {
        limits: ConnLimits {
            max_result_bytes: 150,
            ..ConnLimits::default()
        },
        ..ConnConfig::default()
    };
    let mut client = connected_client_with(db, config);
    begin(&mut client);
    let cypher = format!("RETURN '{}' AS s", "x".repeat(40));
    assert_eq!(run_qid(&mut client, &cypher), Some(0));
    send_run(&mut client, &cypher);
    let failure = failure_map(read_msg(&mut client));
    assert_eq!(
        field(&failure, "message"),
        "query result exceeds max-result-bytes limit of 150 bytes"
    );
}

/// ADR-0310, ADR-0342, ADR-0502 §7: an auto-commit RUN whose result
/// exceeds the cap fails, and no SUCCESS carries its bookmark — yet its
/// write already committed in the engine, so the store's commit sequence
/// moved: the next auto-commit write's bookmark is the engine's next
/// value, the one the engine API's own next commit then follows. The
/// over-cap statement here is a write with returned columns, i.e. exactly
/// the shape that would carry a bookmark if it had fit.
#[test]
fn an_over_cap_auto_commit_run_sends_no_bookmark_and_bolt_tracks_the_engine() {
    let db = Arc::new(Database::open(":memory:").unwrap());
    let config = ConnConfig {
        limits: ConnLimits {
            max_result_bytes: 100,
            ..ConnLimits::default()
        },
        ..ConnConfig::default()
    };
    let mut client = connected_client_with(Arc::clone(&db), config);
    // The retained statement is 147 bytes and the result 16 + 120 = 136
    // logical bytes — either share alone exceeds the 100-byte cap.
    send_run(
        &mut client,
        &format!("CREATE (n:N) RETURN '{}' AS s", "x".repeat(120)),
    );
    let failure = failure_map(read_msg(&mut client));
    assert_eq!(
        field(&failure, "message"),
        "query result exceeds max-result-bytes limit of 100 bytes"
    );
    // RESET clears the failed state; a read commits nothing and carries no
    // bookmark.
    send_reset(&mut client);
    success_map(read_msg(&mut client));
    send_run(&mut client, "RETURN 1 AS x");
    success_map(read_msg(&mut client));
    let ending = drain_all(&mut client, Message::Pull).1;
    assert!(ending.get("bookmark").is_none(), "{ending:?}");
    // The over-cap write committed even though its result was dropped.
    let mut engine = db.connect().unwrap();
    let count = engine.query("MATCH (n:N) RETURN count(n) AS c").unwrap();
    assert_eq!(count.rows, vec![vec![relate::Value::Int(1)]]);
    // A successful auto-commit write carries the engine's bookmark, and the
    // engine API's next commit is the value right after it.
    send_run(&mut client, "CREATE (m:M)");
    success_map(read_msg(&mut client));
    let ending = drain_all(&mut client, Message::Pull).1;
    let bolt = bookmark_sequence(&ending, "auto-commit write");
    let next = engine.query("CREATE (:E)").unwrap().bookmark;
    assert_eq!(next, Some(format!("relate:{}", bolt + 1)));
}
