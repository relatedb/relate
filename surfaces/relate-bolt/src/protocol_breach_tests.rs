//! Protocol breaches (ADR-0314) and unknown message signatures
//! (ADR-0315): what closes the connection, and with which reply.

use std::io::Write;
use std::sync::Arc;

use super::*;
use crate::failures::UNAUTHORIZED_CODE;
use crate::packstream::Pack;
use crate::requests::Message;
use crate::testutil::{
    assert_breach, assert_breach_failure, assert_closes, assert_ignored, connected_client,
    failure_map, field, handshaken, map, read_msg, read_records, send_begin, send_commit,
    send_pull, send_reset, send_run, send_struct, success_map,
};

/// F-32, F-43 / F-33, F-44: COMMIT or ROLLBACK with no transaction.
#[test]
fn commit_or_rollback_without_begin_is_a_breach() {
    for message in [Message::Commit, Message::Rollback] {
        let mut client = connected_client();
        assert_breach(&mut client, message, vec![], "READY");
    }
}

/// F-34, F-45: BEGIN inside a transaction, with and without open results.
#[test]
fn begin_inside_a_transaction_is_a_breach() {
    let mut client = connected_client();
    send_begin(&mut client, vec![]);
    success_map(read_msg(&mut client));
    send_run(&mut client, "RETURN 1 AS x");
    success_map(read_msg(&mut client));
    send_begin(&mut client, vec![]);
    assert_breach_failure(&mut client, Message::Begin, "TX_STREAMING");
}

/// F-35, F-46: PULL or DISCARD with no open result, outside and inside a
/// transaction; never answered SUCCESS.
#[test]
fn pull_or_discard_without_a_result_is_a_breach() {
    for message in [Message::Pull, Message::Discard] {
        let mut client = connected_client();
        send_pull(&mut client, message, vec![]);
        assert_breach_failure(&mut client, message, "READY");

        let mut client = connected_client();
        send_begin(&mut client, vec![]);
        success_map(read_msg(&mut client));
        send_pull(&mut client, message, vec![]);
        assert_breach_failure(&mut client, message, "IN_TRANSACTION");
    }
    // A result that was consumed to its end is closed too.
    let mut client = connected_client();
    send_run(&mut client, "RETURN 1 AS x");
    success_map(read_msg(&mut client));
    send_pull(&mut client, Message::Pull, vec![("n", Pack::Int(-1))]);
    read_records(&mut client);
    send_pull(&mut client, Message::Pull, vec![]);
    assert_breach_failure(&mut client, Message::Pull, "READY");
}

/// The state is the session's real Neo4j state: COMMIT with an
/// auto-commit result open is in STREAMING.
#[test]
fn commit_with_an_open_auto_commit_result_names_streaming() {
    let mut client = connected_client();
    send_run(&mut client, "RETURN 1 AS x");
    success_map(read_msg(&mut client));
    send_commit(&mut client);
    assert_breach_failure(&mut client, Message::Commit, "STREAMING");
}

/// The failed-state gate runs first: in the failed state these messages
/// are IGNORED, not breaches, and RESET recovers.
#[test]
fn the_failed_state_ignores_instead_of_breaching() {
    let mut client = connected_client();
    send_run(&mut client, "RETURN");
    failure_map(read_msg(&mut client));
    for (message, fields) in [
        (Message::Commit, vec![]),
        (Message::Rollback, vec![]),
        (Message::Pull, vec![map(vec![])]),
        (Message::Discard, vec![map(vec![])]),
    ] {
        send_struct(&mut client, message.signature(), fields);
        assert_ignored(
            &mut client,
            &format!("{message:?} is IGNORED in the failed state"),
        );
    }
    send_reset(&mut client);
    success_map(read_msg(&mut client));
}

/// F-31: an unknown signature (the Bolt 1-2 ACK_FAILURE 0x0E) or a message
/// that is not a structure closes the connection without a FAILURE.
#[test]
fn an_unknown_signature_or_a_non_structure_closes_without_a_reply() {
    for message in [
        crate::packstream::struct_msg(0x0E, vec![]),
        crate::packstream::chunk_message(&crate::packstream::encode(&Pack::Int(1)).unwrap()),
    ] {
        let mut client = connected_client();
        client.write_all(&message).unwrap();
        assert_closes(&mut client, "an undecodable request closes at once");
    }
}

/// ADR-0484: at 4.4 the 5.x session messages — LOGON, LOGOFF and
/// TELEMETRY — stay fatal unknowns (ADR-0315): no FAILURE, the close,
/// byte-identical to any other unlisted signature. The messages iterate
/// the vocabulary and their signatures its one table, like the sibling
/// decode test below.
#[test]
fn the_5x_signatures_close_without_a_reply_at_4_4() {
    for message in [Message::Logon, Message::Logoff, Message::Telemetry] {
        let mut client = connected_client();
        send_struct(&mut client, message.signature(), vec![map(vec![])]);
        assert_closes(
            &mut client,
            &format!("{message:?} is a fatal unknown at 4.4"),
        );
    }
}

/// ADR-0484: the same signatures decode at 5.4 — LOGON, LOGOFF and
/// TELEMETRY reach the session's state machine and are answered by it,
/// never the silent ADR-0315 close a 4.4 connection answers. While the
/// session awaits LOGON every message but LOGON is the ADR-0314 breach
/// naming the AUTHENTICATION state; this pins it for LOGOFF and TELEMETRY
/// (the LOGON flow itself, and the two messages from an open session, are
/// pinned by the logon and telemetry modules).
#[test]
fn the_5x_signatures_decode_at_5_4() {
    for (message, fields) in [
        (Message::Logoff, vec![]),
        (Message::Telemetry, vec![Pack::Int(0)]),
    ] {
        let mut client = crate::testutil::awaiting_logon_client(ConnConfig::default());
        assert_breach(&mut client, message, fields, "AUTHENTICATION");
    }
}

/// ADR-0315 item 2: before a validated HELLO with auth on, the ADR-0126
/// gate answers first — Unauthorized, then the close.
#[test]
fn the_auth_gate_answers_an_unknown_signature_before_hello() {
    let config = ConnConfig {
        auth: Arc::new(crate::auth::AuthConfig::basic("relate", "token")),
        ..ConnConfig::default()
    };
    let mut client = handshaken(config);
    send_struct(&mut client, 0x0E, vec![]);
    let failure = failure_map(read_msg(&mut client));
    assert_eq!(field(&failure, "code"), UNAUTHORIZED_CODE);
    assert_closes(&mut client, "the auth gate closes");
}
