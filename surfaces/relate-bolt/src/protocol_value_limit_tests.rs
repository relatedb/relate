//! A well-formed message over the decoded-size budget fails
//! its request with an actionable FAILURE and leaves the connection open,
//! instead of closing it (which a driver retries as a transient failure).

use crate::failures::{decode_budget_message, REQUEST_INVALID_CODE};
use crate::packstream::{decode_message, encode, DecodeError, Pack, MAX_DECODE_BYTES, NODE_BYTES};
use crate::protocol::ConnConfig;
use crate::requests::Message;
use crate::testutil::{
    assert_closes, assert_ignored, awaiting_logon_client, connected_client, failure_map, field,
    handshaken_at, read_msg, read_records, send_pull, send_run, send_struct, success_map,
};
use crate::version::BoltVersion;
use std::io::Write;

/// A RUN whose `rows` parameter is a list of 3-field maps (about 230 decoded
/// bytes per row), the shape the LABS-1412 notebook sends.
fn run_with_rows(rows: usize) -> Vec<Pack> {
    let row = Pack::Map(
        ["a", "b", "c"]
            .into_iter()
            .map(|k| (Pack::String(k.into()), Pack::Int(1)))
            .collect(),
    );
    vec![
        Pack::String("UNWIND $rows AS r RETURN count(*) AS n".into()),
        Pack::Map(vec![(
            Pack::String("rows".into()),
            Pack::List(vec![row; rows]),
        )]),
        Pack::Map(vec![]),
    ]
}

#[test]
fn an_over_budget_run_fails_with_an_actionable_message_and_the_session_continues() {
    let mut client = connected_client();
    send_struct(
        &mut client,
        Message::Run.signature(),
        run_with_rows(320_000),
    );
    let failure = failure_map(read_msg(&mut client));
    assert_eq!(field(&failure, "code"), REQUEST_INVALID_CODE);
    let message = field(&failure, "message");
    for needle in [
        "MiB of values",
        "limit of 64 MiB",
        "per-message limit",
        "Send fewer rows per request",
    ] {
        assert!(
            message.contains(needle),
            "{needle:?} missing from: {message}"
        );
    }

    // The failed state ignores the follow-up PULL (a pipelined driver sends
    // one), then RESET recovers and the same connection serves a query.
    send_pull(&mut client, Message::Pull, vec![]);
    assert_ignored(&mut client, "PULL after the oversize RUN is IGNORED");
    send_struct(&mut client, Message::Reset.signature(), vec![]);
    success_map(read_msg(&mut client));
    send_run(&mut client, "RETURN 1 AS x");
    success_map(read_msg(&mut client));
    send_pull(&mut client, Message::Pull, vec![("n", Pack::Int(-1))]);
    assert_eq!(read_records(&mut client).0.len(), 1);
}

#[test]
fn the_notebook_batch_shape_runs() {
    // 133k rows of 3 fields in one RUN — about 930k values, the batch the
    // former 262,144-value count refused — now fits the byte budget.
    let mut client = connected_client();
    send_struct(
        &mut client,
        Message::Run.signature(),
        run_with_rows(133_000),
    );
    success_map(read_msg(&mut client));
}

#[test]
fn the_decoder_reports_the_budget_as_its_own_typed_error() {
    let fits = MAX_DECODE_BYTES / NODE_BYTES;
    let over = encode(&Pack::List(vec![Pack::Null; fits])).unwrap();
    match decode_message(&over) {
        Err(DecodeError::BudgetExceeded { at_least_bytes }) => {
            assert!(at_least_bytes > MAX_DECODE_BYTES)
        }
        other => panic!("expected the decode budget, got {other:?}"),
    }
    assert!(matches!(
        decode_message(&[0xC4]),
        Err(DecodeError::Malformed(_))
    ));
}

#[test]
fn the_message_names_the_measured_size_the_limit_and_the_remedy() {
    let text = decode_budget_message(70 * 1024 * 1024 + 1, 6_201_334);
    assert!(text.contains("6201334 bytes"), "{text}");
    assert!(text.contains("at least 70 MiB"), "{text}");
    assert!(text.contains("limit of 64 MiB"), "{text}");
    assert!(text.contains("UNWIND"), "{text}");
}

#[test]
fn before_hello_an_over_budget_message_is_the_unauthorized_close() {
    // ADR-0126: the pre-HELLO gate (at 5.4 it is always armed, ADR-0484) answers every message but HELLO with the
    // unauthorized FAILURE, an over-budget one included.
    let mut client = handshaken_at(ConnConfig::default(), BoltVersion::V5_4);
    send_struct(
        &mut client,
        Message::Run.signature(),
        run_with_rows(320_000),
    );
    let failure = failure_map(read_msg(&mut client));
    assert_eq!(field(&failure, "code"), crate::failures::UNAUTHORIZED_CODE);
    assert_closes(&mut client, "closes after the pre-HELLO refusal");
}

#[test]
fn in_the_authentication_state_an_over_budget_message_fails_and_closes() {
    // ADR-0484 decision 4: only LOGON is accepted, anything else is this
    // gate's breach; the over-budget text names the limit.
    let mut client = awaiting_logon_client(ConnConfig::default());
    send_struct(
        &mut client,
        Message::Run.signature(),
        run_with_rows(320_000),
    );
    let failure = failure_map(read_msg(&mut client));
    assert_eq!(field(&failure, "code"), REQUEST_INVALID_CODE);
    assert!(field(&failure, "message").contains("per-message limit"));
    assert_closes(&mut client, "closes after the AUTHENTICATION-state refusal");
}

#[test]
fn an_over_budget_message_queued_ahead_of_a_reset_is_ignored() {
    // ADR-0317: one batch, the oversize RUN ahead of the RESET — IGNORED,
    // then the RESET's SUCCESS, and the session is not failed.
    let mut client = connected_client();
    let mut batch =
        crate::packstream::try_struct_msg(Message::Run.signature(), run_with_rows(320_000))
            .unwrap();
    batch.extend(crate::packstream::try_struct_msg(Message::Reset.signature(), vec![]).unwrap());
    client.write_all(&batch).unwrap();
    assert_ignored(&mut client, "the oversize RUN queued ahead of RESET");
    success_map(read_msg(&mut client));
    send_run(&mut client, "RETURN 1 AS x");
    success_map(read_msg(&mut client));
}
