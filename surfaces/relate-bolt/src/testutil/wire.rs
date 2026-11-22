//! The wire half of the test helpers: the client-side senders, readers and
//! the reply assertions every Bolt test module shares. The fixtures that
//! spawn servers and open sessions live in the parent module.

use std::io::{Read, Write};
use std::net::TcpStream;

use super::*;
use crate::packstream::Pack;
use crate::version::BoltVersion;

// The Bolt handshake helpers are type-free (generic over the stream),
// so their canonical home is the shared test-support crate
// (`relate_test_support::bolt`) and they are re-exported here exactly
// like `upgrade_request` below — `handshake_at` negotiates either spoken
// version (ADR-0484), `handshake` its 4.4 spelling. The Pack-carrying
// helpers cannot share that home: this module is `#[cfg(test)]`, a unit
// test builds its own crate instance, and a `Pack` from the dependency
// instance test-support links against is a different type (measured:
// E0308). External integration tests (the CLI's e2e_serve) therefore use
// `relate_test_support::bolt` directly for the Pack-carrying helpers.
pub(crate) use relate_test_support::bolt::{handshake, handshake_at, try_handshake};

/// Write one struct message to the client; panics on failure.
pub(crate) fn send_struct<S: Write>(client: &mut S, tag: u8, fields: Vec<Pack>) {
    client
        .write_all(&crate::packstream::try_struct_msg(tag, fields).unwrap())
        .unwrap();
    client.flush().unwrap();
}

/// Extract the metadata map of a FAILURE message; panics otherwise.
pub(crate) fn failure_map(msg: Pack) -> Pack {
    let Pack::Struct { tag: 0x7F, fields } = msg else {
        panic!("expected FAILURE, got {msg:?}");
    };
    let Some(m @ Pack::Map(_)) = fields.into_iter().next() else {
        panic!("expected FAILURE map");
    };
    m
}

/// Read the next message as a ROUTE SUCCESS and return its `rt` routing
/// table (ADR-0269): the one extraction the route and namespace tests share.
pub(crate) fn read_route_rt<S: Read>(client: &mut S) -> Pack {
    let reply = Pack::Map(success_map(read_msg(client)));
    reply
        .get("rt")
        .expect("ROUTE SUCCESS must carry an rt map")
        .clone()
}

/// Extract a string field from a Pack map; panics when absent or not a
/// string. The lookup itself is the canonical [`Pack::get`].
pub(crate) fn field<'a>(m: &'a Pack, key: &str) -> &'a str {
    m.get(key)
        .and_then(Pack::as_str)
        .unwrap_or_else(|| panic!("missing field {key}"))
}

/// Read one chunked Bolt message and decode it.
///
/// Delegates the chunk loop to [`crate::protocol::read_message`] so tests
/// inherit its 16 MiB message cap.
pub(crate) fn try_read_msg<S: Read>(client: &mut S) -> std::io::Result<Pack> {
    let payload = crate::protocol::read_message(client)?;
    crate::packstream::decode(&payload)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Read one chunked Bolt message and decode it; panics on failure.
pub(crate) fn read_msg<S: Read>(client: &mut S) -> Pack {
    try_read_msg(client).unwrap()
}

/// Extract the metadata map of a SUCCESS message; panics otherwise.
pub(crate) fn success_map(msg: Pack) -> Vec<(Pack, Pack)> {
    let Pack::Struct { tag: 0x70, fields } = msg else {
        panic!("expected SUCCESS, got {msg:?}");
    };
    let Some(Pack::Map(m)) = fields.into_iter().next() else {
        panic!("expected SUCCESS map");
    };
    m
}

// The canonical definition lives in `crate::websocket` (the module the
// request describes) so the e2e crates can reach it; re-exported here for
// the in-crate unit tests that import it through this module.
/// Raw HTTP WebSocket upgrade request bytes: the one definition lives in
/// the shared test-support crate; re-exported so the in-crate tests keep
/// their `testutil::upgrade_request` path.
pub(crate) use relate_test_support::ws_upgrade_request as upgrade_request;

/// A full HELLO / `RUN RETURN 1 AS n` / PULL exchange over a Bolt byte
/// stream; asserts the RECORD carries `1`.
pub(crate) fn return_one_exchange<S: Read + Write>(client: &mut S) {
    handshake(client);
    send_hello_at(client, BoltVersion::V4_4, vec![]);
    send_run_legacy(client, "RETURN 1 AS n");
    success_map(read_msg(client));
    send_pull(
        client,
        crate::requests::Message::Pull,
        vec![("n", Pack::Int(-1))],
    );
    assert_eq!(
        read_msg(client),
        Pack::Struct {
            tag: 0x71,
            fields: vec![Pack::List(vec![Pack::Int(1)])],
        }
    );
    success_map(read_msg(client));
}

/// String-keyed entries as Pack map entries: the one key-wrapping policy
/// [`map`] and the HELLO extra family share.
pub(crate) fn pack_entries(entries: Vec<(&str, Pack)>) -> Vec<(Pack, Pack)> {
    entries
        .into_iter()
        .map(|(key, value)| (Pack::String(key.into()), value))
        .collect()
}

/// A Pack map from string keys.
pub(crate) fn map(entries: Vec<(&str, Pack)>) -> Pack {
    Pack::Map(pack_entries(entries))
}

/// Send RUN `cypher` with empty params in the legacy two-field wire form —
/// no extra field at all (Bolt before 4.3; ADR-0484): the one spelling of
/// the absent-extra form, and the server accepts it at every spoken
/// version. The extra-carrying form is [`send_run_with`].
pub(crate) fn send_run_legacy<S: Write>(client: &mut S, cypher: &str) {
    send_struct(
        client,
        crate::requests::Message::Run.signature(),
        vec![Pack::String(cypher.into()), Pack::Map(vec![])],
    );
}

/// Send RUN `cypher` with empty params and `entries` as its extra map — the
/// three-field form (Bolt 4.3+; ADR-0484): the one RUN sender that carries
/// an extra map (the 4.4 `db` routing field). The absent-extra form is
/// [`send_run_legacy`].
pub(crate) fn send_run_with<S: Write>(client: &mut S, cypher: &str, entries: Vec<(&str, Pack)>) {
    send_struct(
        client,
        crate::requests::Message::Run.signature(),
        vec![Pack::String(cypher.into()), Pack::Map(vec![]), map(entries)],
    );
}

/// Send RUN `cypher` with empty params and an empty extra map — the
/// three-field form, [`send_run_with`] with `vec![]`: the suite's
/// historical spelling, byte-identical to the form it has always sent (the
/// server accepts the extra field at every spoken version).
pub(crate) fn send_run<S: Write>(client: &mut S, cypher: &str) {
    send_run_with(client, cypher, vec![]);
}

/// Send BEGIN with a caller-supplied extra map.
pub(crate) fn send_begin<S: Write>(client: &mut S, entries: Vec<(&str, Pack)>) {
    send_struct(
        client,
        crate::requests::Message::Begin.signature(),
        vec![map(entries)],
    );
}

/// Send COMMIT — the transaction-ending message with no fields.
pub(crate) fn send_commit<S: Write>(client: &mut S) {
    send_struct(client, crate::requests::Message::Commit.signature(), vec![]);
}

/// Send ROLLBACK — the transaction-ending message with no fields.
pub(crate) fn send_rollback<S: Write>(client: &mut S) {
    send_struct(
        client,
        crate::requests::Message::Rollback.signature(),
        vec![],
    );
}

/// Send PULL or DISCARD (`message`) with the extra `entries` (`n`, `qid`).
pub(crate) fn send_pull<S: Write>(
    client: &mut S,
    message: crate::requests::Message,
    entries: Vec<(&str, Pack)>,
) {
    send_struct(client, message.signature(), vec![map(entries)]);
}

/// Send LOGON with `entries` as its auth map.
pub(crate) fn send_logon<S: Write>(client: &mut S, entries: Vec<(&str, Pack)>) {
    send_struct(
        client,
        crate::requests::Message::Logon.signature(),
        vec![map(entries)],
    );
}

/// Send ROUTE (ADR-0269): the routing-context map, the empty bookmark
/// list, and the extra map — the one spelling of the three-field wire
/// form the route tests share.
pub(crate) fn send_route<S: Write>(
    client: &mut S,
    routing: Vec<(&str, Pack)>,
    extra: Vec<(&str, Pack)>,
) {
    send_struct(
        client,
        crate::requests::Message::Route.signature(),
        vec![map(routing), Pack::List(vec![]), map(extra)],
    );
}

/// Send RESET — the field-less message every failed-state recovery sends.
pub(crate) fn send_reset<S: Write>(client: &mut S) {
    send_struct(client, crate::requests::Message::Reset.signature(), vec![]);
}

/// Send LOGOFF — the 5.x session message with no fields.
pub(crate) fn send_logoff<S: Write>(client: &mut S) {
    send_struct(client, crate::requests::Message::Logoff.signature(), vec![]);
}

/// Send TELEMETRY with `api` as its one field — `None` the absent form,
/// the malformed-`api` table's row axis (the logon module's
/// `Option<Pack>` row shape for a missing field).
pub(crate) fn send_telemetry<S: Write>(client: &mut S, api: Option<Pack>) {
    send_struct(
        client,
        crate::requests::Message::Telemetry.signature(),
        api.into_iter().collect(),
    );
}

/// One PULL-or-DISCARD drain with a caller's `extra` entries: send the
/// message, read the RECORDs, and return them beside the ending SUCCESS
/// map — the one drain spelling every Bolt test module reads (the ending
/// SUCCESS carries the result summary, ADR-0309/ADR-0357). The records
/// come back in [`read_records`]' unwrapped shape — each record the
/// drained values, so callers read `record[0]` directly — and the ending
/// is the SUCCESS map itself, unwrapped the one way [`success_map`]
/// unwraps it.
pub(crate) fn pull_ending<S: Read + Write>(
    client: &mut S,
    message: crate::requests::Message,
    extra: Vec<(&str, Pack)>,
) -> (Vec<Vec<Pack>>, Pack) {
    send_pull(client, message, extra);
    let (records, ending) = read_records(client);
    (records, Pack::Map(success_map(ending)))
}

/// The plain full drain (`n = -1`) — the thin convenience beside
/// [`pull_ending`] for the common case, spelled once here.
pub(crate) fn drain_all<S: Read + Write>(
    client: &mut S,
    message: crate::requests::Message,
) -> (Vec<Vec<Pack>>, Pack) {
    pull_ending(client, message, vec![("n", Pack::Int(-1))])
}

/// Read RECORDs until the first other message: each RECORD's values, and
/// that message.
pub(crate) fn read_records<S: Read>(client: &mut S) -> (Vec<Vec<Pack>>, Pack) {
    let mut records = Vec::new();
    loop {
        match read_msg(client) {
            Pack::Struct { tag: 0x71, fields } => {
                let Some(Pack::List(values)) = fields.into_iter().next() else {
                    panic!("a RECORD carries one list");
                };
                records.push(values);
            }
            other => return (records, other),
        }
    }
}

/// Assert the next message is IGNORED (`0x7E`, no fields): the one
/// spelling of the "reply is IGNORED" policy (ADR-0316, ADR-0319).
pub(crate) fn assert_ignored<S: Read>(client: &mut S, why: &str) {
    assert_eq!(
        read_msg(client),
        Pack::Struct {
            tag: 0x7E,
            fields: vec![],
        },
        "{why}"
    );
}

/// Send `message` with `fields` and assert the one ADR-0314 breach FAILURE
/// naming it in `state`, then the close: the shared spelling of the
/// breach-reply policy (the failed-state and 5.x AUTHENTICATION gates
/// answer through it too). The wire tag comes from the vocabulary's
/// signature table and the expected sentence from its class name — the
/// golden text stays hand-written here so a drift in `breach_text`
/// itself cannot pass this pin.
pub(crate) fn assert_breach(
    client: &mut TcpStream,
    message: crate::requests::Message,
    fields: Vec<Pack>,
    state: &str,
) {
    send_struct(client, message.signature(), fields);
    assert_breach_failure(client, message, state);
}

/// The breach FAILURE + close half of [`assert_breach`], for the sites
/// whose send goes through a canonical sender (`send_run_at` and
/// friends) instead of plain fields: the caller sends, this asserts the
/// one ADR-0314 FAILURE naming the message in `state`, then the close.
pub(crate) fn assert_breach_failure(
    client: &mut TcpStream,
    message: crate::requests::Message,
    state: &str,
) {
    let class = message.class_name();
    let failure = failure_map(read_msg(client));
    assert_eq!(
        field(&failure, "code"),
        crate::failures::REQUEST_INVALID_CODE,
        "{class} in {state}"
    );
    assert_eq!(
        field(&failure, "message"),
        format!("Message of type {class} cannot be handled by a session in the {state} state.")
    );
    assert_closes(client, &format!("{class} in {state} closes the connection"));
}

/// The failed-state ignore-then-recover exchange (ADR-0110, this
/// feature's contract for the 5.x session messages): a failing RUN
/// degrades the session, `message` answers IGNORED, RESET clears the
/// state, and `message` answers SUCCESS `{}` again — the one spelling
/// the logon and telemetry modules share over the messages the failed
/// state IGNOREs.
pub(crate) fn assert_ignored_in_failed_state_then_recovers<S: Read + Write>(
    client: &mut S,
    message: crate::requests::Message,
    fields: Vec<Pack>,
) {
    send_run(client, "RETURN");
    failure_map(read_msg(client));
    send_struct(client, message.signature(), fields.clone());
    assert_ignored(
        client,
        &format!("{} in the failed state", message.class_name()),
    );
    send_reset(client);
    success_map(read_msg(client));
    send_struct(client, message.signature(), fields);
    assert_eq!(success_map(read_msg(client)), vec![]);
}

/// The keys of a metadata map, in wire order.
pub(crate) fn keys(metadata: &[(Pack, Pack)]) -> Vec<&str> {
    metadata
        .iter()
        .map(|(key, _)| key.as_str().expect("metadata keys are strings"))
        .collect()
}

/// The `relate:<n>` sequence number of a SUCCESS map's bookmark —
/// the crate's one test-side parse of the ADR-0310 form.
pub(crate) fn bookmark_sequence(success: &Pack, label: &str) -> u64 {
    let Some(bookmark) = success.get("bookmark").and_then(Pack::as_str) else {
        panic!("{label}: expected a bookmark: {success:?}");
    };
    let Some(number) = bookmark.strip_prefix("relate:") else {
        panic!("{label}: not the ADR-0310 form: {bookmark}");
    };
    number
        .parse()
        .unwrap_or_else(|_| panic!("{label}: not a sequence number: {bookmark}"))
}
