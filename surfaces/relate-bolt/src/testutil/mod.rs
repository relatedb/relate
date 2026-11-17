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

mod wire;
pub use wire::*;
