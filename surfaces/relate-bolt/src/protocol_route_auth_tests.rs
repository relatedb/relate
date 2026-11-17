//! Route/auth concerns of the `protocol` tests: ADR-0126 preset basic auth
//! on HELLO, the ADR-0269/0140 ROUTE single-server self routing table, and
//! the ADR-0139 failed-HELLO delay. Wired into `protocol.rs` beside the
//! other protocol test modules.
//!
//! ADR-0233: the ADR-0139 delay tests ran on tokio's paused clock; the sync
//! port uses real short delays — the delay itself is the 1000 ms constant,
//! so the failed-HELLO test takes about a second of wall clock.

use super::*;
use crate::failures::{FAILED_HELLO_DELAY_MS, UNAUTHORIZED_CODE};
use crate::packstream::Pack;
use crate::requests::Message;
use crate::summary::ROUTE_TTL_SECS;
use crate::testutil::{
    failure_map, field, read_msg, send_pull, send_route, send_run_at, send_struct, spawn_handler,
    success_map,
};
use crate::version::BoltVersion;
use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

// --- ADR-0126: preset basic auth on the HELLO token --------------------------

fn hello_with(client: &mut TcpStream, entries: Vec<(Pack, Pack)>) -> Pack {
    send_struct(client, Message::Hello.signature(), vec![Pack::Map(entries)]);
    read_msg(client)
}

fn basic_hello_entries(user: &str, password: &str) -> Vec<(Pack, Pack)> {
    vec![
        (
            Pack::String("user_agent".into()),
            Pack::String("test".into()),
        ),
        (Pack::String("scheme".into()), Pack::String("basic".into())),
        (Pack::String("principal".into()), Pack::String(user.into())),
        (
            Pack::String("credentials".into()),
            Pack::String(password.into()),
        ),
    ]
}

/// A HELLO carrying only the user agent — no scheme/principal/credentials.
fn user_agent_only() -> Vec<(Pack, Pack)> {
    vec![(
        Pack::String("user_agent".into()),
        Pack::String("test".into()),
    )]
}

/// One auth scenario: spawn a handler with `auth`, connect, handshake, and
/// return the server's response to a HELLO carrying `entries`.
fn hello_attempt(
    db: Arc<Database>,
    auth: crate::auth::AuthConfig,
    entries: Vec<(Pack, Pack)>,
) -> Pack {
    let addr = spawn_handler(
        db,
        ConnConfig {
            auth: Arc::new(auth),
            ..Default::default()
        },
    );
    let mut client = crate::testutil::connect(addr);
    crate::testutil::handshake(&mut client);
    hello_with(&mut client, entries)
}

/// The generated-token preamble both token tests share: a tempdir-backed
/// store URI, `auth::resolve` with nothing preset, and the generated
/// user/token pair. The TempDir guard comes back so the store path lives
/// as long as the test.
fn generated_basic_auth() -> (tempfile::TempDir, crate::auth::AuthConfig, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let uri = dir.path().join("graph").to_str().unwrap().to_string();
    let (config, _) = crate::auth::resolve(None, None, &uri).unwrap();
    let (user, token) = {
        let crate::auth::AuthConfig::Basic { user, password } = &config else {
            panic!("a generated credential is basic");
        };
        (user.clone(), password.clone())
    };
    (dir, config, user, token)
}

#[test]
fn auth_generated_token_validates_itself() {
    let (_dir, config, user, token) = generated_basic_auth();
    let msg = hello_attempt(
        Arc::new(Database::open(":memory:").unwrap()),
        config,
        basic_hello_entries(&user, &token),
    );
    assert!(
        matches!(msg, Pack::Struct { tag: 0x70, .. }),
        "generated token must authenticate: {msg:?}"
    );
}

/// ADR-0126 regression: a client that skips HELLO entirely must not get
/// query execution — the critical auth-bypass class (RUN/PULL/BEGIN are
/// gated on a validated HELLO when a credential is configured).
/// The ADR-0126 gate, pinned for one message: with a credential
/// configured, the message `send` drives before a validated HELLO is a
/// FAILURE with UNAUTHORIZED_CODE and the connection closes — the send
/// goes through its canonical sender, only the assertion is shared.
fn assert_pre_hello_is_unauthorized(send: impl FnOnce(&mut TcpStream)) {
    let config = crate::auth::AuthConfig::Basic {
        user: "relate".into(),
        password: "token".into(),
    };
    let db = Arc::new(Database::open(":memory:").unwrap());
    let addr = spawn_handler(
        db,
        ConnConfig {
            auth: Arc::new(config),
            ..Default::default()
        },
    );
    let mut client = crate::testutil::connect(addr);
    crate::testutil::handshake(&mut client);

    send(&mut client);
    let msg = read_msg(&mut client);
    assert!(
        matches!(msg, Pack::Struct { tag: 0x7F, .. }),
        "a message before a validated HELLO must fail: {msg:?}"
    );
    assert_eq!(field(&failure_map(msg), "code"), UNAUTHORIZED_CODE);

    // after the failure the connection is closed — reads return 0
    let mut b = [0u8; 1];
    assert_eq!(
        client.read(&mut b).expect("closed read"),
        0,
        "the server must close an unauthorized connection"
    );
}

#[test]
fn run_without_hello_is_unauthorized_when_auth_is_on() {
    // RUN straight away, no HELLO
    assert_pre_hello_is_unauthorized(|client| {
        send_run_at(client, BoltVersion::V4_4, "RETURN 1 AS n", None)
    });
}

/// And the permissive pre-auth behavior is untouched when auth is disabled:
/// a RUN without any HELLO still executes.
#[test]
fn run_without_hello_works_when_auth_is_none() {
    let db = Arc::new(Database::open(":memory:").unwrap());
    let addr = spawn_handler(db, ConnConfig::default());
    let mut client = crate::testutil::connect(addr);
    crate::testutil::handshake(&mut client);
    send_run_at(&mut client, BoltVersion::V4_4, "RETURN 1 AS n", None);
    let msg = read_msg(&mut client);
    assert!(
        matches!(msg, Pack::Struct { tag: 0x70, .. }),
        "auth off: RUN without HELLO stays accepted: {msg:?}"
    );
}

fn assert_tag(msg: &Pack, tag: u8) {
    assert!(
        matches!(msg, Pack::Struct { tag: t, .. } if *t == tag),
        "want tag {tag:#x}, got {msg:?}"
    );
}

#[test]
fn auth_accepts_the_preset_and_rejects_everything_else() {
    let config = crate::auth::AuthConfig::Basic {
        user: "relate".into(),
        password: "token".into(),
    };
    let db = Arc::new(Database::open(":memory:").unwrap());

    // right credentials: SUCCESS with the server key
    let msg = hello_attempt(
        Arc::clone(&db),
        config.clone(),
        basic_hello_entries("relate", "token"),
    );
    assert_tag(&msg, 0x70);

    // wrong password: FAILURE
    let msg = hello_attempt(
        Arc::clone(&db),
        config.clone(),
        basic_hello_entries("relate", "wrong"),
    );
    assert_tag(&msg, 0x7F);

    // no credentials at all: FAILURE
    let msg = hello_attempt(Arc::clone(&db), config, user_agent_only());
    assert_tag(&msg, 0x7F);

    // auth disabled: plain HELLO succeeds
    let msg = hello_attempt(db, crate::auth::AuthConfig::None, user_agent_only());
    assert_tag(&msg, 0x70);
}

// --- ADR-0273: the validated HELLO principal reaches dbms.showCurrentUser --

/// HELLO with `entries`, then `CALL dbms.showCurrentUser()` + PULL, and
/// return the first RECORD's column list.
fn show_current_user_row(auth: crate::auth::AuthConfig, entries: Vec<(Pack, Pack)>) -> Vec<Pack> {
    let addr = spawn_handler(
        Arc::new(Database::open(":memory:").unwrap()),
        ConnConfig {
            auth: Arc::new(auth),
            ..Default::default()
        },
    );
    let mut client = crate::testutil::connect(addr);
    crate::testutil::handshake(&mut client);
    assert_tag(&hello_with(&mut client, entries), 0x70);
    send_run_at(
        &mut client,
        BoltVersion::V4_4,
        "CALL dbms.showCurrentUser()",
        None,
    );
    success_map(read_msg(&mut client));
    send_pull(&mut client, Message::Pull, vec![("n", Pack::Int(-1))]);
    let Pack::Struct { tag: 0x71, fields } = read_msg(&mut client) else {
        panic!("expected RECORD");
    };
    let Some(Pack::List(columns)) = fields.into_iter().next() else {
        panic!("expected RECORD column list");
    };
    columns
}

/// `--auth alice:<pw>`: the validated HELLO principal is plumbed engine-ward
/// and `dbms.showCurrentUser()` answers `username: 'alice'`.
#[test]
fn show_current_user_reports_the_validated_hello_principal() {
    let row = show_current_user_row(
        crate::auth::AuthConfig::Basic {
            user: "alice".into(),
            password: "pw".into(),
        },
        basic_hello_entries("alice", "pw"),
    );
    assert_eq!(
        row,
        vec![
            Pack::String("alice".into()),
            Pack::List(vec![Pack::String("admin".into())]),
            Pack::List(vec![]),
        ]
    );
}

/// The generated-token default takes the same path: the generated user is
/// `relate`, so the report is honest rather than a placeholder.
#[test]
fn show_current_user_reports_the_generated_token_user() {
    let (_dir, config, user, token) = generated_basic_auth();
    let row = show_current_user_row(config, basic_hello_entries(&user, &token));
    assert_eq!(row[0], Pack::String("relate".into()));
}

/// `--auth none`: the HELLO principal is unverified and never forwarded;
/// `dbms.showCurrentUser()` keeps the documented placeholder.
#[test]
fn show_current_user_ignores_the_unverified_principal_when_auth_is_none() {
    let row = show_current_user_row(
        crate::auth::AuthConfig::None,
        basic_hello_entries("mallory", "anything"),
    );
    assert_eq!(
        row,
        vec![
            Pack::String("relate".into()),
            Pack::List(vec![Pack::String("admin".into())]),
            Pack::List(vec![]),
        ]
    );
}

// --- ADR-0269/0140: ROUTE single-server self routing table ------------------

/// Send the neo4j-browser connect message — `ROUTE <routing> [] {db:"system"}`
/// — after a successful HELLO and return the `rt` map of the SUCCESS.
fn route_rt<S: Read + std::io::Write>(client: &mut S, routing: Vec<(Pack, Pack)>) -> Pack {
    send_struct(
        client,
        Message::Hello.signature(),
        crate::testutil::hello_fields(),
    );
    success_map(read_msg(client));
    send_route(
        client,
        routing,
        vec![(Pack::String("db".into()), Pack::String("system".into()))],
    );
    let m = Pack::Map(success_map(read_msg(client)));
    m.get("rt")
        .expect("ROUTE SUCCESS must carry an rt map")
        .clone()
}

/// The ADR-0269 invariants plus the ADR-0140 address rule and the ADR-0318
/// role order: ttl, the db echo of the requested `system`, and the WRITE,
/// READ and ROUTE entries, in that order, each advertising exactly
/// `expected_addresses`.
fn assert_self_routing_table(rt: &Pack, expected_addresses: &[&str]) {
    assert_routing_table(rt, "system", expected_addresses);
}

fn assert_routing_table(rt: &Pack, db: &str, expected_addresses: &[&str]) {
    assert_eq!(
        rt.get("ttl").and_then(Pack::as_int),
        Some(ROUTE_TTL_SECS),
        "rt.ttl is the ADR-0269 value"
    );
    assert_eq!(rt.get("db").and_then(Pack::as_str), Some(db), "rt.db");
    let Some(Pack::List(servers)) = rt.get("servers") else {
        panic!("rt must carry a servers list: {rt:?}");
    };
    let roles: Vec<_> = servers
        .iter()
        .map(|s| s.get("role").and_then(Pack::as_str))
        .collect();
    assert_eq!(
        roles,
        [Some("WRITE"), Some("READ"), Some("ROUTE")],
        "Neo4j's role order (ADR-0318)"
    );
    let expected = Pack::List(
        expected_addresses
            .iter()
            .map(|address| Pack::String((*address).into()))
            .collect(),
    );
    for entry in servers {
        assert_eq!(
            entry.get("addresses"),
            Some(&expected),
            "every role addresses {expected_addresses:?}"
        );
    }
}

/// ADR-0318 (F-36, F-37): a ROUTE whose extra names no database answers the
/// home database `neo4j`, roles in Neo4j's WRITE, READ, ROUTE order.
#[test]
fn route_without_a_db_answers_the_home_database() {
    let mut client = crate::testutil::connected_client();
    for extra in [
        vec![],
        vec![(Pack::String("db".into()), Pack::Null)],
        vec![(Pack::String("db".into()), Pack::String(String::new()))],
    ] {
        send_route(
            &mut client,
            vec![(
                Pack::String("address".into()),
                Pack::String("localhost:7687".into()),
            )],
            extra,
        );
        let m = Pack::Map(success_map(read_msg(&mut client)));
        let rt = m.get("rt").expect("ROUTE SUCCESS must carry an rt map");
        assert_routing_table(rt, "neo4j", &["localhost:7687"]);
    }
}

/// The routing table's exact layout (ADR-0269, ADR-0318).
#[test]
fn routing_table_lists_write_read_route() {
    let server = |role: &str| {
        Pack::Map(vec![
            (Pack::String("role".into()), Pack::String(role.into())),
            (
                Pack::String("addresses".into()),
                Pack::List(vec![Pack::String("h:1".into())]),
            ),
        ])
    };
    assert_eq!(
        routing_table("h:1", "neo4j"),
        Pack::Map(vec![
            (Pack::String("ttl".into()), Pack::Int(300)),
            (
                Pack::String("servers".into()),
                Pack::List(vec![server("WRITE"), server("READ"), server("ROUTE")]),
            ),
            (Pack::String("db".into()), Pack::String("neo4j".into())),
        ])
    );
}

/// ADR-0140: the routing-context `address` the client dialed is advertised
/// verbatim — it reached this server for this client, so it is routable.
#[test]
fn route_echoes_the_dialed_address() {
    let db = Arc::new(Database::open(":memory:").unwrap());
    let addr = spawn_handler(db, ConnConfig::default());
    let mut client = crate::testutil::connect(addr);
    crate::testutil::handshake(&mut client);

    let rt = route_rt(
        &mut client,
        vec![(
            Pack::String("address".into()),
            Pack::String("localhost:7687".into()),
        )],
    );
    assert_self_routing_table(&rt, &["localhost:7687"]);
}

/// ADR-0140: an empty routing context falls back to the connection's local
/// address carried by the `ConnConfig` — decided at the transport boundary,
/// where `serve_conn` sets it from the accepted socket (or closes the
/// connection when the lookup fails). This test drives `handle_conn`
/// directly with a test address.
#[test]
fn route_falls_back_to_the_connection_local_address() {
    let db = Arc::new(Database::open(":memory:").unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let config = ConnConfig {
        local_address: "127.0.0.1:9".into(),
        ..Default::default()
    };
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let _ = handle_conn(stream, db, config, None);
    });
    let mut client = crate::testutil::connect(addr);
    crate::testutil::handshake(&mut client);

    for routing in [
        vec![],
        vec![(Pack::String("address".into()), Pack::String(String::new()))],
    ] {
        let rt = route_rt(&mut client, routing);
        assert_self_routing_table(&rt, &["127.0.0.1:9"]);
    }
}

/// ADR-0126 gate, pinned for ROUTE: with a credential configured, a
/// ROUTE before a validated HELLO is Unauthorized and the connection closes.
#[test]
fn route_before_hello_is_unauthorized_when_auth_is_on() {
    assert_pre_hello_is_unauthorized(|client| {
        send_struct(client, Message::Route.signature(), vec![Pack::Map(vec![])])
    });
}

// --- ADR-0139: failed-HELLO delay ------------------------------------------

/// No timeouts: the only clocks in these tests are the ADR-0139 delay and
/// the client's own read deadlines.
fn no_timeouts() -> ConnLimits {
    ConnLimits {
        handshake_timeout: None,
        idle_timeout: None,
        query_timeout: None,
        ..ConnLimits::default()
    }
}

fn basic_auth_config() -> ConnConfig {
    ConnConfig {
        auth: Arc::new(crate::auth::AuthConfig::Basic {
            user: "relate".into(),
            password: "token".into(),
        }),
        limits: no_timeouts(),
        ..Default::default()
    }
}

/// A `handle_conn` server thread on one loopback connection plus the
/// connected client.
fn hello_server(config: ConnConfig) -> (TcpStream, std::thread::JoinHandle<()>) {
    let db = Arc::new(Database::open(":memory:").unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let task = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        handle_conn(stream, db, config, None).unwrap();
    });
    let mut client = TcpStream::connect(addr).unwrap();
    crate::testutil::handshake(&mut client);
    (client, task)
}

/// ADR-0139: a wrong-credential HELLO does not receive the Unauthorized
/// FAILURE before the 1000 ms delay elapses, and does afterwards; the
/// connection then closes. (The pre-sync test pinned this with tokio's
/// paused clock; the sync port measures the real delay — it costs about a
/// second of wall clock.)
#[test]
fn failed_hello_waits_before_the_unauthorized_failure() {
    let (mut client, server_task) = hello_server(basic_auth_config());
    send_struct(
        &mut client,
        Message::Hello.signature(),
        vec![Pack::Map(basic_hello_entries("relate", "wrong"))],
    );

    // Nothing arrives before the delay elapses: a read deadline well under
    // 1000 ms must time out.
    client
        .set_read_timeout(Some(Duration::from_millis(FAILED_HELLO_DELAY_MS / 2)))
        .unwrap();
    let sent = Instant::now();
    let early = crate::testutil::try_read_msg(&mut client);
    assert!(
        early.as_ref().is_err_and(crate::protocol::is_timeout),
        "no FAILURE before the ADR-0139 delay: {early:?}"
    );

    // Once the delay has elapsed, the Unauthorized FAILURE arrives.
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let msg = crate::testutil::try_read_msg(&mut client)
        .expect("FAILURE once the ADR-0139 delay has elapsed");
    let elapsed = sent.elapsed();
    assert!(
        elapsed >= Duration::from_millis(FAILED_HELLO_DELAY_MS - 100),
        "the FAILURE arrived before the 1000 ms delay: {elapsed:?}"
    );
    let fields = failure_map(msg);
    assert_eq!(field(&fields, "code"), UNAUTHORIZED_CODE);

    // The failed HELLO closes the connection.
    server_task.join().unwrap();
}

/// ADR-0139: a valid HELLO gets SUCCESS promptly — the delay applies only
/// to failures.
#[test]
fn successful_hello_is_answered_without_the_delay() {
    let (mut client, server_task) = hello_server(basic_auth_config());

    let start = Instant::now();
    send_struct(
        &mut client,
        Message::Hello.signature(),
        vec![Pack::Map(basic_hello_entries("relate", "token"))],
    );
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let msg = crate::testutil::try_read_msg(&mut client).expect("a valid HELLO is answered");
    success_map(msg);
    assert!(
        start.elapsed() < Duration::from_millis(500),
        "a successful HELLO is answered without the ADR-0139 delay"
    );

    send_struct(&mut client, Message::Goodbye.signature(), vec![]);
    server_task.join().unwrap();
}
