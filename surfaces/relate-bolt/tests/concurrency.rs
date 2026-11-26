//! End-to-end concurrency and cancellation tests (ADR-0233): the real
//! binary, real sockets, and real `std::thread`s. These drive raw TCP Bolt
//! only — no WebSocket — because they exercise the sync core's concurrency
//! and cancellation, which is transport-agnostic.

mod common;

use common::packstream::{decode, Pack};
use common::{
    connect_and_hello, is_success, read_message, run_and_pull, send, start_server,
    try_connect_and_hello,
};
use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

/// The read guard on test client sockets: a guard against a wedged server,
/// never the behavior under test.
const GUARD: Duration = Duration::from_secs(30);

/// Connect, negotiate Bolt 4.4, and complete HELLO on `addr`, retrying until
/// `deadline` passes — the server-side slot release after a disconnect or a
/// freed cap slot is asynchronous, so a single attempt can race it.
/// Panics when no session is served before the deadline.
fn hello_with_retry(addr: &str, deadline: Duration, what: &str) -> TcpStream {
    let until = Instant::now() + deadline;
    loop {
        match try_connect_and_hello(addr) {
            Ok(session) => return session,
            Err(_) => {
                assert!(Instant::now() < until, "{what}");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

/// RUN `RETURN <n> AS marker` and PULL; asserts the single RECORD carries
/// exactly `n` — the per-connection isolation probe.
fn run_return(client: &mut TcpStream, n: i64) {
    let records = run_and_pull(client, &format!("RETURN {n} AS marker"), vec![]);
    assert_eq!(
        records,
        vec![vec![Pack::Int(n)]],
        "the record must be this connection's own marker {n}"
    );
}

/// Poll the socket until the peer closes it (clean EOF or a reset) or
/// `deadline` passes. Dropping / closure detection is cross-platform.
fn wait_closed(stream: &mut TcpStream, deadline: Duration) -> bool {
    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let until = Instant::now() + deadline;
    let mut buf = [0u8; 1];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => return true, // clean EOF: the server closed
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                if Instant::now() >= until {
                    return false;
                }
            }
            Err(_) => return true, // connection reset: also a closure
            Ok(_) => {}            // an unexpected byte; keep polling
        }
    }
}

/// A client that trickles one byte per read window through the version
/// exchange must die at the ADR-0141 deadline: each negotiation read gets
/// the *remaining* budget, so the wall clock — not the drip rate — closes
/// it. (The silent-client case is covered by the in-crate
/// `handshake_timeout_closes_a_silent_client`.)
#[test]
fn trickling_negotiation_is_closed_at_the_deadline() {
    let server = start_server(&[
        "--uri",
        ":memory:",
        "--auth",
        "none",
        "--handshake-timeout-secs",
        "1",
        "--idle-timeout-secs",
        "30",
        "--no-websocket",
    ]);
    let mut client = TcpStream::connect(&server.addr).unwrap();
    let started = Instant::now();
    // The magic arrives one byte at a time, each byte inside its read
    // window; then the versions stall entirely.
    for byte in [0x60u8, 0x60, 0xB0, 0x17] {
        client.write_all(&[byte]).unwrap();
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        wait_closed(&mut client, Duration::from_secs(5)),
        "a trickling negotiation must be closed at the handshake deadline"
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(10),
        "the deadline is 1 s; the trickle must not buy more: {elapsed:?}"
    );
}

/// A query that runs for seconds in a debug build: repeated UNWIND +
/// aggregate stages keep memory bounded while doing enough work. The engine
/// checks its CancelToken during UNWIND row production (every 1024 rows),
/// inside aggregation, and at every clause boundary.
fn long_query() -> String {
    let stage = "UNWIND range(1, 100000) AS x WITH count(x) AS c ";
    format!("{}RETURN c", stage.repeat(400))
}

/// 16 concurrent clients, each holding one session and issuing interleaved
/// pipelined RUN/DISCARD/RUN/PULL streams. Every connection must get exactly
/// its own results, in its own message order — no cross-talk.
#[test]
fn parallel_pipelined_sessions_are_isolated() {
    let server = start_server(&["--auth", "none"]);
    const CLIENTS: i64 = 16;
    const ROUNDS: i64 = 8;
    let mut handles = Vec::new();
    for c in 0..CLIENTS {
        let addr = server.addr.clone();
        handles.push(std::thread::spawn(move || {
            let mut session = connect_and_hello(&addr);
            for r in 0..ROUNDS {
                let base = c * 10_000 + r * 3;
                // Pipeline the whole round before reading any reply: two
                // RUNs (the second replaces the first's pending result), a
                // DISCARD (clears the second), then a RUN + PULL.
                for marker in [base, base + 1] {
                    send(
                        &mut session,
                        0x10,
                        vec![
                            Pack::String(format!("RETURN {marker} AS marker")),
                            Pack::Map(vec![]),
                        ],
                    );
                }
                send(&mut session, 0x2F, vec![Pack::Map(vec![])]);
                send(
                    &mut session,
                    0x10,
                    vec![
                        Pack::String(format!("RETURN {} AS marker", base + 2)),
                        Pack::Map(vec![]),
                    ],
                );
                send(
                    &mut session,
                    0x3F,
                    vec![Pack::Map(vec![(Pack::String("n".into()), Pack::Int(-1))])],
                );
                // Per-connection reply order: SUCCESS × 4 (RUN, RUN,
                // DISCARD, RUN), then the PULL's RECORD + SUCCESS.
                for which in 0..4 {
                    let msg = read_message(&mut session);
                    assert!(
                        is_success(&msg),
                        "reply {which} of client {c} round {r} must be SUCCESS: {msg:?}"
                    );
                }
                let record = read_message(&mut session);
                match decode(&record).unwrap_or_else(|e| panic!("RECORD decodes: {e}")) {
                    Pack::Struct { tag: 0x71, fields } => {
                        assert_eq!(
                            fields,
                            vec![Pack::List(vec![Pack::Int(base + 2)])],
                            "client {c} round {r} must read its own marker"
                        );
                    }
                    other => panic!("client {c} round {r}: expected RECORD, got {other:?}"),
                }
                assert!(
                    is_success(&read_message(&mut session)),
                    "PULL summary of client {c} round {r}"
                );
            }
        }));
    }
    for h in handles {
        h.join().expect("a pipelined client panicked");
    }
}

/// A client drops its socket mid-query. The server must notice (the
/// connection slot returns to the cap), keep answering, and keep its store
/// intact — no panic, no leaked slot, no durability damage.
#[test]
fn client_disconnect_mid_query_frees_the_slot_and_the_server_survives() {
    // Memory tier (ADR-0251): the store location is incidental — the test
    // observes slot release and write/read-back on the one server's store,
    // never a path — so it runs on the in-memory VFS.
    // Cap of 2: with the dropped connection holding one slot, two fresh
    // sessions can both be served only once the drop is noticed.
    let server = start_server(&[
        "--uri",
        ":memory:",
        "--auth",
        "none",
        "--max-connections",
        "2",
    ]);

    let mut dropped = connect_and_hello(&server.addr);
    send(
        &mut dropped,
        0x10,
        vec![Pack::String(long_query()), Pack::Map(vec![])],
    );
    drop(dropped); // abrupt disconnect, cross-platform

    // The slot must return to the cap: two fresh full sessions (handshake +
    // HELLO + query) must succeed, retrying until the asynchronous
    // server-side slot release lands.
    for served in 0..2 {
        let mut session = hello_with_retry(
            &server.addr,
            Duration::from_secs(90),
            "the dropped connection's slot never returned to the cap",
        );
        run_return(&mut session, served);
    }

    // Durability intact: a write and a read-back on the server's store
    // both succeed after the mid-flight disconnect. This session retries
    // too — the disconnect probe above dropped its own sessions, and their
    // server-side teardown is just as asynchronous.
    let mut check = hello_with_retry(
        &server.addr,
        Duration::from_secs(30),
        "the server stopped serving after the mid-query disconnect",
    );
    let created = run_and_pull(&mut check, "CREATE (:Marker {v: 7})", vec![]);
    assert!(created.is_empty(), "CREATE returns no records");
    let read_back = run_and_pull(&mut check, "MATCH (m:Marker) RETURN m.v AS v", vec![]);
    assert_eq!(read_back, vec![vec![Pack::Int(7)]]);

    // No panic reached the server: the in-process harness (LABS-381, ADR-0368) cannot
    // capture the serve thread's stderr — the old spawn asserted no
    // "panicked" line there; the equivalent in-process signal is the server
    // still serving, which the read-back above just proved (a panicked
    // connection thread would not break the listener, but a panicked serve
    // loop ends it — and `hello_with_retry` above reconnects through that
    // same listener).
}

/// The idle timeout closes a stalled session and no other connection.
#[test]
fn idle_timeout_closes_only_the_stalled_session() {
    let server = start_server(&["--auth", "none", "--idle-timeout-secs", "2"]);
    let mut stalled = connect_and_hello(&server.addr);
    // A concurrent session works while the stalled one is idle.
    let mut active = connect_and_hello(&server.addr);
    run_return(&mut active, 1);
    drop(active);

    // The stalled session must be closed within a generous multiple of the
    // 2 s deadline.
    assert!(
        wait_closed(&mut stalled, Duration::from_secs(10)),
        "the stalled session must be closed by the idle timeout"
    );

    // The server is unaffected: fresh sessions are still served.
    let mut fresh = connect_and_hello(&server.addr);
    run_return(&mut fresh, 2);
}

/// A client that connects and sends nothing is closed at the negotiation
/// deadline; the server keeps serving.
#[test]
fn handshake_timeout_closes_a_silent_client() {
    let server = start_server(&["--auth", "none", "--handshake-timeout-secs", "2"]);
    let mut silent = TcpStream::connect(&server.addr).unwrap();
    assert!(
        wait_closed(&mut silent, Duration::from_secs(10)),
        "a client that never negotiates must be closed at the handshake deadline"
    );
    let mut fresh = connect_and_hello(&server.addr);
    run_return(&mut fresh, 1);
}

/// The query timeout cancels a running query with an interrupted FAILURE
/// (ADR-0111), and the same session stays usable afterwards. Cancellation
/// happens while the planned statement executes, so the RUN answers SUCCESS
/// and the PULL reports it (ADR-0319).
#[test]
fn query_timeout_interrupts_and_the_session_stays_usable() {
    let server = start_server(&["--auth", "none", "--query-timeout-secs", "1"]);
    let mut session = connect_and_hello(&server.addr);

    send(
        &mut session,
        0x10,
        vec![Pack::String(long_query()), Pack::Map(vec![])],
    );
    assert!(
        is_success(&read_message(&mut session)),
        "the RUN of a statement cancelled while executing succeeds"
    );
    send(
        &mut session,
        0x3F,
        vec![Pack::Map(vec![(Pack::String("n".into()), Pack::Int(-1))])],
    );
    let msg = read_message(&mut session);
    let Pack::Struct { tag, fields } =
        decode(&msg).unwrap_or_else(|e| panic!("FAILURE decodes: {e}"))
    else {
        panic!("a timed-out query must fail with a struct message: {msg:?}");
    };
    assert_eq!(tag, 0x7F, "a timed-out query must be a FAILURE: {msg:?}");
    let message = fields
        .first()
        .and_then(|m| m.get("message"))
        .and_then(Pack::as_str)
        .unwrap_or_default();
    assert!(
        message.contains("query timed out after 1s"),
        "the FAILURE must report the timeout: {message}"
    );

    // RESET clears the failed state; the same session answers a follow-up.
    send(&mut session, 0x0F, vec![]);
    assert!(
        is_success(&read_message(&mut session)),
        "RESET after a timed-out query"
    );
    run_return(&mut session, 1);
}

/// With `--max-connections 4`: four sessions are served, the fifth is
/// accepted then immediately closed, and the cap recovers as sessions end.
#[test]
fn connection_cap_rejects_over_cap_and_recovers_e2e() {
    let server = start_server(&["--auth", "none", "--max-connections", "4"]);

    let mut held: Vec<TcpStream> = (0..4).map(|_| connect_and_hello(&server.addr)).collect();

    // The fifth connection is accepted at the TCP level, then closed before
    // any version selection is written. A prompt close can race the probe's
    // own writes (EPIPE/ECONNRESET on the way in), so a write error is
    // ignored and the read below is the single verdict: an immediate EOF or
    // reset is the required close.
    let mut fifth = TcpStream::connect(&server.addr).unwrap();
    fifth.set_read_timeout(Some(GUARD)).unwrap();
    let _ = fifth
        .write_all(&common::MAGIC)
        .and_then(|()| fifth.write_all(&common::VERSION_4_4));
    let mut selected = [0u8; 4];
    match fifth.read_exact(&mut selected) {
        Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
            panic!("an over-cap connection must be closed promptly, not left open")
        }
        Err(_) => {} // EOF or reset: closed as required
        Ok(()) => panic!("an over-cap connection must not be served, got {selected:?}"),
    }

    // The held sessions keep being served while the probe is gone.
    run_return(&mut held[0], 1);

    // As a session ends, the cap recovers: retry until a new session is
    // served, bounded by a deadline.
    drop(held.pop());
    let mut session = hello_with_retry(
        &server.addr,
        Duration::from_secs(30),
        "the cap never recovered after a session ended",
    );
    run_return(&mut session, 2);
}
