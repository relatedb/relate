//! ADR-0610's Bolt transport channel in its own integration-test binary:
//! a BEGIN's `tx_metadata` extra seeds the connection's per-transaction
//! metadata map, the transaction's committed write carries it into its
//! retained record, and a malformed extra fails the BEGIN before
//! anything starts.
//!
//! The suite is process-isolated from the crate's unit-test binary on
//! purpose: its stores open under exactly the `value-provenance` opt-in
//! (`preview_value_provenance`), while the unit binary's ontology-gated
//! suites keep their own — one binary, one preview set, because the
//! opt-in is first-call-wins per process and a shared binary would
//! couple the two. The wire exchange is hand-spelled through the crate's
//! public `packstream`/`protocol`/`transport` modules (the in-crate
//! `testutil` is `#[cfg(test)]` and stays the unit binary's), over a
//! one-connection `serve_conn` thread carrying the engine query
//! worker's own stack budget — the same production-shaped thread
//! `testutil::spawn_handler` runs, mirrored here through the public
//! surface. The bookmark the COMMIT SUCCESS returns is decoded by the
//! engine crate's one doc-hidden decoder, never re-inlined.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use relate::Database;
use relate_bolt::packstream::{decode, try_struct_msg, Pack};
use relate_bolt::protocol::{read_message, ConnConfig};
use relate_bolt::transport::{serve_conn, Transport};

/// The fixture store, opened under this binary's one preview opt-in —
/// the whole point of the split: this set, and only this set.
fn memory_db() -> Arc<Database> {
    relate_test_support::preview::preview_value_provenance();
    Arc::new(Database::open(":memory:").unwrap())
}

/// Spawn the one-connection server thread on the fixture's store and
/// return the connected client, past nothing: the caller negotiates the
/// handshake and HELLO through [`connected_client`] or by hand.
fn spawn_session(db: Arc<Database>) -> TcpStream {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let transport = Transport {
        tls: None,
        plaintext_allowed: true,
        websocket: None,
    };
    std::thread::Builder::new()
        .stack_size(relate::QUERY_WORKER_STACK_BYTES)
        .spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            // The serve result is the session's own report; these tests
            // read the session through the client, so it is dropped.
            let _ = serve_conn(stream, &transport, db, ConnConfig::default());
        })
        .expect("spawn the fixture's connection thread");
    let client = TcpStream::connect(addr).unwrap();
    // A wedged server fails the test instead of hanging the suite —
    // `testutil`'s own read guard.
    client
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    client
}

/// Connect, negotiate Bolt 4.4 and complete a plain HELLO (the 4.4
/// required `user_agent`, ADR-0313): the ready session every test below
/// drives.
fn connected_client(db: Arc<Database>) -> TcpStream {
    let mut client = spawn_session(db);
    relate_test_support::bolt::handshake(&mut client);
    send(
        &mut client,
        0x01,
        vec![Pack::Map(vec![(
            Pack::String("user_agent".into()),
            Pack::String("relate-bolt-tx-metadata/1.0".into()),
        )])],
    );
    assert!(
        matches!(message(&mut client), Pack::Struct { tag: 0x70, .. }),
        "HELLO must succeed"
    );
    client
}

/// One chunk-framed request from `fields` (the message tag's
/// PackStream spelling, the same bytes the crate's own request table
/// writes).
fn send<S: Write>(client: &mut S, tag: u8, fields: Vec<Pack>) {
    client
        .write_all(&try_struct_msg(tag, fields).unwrap())
        .unwrap();
}

/// One response message, decoded.
fn message<R: Read>(client: &mut R) -> Pack {
    decode(&read_message(client).unwrap()).unwrap()
}

/// The `(&str, Pack)` pairs a message's extra map carries, as one
/// `Pack::Map`.
fn pack_entries(entries: Vec<(&str, Pack)>) -> Pack {
    Pack::Map(
        entries
            .into_iter()
            .map(|(key, value)| (Pack::String(key.into()), value))
            .collect(),
    )
}

/// BEGIN (0x11) with a caller-supplied extra map.
fn send_begin(client: &mut TcpStream, entries: Vec<(&str, Pack)>) {
    send(client, 0x11, vec![pack_entries(entries)]);
}

/// RUN (0x10) `cypher` with empty params and an empty extra map — the
/// three-field form the crate's own client helpers send.
fn send_run(client: &mut TcpStream, cypher: &str) {
    send(
        client,
        0x10,
        vec![
            Pack::String(cypher.into()),
            Pack::Map(vec![]),
            Pack::Map(vec![]),
        ],
    );
}

/// PULL (0x3F) with the plain `n = -1` drain.
fn send_pull(client: &mut TcpStream) {
    send(
        client,
        0x3F,
        vec![Pack::Map(vec![(Pack::String("n".into()), Pack::Int(-1))])],
    );
}

/// COMMIT (0x12) — the transaction-ending message with no fields.
fn send_commit(client: &mut TcpStream) {
    send(client, 0x12, vec![]);
}

/// RESET (0x0F).
fn send_reset(client: &mut TcpStream) {
    send(client, 0x0F, vec![]);
}

/// The next message, asserted SUCCESS: its one map field, the summary
/// every caller reads (the bookmark, the `type`/`has_more` facts).
fn success_map(client: &mut TcpStream) -> Vec<(Pack, Pack)> {
    match message(client) {
        Pack::Struct { tag: 0x70, fields } => map_field(fields, "SUCCESS"),
        other => panic!("expected SUCCESS, got {other:?}"),
    }
}

/// The next message, asserted FAILURE: its one map field (the `code`
/// and `message` the assertions read).
fn failure_map(client: &mut TcpStream) -> Vec<(Pack, Pack)> {
    match message(client) {
        Pack::Struct { tag: 0x7F, fields } => map_field(fields, "FAILURE"),
        other => panic!("expected FAILURE, got {other:?}"),
    }
}

/// A summary message's one map field, asserted by shape.
fn map_field(fields: Vec<Pack>, what: &str) -> Vec<(Pack, Pack)> {
    let Some(Pack::Map(map)) = fields.into_iter().next() else {
        panic!("{what} carries one map");
    };
    map
}

/// One map field's string value, asserted present.
fn field<'a>(map: &'a [(Pack, Pack)], key: &str) -> &'a str {
    map.iter()
        .find(|(k, _)| k.as_str() == Some(key))
        .and_then(|(_, v)| v.as_str())
        .unwrap_or_else(|| panic!("the {key} field is present"))
}

/// Read the PULL stream to its SUCCESS: every RECORD's value list, in
/// arrival order.
fn pull_records(client: &mut TcpStream) -> Vec<Vec<Pack>> {
    let mut records = Vec::new();
    loop {
        match message(client) {
            Pack::Struct { tag: 0x71, fields } => {
                let Some(Pack::List(values)) = fields.into_iter().next() else {
                    panic!("RECORD carries one list of values");
                };
                records.push(values);
            }
            Pack::Struct { tag: 0x70, .. } => return records,
            other => panic!("expected RECORD or SUCCESS, got {other:?}"),
        }
    }
}

/// The `db.transactions` rows of this session's store, as
/// `(commitTs, metadata, type, changedKeys)` tuples — the retained
/// records read back over the same transport that wrote them.
fn transaction_rows(client: &mut TcpStream) -> Vec<(i64, Pack, String, Pack)> {
    send_run(
        client,
        "CALL db.transactions() YIELD commitTs, metadata, type, changedKeys \
         RETURN commitTs, metadata, type, changedKeys",
    );
    success_map(client);
    send_pull(client);
    pull_records(client)
        .into_iter()
        .map(|values| {
            let [commit_ts, metadata, record_type, changed_keys]: [Pack; 4] =
                values.try_into().expect("the four yielded columns");
            (
                commit_ts.as_int().expect("commitTs is an Int"),
                metadata,
                record_type.as_str().expect("type is a String").to_owned(),
                changed_keys,
            )
        })
        .collect()
}

/// ADR-0610: a BEGIN's `tx_metadata` extra seeds the transaction's
/// per-transaction metadata map, and the write the transaction commits
/// carries it into its WAL record — the commit's retained transaction
/// record holds the map, named by the COMMIT SUCCESS's bookmark.
#[test]
fn begin_tx_metadata_attaches_to_the_commits_record() {
    let db = memory_db();
    let mut client = connected_client(db);
    send_begin(
        &mut client,
        vec![(
            "tx_metadata",
            Pack::Map(vec![
                (Pack::String("by".into()), Pack::String("driver".into())),
                (Pack::String("attempt".into()), Pack::Int(3)),
            ]),
        )],
    );
    success_map(&mut client);
    send_run(&mut client, "CREATE (n:Person {name: 'Ada'})");
    success_map(&mut client);
    send_pull(&mut client);
    pull_records(&mut client);
    send_commit(&mut client);
    let ending = success_map(&mut client);
    let sequence = relate::bookmark_sequence(field(&ending, "bookmark")).expect("the commit wrote");
    // The retained record of that commit, read back through the audit
    // surface's own listing: the merged map the BEGIN seeded.
    let row = transaction_rows(&mut client)
        .into_iter()
        .find(|(commit_ts, ..)| *commit_ts as u64 == sequence)
        .expect("the commit retained its record");
    assert_eq!(
        row.1,
        Pack::Map(vec![
            (Pack::String("attempt".into()), Pack::Int(3)),
            (Pack::String("by".into()), Pack::String("driver".into())),
        ])
    );
}

/// ADR-0610: the transport channel hard-sets the provenance record
/// family — the BEGIN's `tx_metadata` seeds a transaction whose record
/// belongs to the provenance family, like every untyped channel (the
/// embedded channel's side lives in the `relate` suite's typed-records
/// module), and the provenance family's commits carry their derived
/// changed keys.
#[test]
fn begin_tx_metadata_hard_sets_the_provenance_type() {
    let db = memory_db();
    let mut client = connected_client(db);
    send_begin(
        &mut client,
        vec![(
            "tx_metadata",
            Pack::Map(vec![(
                Pack::String("by".into()),
                Pack::String("driver".into()),
            )]),
        )],
    );
    success_map(&mut client);
    send_run(&mut client, "CREATE (n:Person {name: 'Ada'})");
    success_map(&mut client);
    send_pull(&mut client);
    pull_records(&mut client);
    send_commit(&mut client);
    let ending = success_map(&mut client);
    let sequence = relate::bookmark_sequence(field(&ending, "bookmark")).expect("the commit wrote");
    let row = transaction_rows(&mut client)
        .into_iter()
        .find(|(commit_ts, ..)| *commit_ts as u64 == sequence)
        .expect("the commit retained its record");
    assert_eq!(row.2, "provenance");
    assert!(
        matches!(row.3, Pack::List(_)),
        "a provenance record carries its derived changed keys, got {:?}",
        row.3
    );
}

/// ADR-0610, closed side: a malformed `tx_metadata` value fails the BEGIN
/// with the malformed-request rejection before anything starts — the
/// same fail-before-start rule the extra's other keys follow — and the
/// session recovers with RESET.
#[test]
fn a_non_map_begin_tx_metadata_fails_the_begin() {
    let db = memory_db();
    let mut client = connected_client(db);
    send_begin(&mut client, vec![("tx_metadata", Pack::Int(1))]);
    let failure = failure_map(&mut client);
    assert_eq!(field(&failure, "code"), "Neo.ClientError.Request.Invalid");
    assert!(
        field(&failure, "message").contains("Illegal value for field \"tx_metadata\""),
        "{failure:?}"
    );
    // RESET clears the failed state and the session is usable again.
    send_reset(&mut client);
    success_map(&mut client);
    send_run(&mut client, "RETURN 1 AS n");
    success_map(&mut client);
    send_pull(&mut client);
    assert_eq!(pull_records(&mut client), vec![vec![Pack::Int(1)]]);
}
