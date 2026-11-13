//! Test helpers (ADR-0233: sync): one-connection server threads on real
//! loopback sockets, and the client-side Bolt exchange helpers. Client
//! sockets get a generous read timeout so a hung server fails the test
//! instead of hanging the suite.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use crate::packstream::Pack;
use crate::version::BoltVersion;

/// The default read timeout armed on test client sockets: a guard against a
/// wedged server, never the behavior under test (tests that pin a timing
/// contract set their own).
pub const CLIENT_READ_GUARD: Duration = Duration::from_secs(30);

/// Spawn a one-connection server thread running the full `serve_conn`
/// transport stack with `config` (no TLS, plaintext allowed, no WebSocket);
/// returns the bound address. The thread mirrors the production accept
/// loop exactly (ADR-0504): the engine query worker's own 64 MiB stack
/// budget and the fast-path fact that follows from it — so the sessions
/// these fixtures drive exercise the same statement path production does.
pub fn spawn_handler(
    db: std::sync::Arc<relate::Database>,
    config: crate::protocol::ConnConfig,
) -> std::net::SocketAddr {
    spawn_handler_recorded(db, config).0
}

/// [`spawn_handler`], also returning the fixture thread's handle: joining
/// it yields `Some(error text)` when `serve_conn` returned an error and
/// `None` after a clean session, so a test can tell a genuine server-side
/// error from a deliberate close, deterministically. Nothing is logged
/// (many tests abort connections on purpose).
pub fn spawn_handler_recorded(
    db: std::sync::Arc<relate::Database>,
    config: crate::protocol::ConnConfig,
) -> (
    std::net::SocketAddr,
    std::thread::JoinHandle<Option<String>>,
) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let mut config = config;
    // The fast-path fact, anchored exactly as `serve_listener_with_cap`
    // anchors it: the thread this fixture spawns carries the worker
    // budget (the same `CONNECTION_STACK_BYTES` alias), so the sessions
    // these fixtures drive exercise the same statement path production
    // does (ADR-0504).
    config.caller_thread_carries_worker_stack =
        crate::server::CONNECTION_STACK_BYTES == relate::QUERY_WORKER_STACK_BYTES;
    let handle = std::thread::Builder::new()
        .stack_size(crate::server::CONNECTION_STACK_BYTES)
        .spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let t = crate::transport::Transport {
                tls: None,
                plaintext_allowed: true,
                websocket: None,
            };
            crate::transport::serve_conn(stream, &t, db, config)
                .err()
                .map(|e| e.to_string())
        })
        .expect("spawn the fixture's connection thread");
    (addr, handle)
}

/// Assert the server closes the connection (EOF) within the read guard.
pub fn assert_closes(client: &mut TcpStream, what: &str) {
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut buf = [0u8; 1];
    let closed = client
        .read(&mut buf)
        .unwrap_or_else(|e| panic!("{what}: {e}"));
    assert_eq!(closed, 0, "{what}");
}

/// The `user_agent` every test HELLO carries (Bolt 4.4 requires one,
/// ADR-0313).
pub const TEST_USER_AGENT: &str = "relate-bolt-tests/1.0";

/// The `bolt_agent` every test HELLO carries at 5.x, where one is required
/// (ADR-0484 decision 3): a string-to-string map, the minimum honest
/// shape, its product the [`TEST_USER_AGENT`] identity — the one spelling
/// of the fixture client's identity.
pub(crate) fn test_bolt_agent() -> Pack {
    map(vec![("product", Pack::String(TEST_USER_AGENT.into()))])
}

/// A HELLO extra map: the required `user_agent` (ADR-0313) plus `entries`
/// — [`hello_extra_at`] at 4.4, where the fixtures keep their historical
/// shape.
pub fn hello_extra(entries: Vec<(Pack, Pack)>) -> Pack {
    hello_extra_at(BoltVersion::V4_4, entries)
}

/// The HELLO's required entries at `version` (ADR-0313, ADR-0484
/// decision 3): the `user_agent` every spoken version requires plus, at
/// 5.x, the `bolt_agent` map — the one table the extra-map family
/// prepends or overrides.
fn required_hello_entries(version: BoltVersion) -> Vec<(Pack, Pack)> {
    let mut entries = vec![(
        Pack::String("user_agent".into()),
        Pack::String(TEST_USER_AGENT.into()),
    )];
    if version.speaks_5x() {
        entries.push((Pack::String("bolt_agent".into()), test_bolt_agent()));
    }
    entries
}

/// A HELLO extra map valid at `version` (ADR-0484 decision 3): the
/// required entries, then `entries` — the one body owning the
/// required-key prepend policy. An extra key a 4.4 HELLO does not know
/// is ignored, so the 5.x map is valid there too; the 4.4 fixtures keep
/// their historical shape through [`hello_extra`], its 4.4 delegation.
pub(crate) fn hello_extra_at(version: BoltVersion, entries: Vec<(Pack, Pack)>) -> Pack {
    let mut map = required_hello_entries(version);
    map.extend(entries);
    Pack::Map(map)
}

/// A HELLO extra map at `version` with one required key's entry forced
/// to `value` — the key absent when `None`: the hello-extra family's
/// override form, over which the malformed-HELLO rejection tests
/// table-drive one row per required key.
pub(crate) fn hello_extra_at_overriding(
    version: BoltVersion,
    key: &str,
    value: Option<Pack>,
) -> Pack {
    let mut map = required_hello_entries(version);
    map.retain(|(k, _)| k.as_str() != Some(key));
    if let Some(value) = value {
        map.push((Pack::String(key.into()), value));
    }
    Pack::Map(map)
}

/// The fields of a plain HELLO: the extra map with only `user_agent`.
pub fn hello_fields() -> Vec<Pack> {
    vec![hello_extra(vec![])]
}

/// A fresh in-memory database — the store every fixture that owns its
/// db opens, one spelling — opened with the `ontology` preview opt-in
/// (ADR-0593). The opt-in is the fixture's precondition, not a per-test
/// call: the setting is process-global and first-call-wins, so per-test
/// calls make every store-opening test order-dependent. The flag only
/// affects the gated surfaces; the transport tests that never touch them
/// are unchanged by it.
pub(crate) fn memory_db() -> std::sync::Arc<relate::Database> {
    relate_test_support::preview::preview_ontology();
    std::sync::Arc::new(relate::Database::open(":memory:").unwrap())
}

/// A connected client past the version negotiation and a plain HELLO
/// (the shared fixture the protocol test modules used to duplicate).
pub fn connected_client() -> TcpStream {
    connected_client_with_db().1
}

/// The same connected client beside the database handle it spawned —
/// the variant for the tests that re-run statements through the plain
/// engine connection; the open-session core ([`open_client_db_at`])
/// keeps the preamble one spelling.
#[allow(clippy::type_complexity)]
pub fn connected_client_with_db() -> (std::sync::Arc<relate::Database>, TcpStream) {
    let db = memory_db();
    (
        db.clone(),
        open_client_db_at(db, Default::default(), BoltVersion::V4_4, vec![]),
    )
}

/// [`connected_client`] against `db` served under `config`.
pub fn connected_client_with(
    db: std::sync::Arc<relate::Database>,
    config: crate::protocol::ConnConfig,
) -> TcpStream {
    open_client_db_at(db, config, BoltVersion::V4_4, vec![])
}

/// [`connected_client_with`] sending a caller-supplied HELLO extra map —
/// the whole-map form, its caller owning the complete extra instead of
/// entries beside the required fields ([`hello_extra_at`]): returns the
/// client and the HELLO SUCCESS metadata.
pub fn connected_client_with_hello(
    db: std::sync::Arc<relate::Database>,
    config: crate::protocol::ConnConfig,
    extra: Pack,
) -> (TcpStream, Pack) {
    let mut client = handshaken_db_at(db, config, BoltVersion::V4_4);
    send_struct(
        &mut client,
        crate::requests::Message::Hello.signature(),
        vec![extra],
    );
    let metadata = Pack::Map(success_map(read_msg(&mut client)));
    (client, metadata)
}

/// The HELLO step the version-aware fixtures compose: a HELLO built from
/// `entries` beside `version`'s required fields ([`hello_extra_at`]) and
/// its SUCCESS. Generic over the stream like its sibling senders.
fn send_hello_at<S: Read + Write>(
    client: &mut S,
    version: BoltVersion,
    entries: Vec<(Pack, Pack)>,
) {
    send_struct(
        client,
        crate::requests::Message::Hello.signature(),
        vec![hello_extra_at(version, entries)],
    );
    success_map(read_msg(client));
}

/// The open-session tail over a handshaken client (ADR-0484): the HELLO
/// from `entries` ([`send_hello_at`]), then — on a 5.x connection — the
/// LOGON that opens the session, `{"scheme": "none"}`, which
/// authenticates under the fixtures' auth-none config — the tail
/// [`open_client_db_at`] wraps behind the preamble.
fn open_session_at(client: &mut TcpStream, version: BoltVersion, entries: Vec<(Pack, Pack)>) {
    send_hello_at(client, version, entries);
    if version.speaks_5x() {
        send_logon(client, vec![("scheme", Pack::String("none".into()))]);
        success_map(read_msg(client));
    }
}

/// The connect+spawn+handshake preamble over a caller's `db`: spawn the
/// one-connection server under `config`, connect, and negotiate
/// `version` — the one body every fixture composes; db ownership is its
/// only axis.
fn handshaken_db_at(
    db: std::sync::Arc<relate::Database>,
    config: crate::protocol::ConnConfig,
    version: BoltVersion,
) -> TcpStream {
    let mut client = connect(spawn_handler(db, config));
    handshake_at(&mut client, version.wire());
    client
}

/// The axis-complete open-session core (ADR-0484) the connected-client
/// fixture family spells slices of: the preamble ([`handshaken_db_at`])
/// over `db` under `config` at `version`, then [`open_session_at`] from
/// `entries` — every `{db, config, version, hello-entries}` combination
/// one signature.
fn open_client_db_at(
    db: std::sync::Arc<relate::Database>,
    config: crate::protocol::ConnConfig,
    version: BoltVersion,
    entries: Vec<(Pack, Pack)>,
) -> TcpStream {
    let mut client = handshaken_db_at(db, config, version);
    open_session_at(&mut client, version, entries);
    client
}

/// A connected client past the version negotiation, a plain HELLO and —
/// at 5.4 — the LOGON that opens the session (ADR-0484): the open-session
/// base for the version-gated message tests, the per-version spelling of
/// [`connected_client`] (which is the 4.4 form under `--auth none`). The
/// 5.4 LOGON is `{"scheme": "none"}`, which authenticates under the
/// fixture's auth-none config.
pub(crate) fn connected_client_at(version: BoltVersion) -> TcpStream {
    open_client_db_at(memory_db(), Default::default(), version, vec![])
}

/// [`connected_client_at`] over a caller-supplied `db` and HELLO
/// `entries` — the version-aware form of [`connected_client_with_hello`]'s
/// db + extra parameterization: seed `db` before the call and the session
/// reads the seeded store; `entries` ride the HELLO beside the version's
/// required fields.
pub(crate) fn connected_client_at_with(
    db: std::sync::Arc<relate::Database>,
    version: BoltVersion,
    entries: Vec<(Pack, Pack)>,
) -> TcpStream {
    open_client_db_at(db, Default::default(), version, entries)
}

/// A 5.4 client past its HELLO and awaiting LOGON (ADR-0484 decision 4):
/// handshake, validated HELLO, no LOGON yet — the AUTHENTICATION-state
/// fixture for the LOGON flow tests. Under an auth-configured `config`
/// the HELLO still validates (the 5.x HELLO does not authenticate).
pub(crate) fn awaiting_logon_client(config: crate::protocol::ConnConfig) -> TcpStream {
    let mut client = handshaken_at(config, BoltVersion::V5_4);
    send_hello_at(&mut client, BoltVersion::V5_4, vec![]);
    client
}

/// The two-label store the result-metadata and notification fixtures
/// share (ADR-0363): one `:Person` and one `:City`, seeded through the
/// plain engine connection so the Bolt session only ever sees the
/// statements under test — the seed [`TWO_LABEL_CARTESIAN`] and the
/// notification triggers read.
pub(crate) fn two_label_db() -> std::sync::Arc<relate::Database> {
    let db = memory_db();
    let mut conn = db.connect().unwrap();
    conn.query("CREATE (:Person)").unwrap();
    conn.query("CREATE (:City)").unwrap();
    drop(conn);
    db
}

/// The two-label store's INFORMATION trigger (ADR-0363): a multi-pattern
/// MATCH whose patterns share no node variable, planning as a Cartesian
/// product — the one spelling of the shared EXPLAIN beside the store
/// that seeds it.
pub(crate) const TWO_LABEL_CARTESIAN: &str = "EXPLAIN MATCH (a:Person), (b:City) RETURN a, b";

/// [`handshaken`] at a negotiated version (ADR-0484): past the version
/// exchange only — the pre-HELLO fixture for the tests that drive HELLO
/// and LOGON themselves at 5.4.
pub(crate) fn handshaken_at(
    config: crate::protocol::ConnConfig,
    version: BoltVersion,
) -> TcpStream {
    handshaken_db_at(memory_db(), config, version)
}

/// A connected client past the version negotiation only (no HELLO): the
/// pre-HELLO fixture for tests that drive HELLO themselves — the 4.4
/// spelling of [`handshaken_at`].
pub fn handshaken(config: crate::protocol::ConnConfig) -> TcpStream {
    handshaken_at(config, BoltVersion::V4_4)
}

/// The DISCARD-drain exchange: send the DISCARD with an empty map and
/// read its SUCCESS — the shared fixture for draining a buffered result
/// so the next RUN starts clean.
pub fn discard<S: std::io::Read + std::io::Write>(client: &mut S) {
    send_struct(
        client,
        crate::requests::Message::Discard.signature(),
        vec![Pack::Map(vec![])],
    );
    success_map(read_msg(client));
}

/// Connect a client socket with the read guard armed.
pub fn connect(addr: std::net::SocketAddr) -> TcpStream {
    let client = TcpStream::connect(addr).unwrap();
    client.set_read_timeout(Some(CLIENT_READ_GUARD)).unwrap();
    client
}

/// The engine's own error text for `cypher`, run directly on a fresh
/// in-memory database: the text a wire FAILURE must carry verbatim
/// (ADR-0307).
pub fn engine_error(cypher: &str) -> String {
    relate::Database::open(":memory:")
        .unwrap()
        .connect()
        .unwrap()
        .query(cypher)
        .unwrap_err()
        .to_string()
}

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
pub use relate_test_support::bolt::{handshake, handshake_at, try_handshake};

/// Write one struct message to the client; panics on failure.
pub fn send_struct<S: Write>(client: &mut S, tag: u8, fields: Vec<Pack>) {
    client
        .write_all(&crate::packstream::try_struct_msg(tag, fields).unwrap())
        .unwrap();
    client.flush().unwrap();
}

/// Extract the metadata map of a FAILURE message; panics otherwise.
pub fn failure_map(msg: Pack) -> Pack {
    let Pack::Struct { tag: 0x7F, fields } = msg else {
        panic!("expected FAILURE, got {msg:?}");
    };
    let Some(m @ Pack::Map(_)) = fields.into_iter().next() else {
        panic!("expected FAILURE map");
    };
    m
}

/// Extract a string field from a Pack map; panics when absent or not a
/// string. The lookup itself is the canonical [`Pack::get`].
pub fn field<'a>(m: &'a Pack, key: &str) -> &'a str {
    m.get(key)
        .and_then(Pack::as_str)
        .unwrap_or_else(|| panic!("missing field {key}"))
}

/// Read one chunked Bolt message and decode it.
///
/// Delegates the chunk loop to [`crate::protocol::read_message`] so tests
/// inherit its 16 MiB message cap.
pub fn try_read_msg<S: Read>(client: &mut S) -> std::io::Result<Pack> {
    let payload = crate::protocol::read_message(client)?;
    crate::packstream::decode(&payload)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Read one chunked Bolt message and decode it; panics on failure.
pub fn read_msg<S: Read>(client: &mut S) -> Pack {
    try_read_msg(client).unwrap()
}

/// Extract the metadata map of a SUCCESS message; panics otherwise.
pub fn success_map(msg: Pack) -> Vec<(Pack, Pack)> {
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
pub use relate_test_support::ws_upgrade_request as upgrade_request;

/// A full HELLO / `RUN RETURN 1 AS n` / PULL exchange over a Bolt byte
/// stream; asserts the RECORD carries `1`.
pub fn return_one_exchange<S: Read + Write>(client: &mut S) {
    handshake(client);
    send_hello_at(client, BoltVersion::V4_4, vec![]);
    send_run_at(client, BoltVersion::V4_4, "RETURN 1 AS n", None);
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

/// A Pack map from string keys.
pub fn map(entries: Vec<(&str, Pack)>) -> Pack {
    Pack::Map(
        entries
            .into_iter()
            .map(|(key, value)| (Pack::String(key.into()), value))
            .collect(),
    )
}

/// Send RUN `cypher` with empty params at `version` (ADR-0484): the
/// two-field wire form at 4.4, the three-field form at 5.x — `extra`
/// entries ride the extra map at either version (the Bolt 4.3+ `db`
/// routing field at 4.4). The one RUN sender: the version axis is this
/// parameter, not a helper name.
pub(crate) fn send_run_at<S: Write>(
    client: &mut S,
    version: BoltVersion,
    cypher: &str,
    extra: Option<Vec<(Pack, Pack)>>,
) {
    let mut fields = vec![Pack::String(cypher.into()), Pack::Map(vec![])];
    match extra {
        Some(entries) => fields.push(Pack::Map(entries)),
        None if version.speaks_5x() => fields.push(Pack::Map(vec![])),
        None => {}
    }
    send_struct(client, crate::requests::Message::Run.signature(), fields);
}

/// Send RUN `cypher` with empty params and an empty extra map — the
/// three-field form, [`send_run_at`] at 5.4: the suite's historical
/// spelling, byte-identical to the form it has always sent (the server
/// accepts the extra field at every spoken version).
pub fn send_run<S: Write>(client: &mut S, cypher: &str) {
    send_run_at(client, BoltVersion::V5_4, cypher, None);
}

/// Send BEGIN with a caller-supplied extra map.
pub(crate) fn send_begin<S: Write>(client: &mut S, entries: Vec<(Pack, Pack)>) {
    send_struct(
        client,
        crate::requests::Message::Begin.signature(),
        vec![Pack::Map(entries)],
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

/// Send LOGOFF — the 5.x session message with no fields.
pub(crate) fn send_logoff<S: Write>(client: &mut S) {
    send_struct(client, crate::requests::Message::Logoff.signature(), vec![]);
}

/// Send TELEMETRY with `api` as its one field — `None` the absent form,
/// the malformed-`api` table's row axis (the logon module's
/// `Option<Pack>` row shape for a missing field).
pub(crate) fn send_telemetry<S: Write>(client: &mut S, api: Option<Pack>) {
    let fields = match api {
        Some(api) => vec![api],
        None => vec![],
    };
    send_struct(
        client,
        crate::requests::Message::Telemetry.signature(),
        fields,
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
pub fn read_records<S: Read>(client: &mut S) -> (Vec<Vec<Pack>>, Pack) {
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
pub fn assert_ignored<S: Read>(client: &mut S, why: &str) {
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
    send_struct(client, crate::requests::Message::Reset.signature(), vec![]);
    success_map(read_msg(client));
    send_struct(client, message.signature(), fields);
    assert_eq!(success_map(read_msg(client)), vec![]);
}

/// The keys of a metadata map, in wire order.
pub fn keys(metadata: &[(Pack, Pack)]) -> Vec<&str> {
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
