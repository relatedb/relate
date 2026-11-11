use super::*;
use crate::packstream::{decode, try_struct_msg, Pack};
use crate::requests::Message;
use crate::testutil::{
    assert_closes, connected_client_with, failure_map, field, read_msg, send_pull, send_run_at,
    send_struct, spawn_handler, success_map,
};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

fn limits(handshake: Duration, idle: Duration, max_result_bytes: u64) -> ConnLimits {
    ConnLimits {
        handshake_timeout: Some(handshake),
        idle_timeout: Some(idle),
        query_timeout: None,
        max_result_bytes,
        max_connections: DEFAULT_MAX_CONNECTIONS,
        ..ConnLimits::default()
    }
}

#[test]
fn handshake_timeout_closes_a_silent_client() {
    let db = Arc::new(Database::open(":memory:").unwrap());
    let addr = spawn_handler(
        db,
        ConnConfig {
            limits: limits(
                Duration::from_millis(200),
                Duration::from_secs(5),
                DEFAULT_MAX_RESULT_BYTES,
            ),
            ..Default::default()
        },
    );
    let mut client = TcpStream::connect(addr).unwrap();
    assert_closes(
        &mut client,
        "a silent client must be closed by the handshake timeout",
    );
}

/// Regression: a negotiation timeout must CLOSE the connection — a
/// fall-through would start the session loop on an unnegotiated
/// connection and let a stalled pre-auth client keep its slot (the
/// ADR-0141 handshake timeout's whole job).
#[test]
fn handshake_timeout_mid_versions_starts_no_session() {
    let db = Arc::new(Database::open(":memory:").unwrap());
    let addr = spawn_handler(
        db,
        ConnConfig {
            limits: limits(
                Duration::from_millis(200),
                Duration::from_secs(300),
                DEFAULT_MAX_RESULT_BYTES,
            ),
            ..Default::default()
        },
    );
    let mut client = TcpStream::connect(addr).unwrap();
    // The magic arrives; the versions never do.
    client.write_all(&[0x60, 0x60, 0xB0, 0x17]).unwrap();
    assert_closes(
        &mut client,
        "a client stalled before the version exchange must be closed by the handshake timeout",
    );
    // And no session ever started: a HELLO written afterwards hits a dead
    // socket (write errors or the next read is still EOF).
    let _ = client.write_all(&[0u8; 4]);
    let mut b = [0u8; 1];
    assert_eq!(
        client.read(&mut b).expect("closed read"),
        0,
        "no session may start after an aborted negotiation"
    );
}

/// A client that drips a message one byte per read window survives every
/// per-read timeout — the whole-message deadline (the idle budget shared
/// across the message, re-armed per chunk read) closes it instead.
#[test]
fn idle_timeout_bounds_a_whole_trickled_message() {
    let db = Arc::new(Database::open(":memory:").unwrap());
    let mut client = connected_client_with(
        db,
        ConnConfig {
            limits: limits(
                Duration::from_secs(5),
                Duration::from_millis(500),
                DEFAULT_MAX_RESULT_BYTES,
            ),
            ..Default::default()
        },
    );

    // Two chunks (4 bytes each), one byte per 100 ms — size reads AND body
    // reads are re-armed, so the drip across a chunk boundary cannot
    // outlive the ~500 ms message window. The read guard is armed BEFORE
    // the drip: after the server closes, installing it can fail on some
    // platforms (EINVAL on a broken socket).
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let started = Instant::now();
    client.write_all(&[0x00, 0x04]).unwrap();
    for _ in 0..2 {
        for _ in 0..4 {
            std::thread::sleep(Duration::from_millis(100));
            if client.write_all(&[0x00]).is_err() {
                break;
            }
        }
        if client.write_all(&[0x00, 0x04]).is_err() {
            break; // already closed mid-drip (the second chunk's header)
        }
    }
    // Closed: clean EOF or a reset after the failed drip writes.
    let mut buf = [0u8; 1];
    let closed = client.read(&mut buf);
    assert!(
        matches!(closed, Ok(0) | Err(_)),
        "a message trickled past its whole-message deadline must be closed: {closed:?}"
    );
    let elapsed = started.elapsed();
    // The bound must hold on a loaded runner: the client-side drip itself is
    // 8 × 100 ms of sleeps that scheduling can overshoot, so the old 900 ms
    // bound flaked on the scheduled OS-sweep lanes (Linear 3f4abce4). The
    // invariant it guards — the server closed well before the client's 5 s
    // read timeout, i.e. the trickle could not outlive the message window
    // by minutes — is preserved at 2 s.
    assert!(
        elapsed < Duration::from_secs(2),
        "closed at {elapsed:?}: the trickle bought more than the message window"
    );
}

#[test]
fn idle_timeout_closes_a_client_stalled_mid_chunk() {
    let db = Arc::new(Database::open(":memory:").unwrap());
    let addr = spawn_handler(
        db,
        ConnConfig {
            limits: limits(
                Duration::from_secs(5),
                Duration::from_millis(200),
                DEFAULT_MAX_RESULT_BYTES,
            ),
            ..Default::default()
        },
    );
    let mut client = TcpStream::connect(addr).unwrap();
    crate::testutil::handshake(&mut client);
    // Declare a 10-byte chunk but send only one body byte, then stall.
    client.write_all(&[0u8, 10, 0x00]).unwrap();
    assert_closes(
        &mut client,
        "a client stalled mid-chunk must be closed by the idle timeout",
    );
}

#[test]
fn result_over_cap_fails_naming_the_limit() {
    let db = Arc::new(Database::open(":memory:").unwrap());
    let mut client = connected_client_with(
        db,
        ConnConfig {
            limits: limits(Duration::from_secs(5), Duration::from_secs(5), 20),
            ..Default::default()
        },
    );

    send_run_at(
        &mut client,
        BoltVersion::V4_4,
        "RETURN 'abcdefghijklmnopqrstuvwxyz' AS s",
        None,
    );
    let metadata = failure_map(read_msg(&mut client));
    let message = field(&metadata, "message");
    assert!(
        message.contains("20"),
        "FAILURE must name the byte limit: {message}"
    );
}

/// Rows sharing one large payload (ADR-0170) are small in memory but large
/// in logical bytes. The cap check stops once the total passes the limit, so
/// 20,000 rows × a 100,000-element list (~16 GB logical) fail fast instead
/// of being summed in full, with and without a query timeout (the
/// watchdog in `keepalive::run_statement`). A full sum takes ~20 s per RUN
/// in a debug build; the bound is wall-clock.
#[test]
fn shared_payload_over_cap_fails_fast() {
    for query_timeout in [None, Some(Duration::from_secs(60))] {
        let db = Arc::new(Database::open(":memory:").unwrap());
        let mut client = connected_client_with(
            db,
            ConnConfig {
                limits: ConnLimits {
                    query_timeout,
                    ..limits(Duration::from_secs(5), Duration::from_secs(60), 1 << 20)
                },
                ..Default::default()
            },
        );

        let started = std::time::Instant::now();
        send_run_at(
            &mut client,
            BoltVersion::V4_4,
            "WITH range(1, 100000) AS v \
             UNWIND range(1, 20000) AS i RETURN v",
            None,
        );
        let metadata = failure_map(read_msg(&mut client));
        let elapsed = started.elapsed();
        let message = field(&metadata, "message");
        assert!(
            message.contains("exceeds max-result-bytes limit of 1048576 bytes"),
            "FAILURE must name the byte limit ({query_timeout:?}): {message}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "the cap check must stop once the total exceeds the limit \
             ({query_timeout:?}): took {elapsed:?}"
        );
    }
}

/// A query timeout cancels the running query through the engine's
/// `CancelToken` and the same connection remains usable afterwards
/// (ADR-0110/0111). The cancellation is an execution-phase failure, so the
/// RUN answers SUCCESS and the PULL reports it (ADR-0319).
#[test]
fn query_timeout_cancels_the_run_and_the_session_stays_usable() {
    let db = Arc::new(Database::open(":memory:").unwrap());
    let mut client = connected_client_with(
        db,
        ConnConfig {
            limits: ConnLimits {
                query_timeout: Some(Duration::from_secs(1)),
                ..limits(
                    Duration::from_secs(5),
                    Duration::from_secs(5),
                    DEFAULT_MAX_RESULT_BYTES,
                )
            },
            ..Default::default()
        },
    );

    // Repeated UNWIND + aggregate stages keep memory bounded while doing
    // enough work to exceed the 1s timeout. The engine checks the token
    // during UNWIND row production (every 1024 rows), inside aggregation,
    // and at every clause boundary, so cancellation is observed promptly.
    let stage = "UNWIND range(1, 100000) AS x WITH count(x) AS c ";
    let long_query = format!("{}RETURN c", stage.repeat(400));
    send_run_at(&mut client, BoltVersion::V4_4, &long_query, None);
    success_map(read_msg(&mut client));
    send_pull(&mut client, Message::Pull, vec![("n", Pack::Int(-1))]);

    let metadata = failure_map(read_msg(&mut client));
    let message = field(&metadata, "message");
    assert!(
        message.contains("query cancelled"),
        "FAILURE must report cancellation: {message}"
    );
    // ADR-0307: the engine's typed `Cancelled` maps to Neo4j's timeout code.
    assert_eq!(
        metadata.get("code").and_then(Pack::as_str),
        Some("Neo.ClientError.Transaction.TransactionTimedOut")
    );

    // RESET clears the failed state, then the same connection answers a
    // normal query: the `Connection` survived the cancelled RUN.
    send_struct(&mut client, Message::Reset.signature(), vec![]);
    success_map(read_msg(&mut client));

    send_run_at(&mut client, BoltVersion::V4_4, "RETURN 1 AS n", None);
    success_map(read_msg(&mut client));
    send_pull(&mut client, Message::Pull, vec![("n", Pack::Int(-1))]);
    assert_eq!(
        read_msg(&mut client),
        Pack::Struct {
            tag: 0x71,
            fields: vec![Pack::List(vec![Pack::Int(1)])],
        }
    );
    success_map(read_msg(&mut client));
}

#[test]
fn oversized_message_closes_only_its_connection() {
    let db = Arc::new(Database::open(":memory:").unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let mut joins = Vec::new();
        for _ in 0..2 {
            let (stream, _) = listener.accept().unwrap();
            let db = Arc::clone(&db);
            joins.push(std::thread::spawn(move || {
                let _ = handle_conn(stream, db, ConnConfig::default(), None);
            }));
        }
        for join in joins {
            join.join().unwrap();
        }
    });

    let mut first = crate::testutil::connect(addr);
    crate::testutil::handshake(&mut first);
    let chunk = vec![0; 65535];
    // 256 chunks account for 16,776,960 payload bytes. A 257-byte
    // next chunk crosses the 16 MiB cap; its body is intentionally absent.
    for _ in 0..256 {
        first.write_all(&65535u16.to_be_bytes()).unwrap();
        first.write_all(&chunk).unwrap();
    }
    first.write_all(&257u16.to_be_bytes()).unwrap();
    assert_closes(&mut first, "oversized chunk header must close immediately");

    let mut second = crate::testutil::connect(addr);
    crate::testutil::handshake(&mut second);
    second
        .write_all(
            &try_struct_msg(Message::Hello.signature(), crate::testutil::hello_fields()).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        decode(&read_message(&mut second).unwrap()).unwrap(),
        Pack::Struct { tag: 0x70, .. }
    ));
    // Drop the clients so the server threads see EOF instead of parking on
    // the 300 s default idle timeout.
    drop(second);
    server.join().unwrap();
}
