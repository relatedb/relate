//! A streamed RUN's PULL/DISCARD sequence (ADR-0682): the pages come from
//! the statement's own batches, an execution failure lands at the PULL that
//! reached it, DISCARD never encodes the rest, and a stalled client's
//! statement is stopped by the PULL idle timeout.

use std::sync::Arc;
use std::time::Duration;

use relate::{Database, OpenOptions};

use crate::failures::REQUEST_INVALID_CODE;
use crate::packstream::Pack;
use crate::protocol::{ConnConfig, ConnLimits};
use crate::requests::Message;
use crate::testutil::{
    connected_client_with, failure_map, field, pull_ending, read_msg, read_records, send_pull,
    send_run, success_map,
};

fn has_more(m: &Pack) -> bool {
    m.get("has_more") == Some(&Pack::Bool(true))
}

/// A database whose statements share a 16 MiB workspace pool — a pool the
/// two-million-row answer collected could never fit.
fn small_pool_db() -> Arc<Database> {
    Arc::new(
        Database::open_with(
            ":memory:",
            OpenOptions {
                concurrent_logical_workspace_limit: Some(16 << 20),
                ..Default::default()
            },
        )
        .unwrap(),
    )
}

/// The first Int of each record.
fn ints(records: &[Vec<Pack>]) -> Vec<i64> {
    records
        .iter()
        .map(|record| record[0].as_int().expect("an Int column"))
        .collect()
}

/// A two-million-row result is paged out in PULLs under the small pool:
/// every page carries the rows in order, `has_more` stays true until the
/// page that takes the last row, which carries the ending summary.
#[test]
fn a_two_million_row_pull_sequence_passes_under_a_small_pool() {
    let mut client = connected_client_with(small_pool_db(), ConnConfig::default());
    send_run(&mut client, "UNWIND range(1, 2000000) AS i RETURN i");
    let run = Pack::Map(success_map(read_msg(&mut client)));
    assert_eq!(
        run.get("fields"),
        Some(&Pack::List(vec![Pack::String("i".into())]))
    );
    assert_eq!(
        run.get("columnTypes"),
        Some(&Pack::List(vec![Pack::String("INTEGER".into())]))
    );
    let page = 100_000;
    let mut next = 1;
    for pulled in 1..=20 {
        let (records, end) = pull_ending(&mut client, Message::Pull, vec![("n", Pack::Int(page))]);
        assert_eq!(records.len(), page as usize, "PULL {pulled}");
        assert_eq!(ints(&records[..1]), vec![next]);
        assert_eq!(ints(&records[records.len() - 1..]), vec![next + page - 1]);
        next += page;
        assert_eq!(has_more(&end), pulled < 20, "PULL {pulled}: {end:?}");
        if pulled == 20 {
            assert_eq!(end.get("type"), Some(&Pack::String("r".into())));
        }
    }
}

/// DISCARD of the rest of a streamed result answers its ending summary
/// without a single RECORD, and the session serves the next statement.
#[test]
fn discard_drops_the_rest_of_a_streamed_result() {
    let mut client = connected_client_with(small_pool_db(), ConnConfig::default());
    // DISCARD lets the statement run on without encoding rows, so the
    // statement's own run time bounds this test: many batches, not millions.
    send_run(&mut client, "UNWIND range(1, 200000) AS i RETURN i");
    success_map(read_msg(&mut client));
    let (records, end) = pull_ending(&mut client, Message::Pull, vec![("n", Pack::Int(5))]);
    assert_eq!(ints(&records), vec![1, 2, 3, 4, 5]);
    assert!(has_more(&end));
    let (records, end) = pull_ending(&mut client, Message::Discard, vec![("n", Pack::Int(-1))]);
    assert!(records.is_empty());
    assert!(!has_more(&end), "{end:?}");
    assert_eq!(end.get("type"), Some(&Pack::String("r".into())));
    send_run(&mut client, "RETURN 1 AS x");
    success_map(read_msg(&mut client));
}

/// A RUN that follows an unfinished streamed result replaces it: the
/// statement is stopped, the connection is taken back, and the new result
/// answers.
#[test]
fn a_new_run_replaces_an_unfinished_streamed_result() {
    let mut client = connected_client_with(small_pool_db(), ConnConfig::default());
    send_run(&mut client, "UNWIND range(1, 2000000) AS i RETURN i");
    success_map(read_msg(&mut client));
    let (records, end) = pull_ending(&mut client, Message::Pull, vec![("n", Pack::Int(3))]);
    assert_eq!(records.len(), 3);
    assert!(has_more(&end));
    send_run(&mut client, "UNWIND range(1, 4) AS j RETURN j");
    success_map(read_msg(&mut client));
    let (records, end) = pull_ending(&mut client, Message::Pull, vec![("n", Pack::Int(-1))]);
    assert_eq!(ints(&records), vec![1, 2, 3, 4]);
    assert!(!has_more(&end));
}

/// An execution error mid-stream is the FAILURE of the PULL that reached
/// it (ADR-0319, ADR-0682): the rows before it are delivered, then the
/// FAILURE, and the session is failed until RESET.
#[test]
fn a_failure_after_the_first_batch_lands_at_the_pull_that_reached_it() {
    let mut client = connected_client_with(small_pool_db(), ConnConfig::default());
    // Row 5000 divides by zero: four full batches of 1024 precede the
    // batch that cannot be produced.
    let statement = "UNWIND range(1, 5000) AS i RETURN 100 / (5000 - i) AS v";
    send_run(&mut client, statement);
    success_map(read_msg(&mut client));
    let mut delivered = 0;
    loop {
        send_pull(&mut client, Message::Pull, vec![("n", Pack::Int(1024))]);
        let (records, last) = read_records(&mut client);
        delivered += records.len();
        match last {
            Pack::Struct { tag: 0x70, .. } => {
                assert!(has_more(&Pack::Map(success_map(last))), "no ending here");
            }
            failure => {
                let failure = failure_map(failure);
                assert_eq!(field(&failure, "code"), relate_error::ARITHMETIC_ERROR_CODE);
                break;
            }
        }
    }
    assert_eq!(delivered, 4 * 1024, "the batches before the failing one");
    send_pull(&mut client, Message::Pull, vec![]);
    crate::testutil::assert_ignored(&mut client, "a PULL after the failed result");
}

/// A client that stops PULLing does not park the statement forever: the
/// PULL idle timeout stops it, rows already delivered still page out, and
/// the PULL that needs more answers a timeout FAILURE.
#[test]
fn the_pull_idle_timeout_ends_a_stalled_statement() {
    let config = ConnConfig {
        limits: ConnLimits {
            pull_idle_timeout: Some(Duration::from_millis(100)),
            ..ConnLimits::default()
        },
        ..Default::default()
    };
    let mut client = connected_client_with(small_pool_db(), config);
    send_run(&mut client, "UNWIND range(1, 2000000) AS i RETURN i");
    success_map(read_msg(&mut client));
    let (records, end) = pull_ending(&mut client, Message::Pull, vec![("n", Pack::Int(10))]);
    assert_eq!(records.len(), 10);
    assert!(has_more(&end));
    std::thread::sleep(Duration::from_millis(600));
    send_pull(&mut client, Message::Pull, vec![("n", Pack::Int(-1))]);
    let (records, failure) = read_records(&mut client);
    assert_eq!(records.len(), 1024 - 10, "the batch in hand pages out");
    let failure = failure_map(failure);
    assert_eq!(
        field(&failure, "code"),
        relate_error::TRANSACTION_TIMED_OUT_CODE
    );
    assert_ne!(field(&failure, "code"), REQUEST_INVALID_CODE);
}
