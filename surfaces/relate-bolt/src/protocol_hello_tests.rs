//! HELLO: the receive-timeout hint and the NOOP keep-alive behind it
//! (ADR-0312), and the required `user_agent` (ADR-0313).

use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::*;
use crate::failures::{FAILED_HELLO_DELAY_MS, REQUEST_INVALID_CODE};
use crate::packstream::Pack;
use crate::requests::Message;
use crate::testutil::{
    assert_closes, assert_ignored, failure_map, field, handshaken, map, read_msg, send_pull,
    send_reset, send_run, send_struct, success_map,
};

/// F-07: HELLO SUCCESS is server, connection_id and the default
/// `connection.recv_timeout_seconds` hint of 120.
#[test]
fn hello_success_advertises_the_recv_timeout_hint() {
    let mut client = handshaken(ConnConfig::default());
    send_struct(
        &mut client,
        Message::Hello.signature(),
        crate::testutil::hello_fields(),
    );
    let hello = success_map(read_msg(&mut client));
    assert_eq!(
        crate::testutil::keys(&hello),
        ["server", "connection_id", "hints"]
    );
    assert_eq!(
        Pack::Map(hello).get("hints"),
        Some(&map(vec![(
            "connection.recv_timeout_seconds",
            Pack::Int(120)
        )]))
    );
}

/// The hint is twice the keep-alive interval, in whole seconds rounded up.
#[test]
fn the_hint_is_twice_the_keep_alive_interval() {
    use crate::summary::recv_timeout_hint;
    assert_eq!(
        recv_timeout_hint(crate::keepalive::DEFAULT_KEEP_ALIVE_INTERVAL),
        120
    );
    assert_eq!(recv_timeout_hint(Duration::from_secs(7)), 14);
    assert_eq!(recv_timeout_hint(Duration::from_millis(50)), 1);
}

/// Read one message's chunks raw, counting the NOOP chunks (`00 00`) the
/// server writes before it. The chunk rule lives in
/// [`crate::framing::read_message_counting_noops`].
fn read_counting_noops(client: &mut TcpStream) -> (usize, Pack) {
    let (noops, payload) = crate::framing::read_message_counting_noops(client).unwrap();
    (noops, crate::packstream::decode(&payload).unwrap())
}

/// ADR-0312: a RUN that outlives the keep-alive interval gets NOOP chunks
/// before its reply; a fast RUN gets none.
#[test]
fn a_long_run_gets_noop_keep_alives_before_its_reply() {
    let mut client = handshaken(ConnConfig {
        limits: ConnLimits {
            keep_alive_interval: Duration::from_millis(20),
            ..ConnLimits::default()
        },
        ..ConnConfig::default()
    });
    send_struct(
        &mut client,
        Message::Hello.signature(),
        crate::testutil::hello_fields(),
    );
    let (noops, hello) = read_counting_noops(&mut client);
    assert_eq!(noops, 0);
    success_map(hello);

    let stage = "UNWIND range(1, 100000) AS x WITH count(x) AS c ";
    let started = Instant::now();
    send_run(&mut client, &format!("{}RETURN c", stage.repeat(60)));
    let (noops, run) = read_counting_noops(&mut client);
    let took = started.elapsed();
    success_map(run);
    assert!(
        noops >= 1,
        "a RUN taking {took:?} past a 20 ms interval must get NOOPs"
    );

    send_pull(&mut client, Message::Pull, vec![("n", Pack::Int(-1))]);
    let (noops, record) = read_counting_noops(&mut client);
    assert_eq!(noops, 0, "PULL streams a buffer and never waits");
    assert!(matches!(record, Pack::Struct { tag: 0x71, .. }));
}

/// F-09, F-68, ADR-0342: a HELLO without a non-empty string `user_agent` is
/// rejected with Neo4j's code and text; the session stays closed (IGNORED),
/// a second HELLO is a breach in the AUTHENTICATION state and closes.
#[test]
fn hello_without_user_agent_is_rejected() {
    for user_agent in [
        None,
        Some(Pack::Null),
        Some(Pack::Int(4)),
        Some(Pack::String(String::new())),
    ] {
        let mut client = handshaken(ConnConfig::default());
        let mut extra = vec![(Pack::String("scheme".into()), Pack::String("none".into()))];
        if let Some(value) = user_agent.clone() {
            extra.push((Pack::String("user_agent".into()), value));
        }
        send_struct(
            &mut client,
            Message::Hello.signature(),
            vec![Pack::Map(extra)],
        );
        let failure = failure_map(read_msg(&mut client));
        assert_eq!(
            field(&failure, "code"),
            REQUEST_INVALID_CODE,
            "{user_agent:?}"
        );
        assert_eq!(
            field(&failure, "message"),
            "Illegal value for field \"user_agent\": Expected value to be non-null"
        );
        send_reset(&mut client);
        assert_ignored(&mut client, "RESET after a rejected HELLO");
        send_run(&mut client, "RETURN 1 AS x");
        assert_ignored(&mut client, "RUN after a rejected HELLO");
        send_struct(
            &mut client,
            Message::Hello.signature(),
            crate::testutil::hello_fields(),
        );
        let failure = failure_map(read_msg(&mut client));
        assert_eq!(field(&failure, "code"), REQUEST_INVALID_CODE);
        assert_eq!(
            field(&failure, "message"),
            "Message of type HelloMessage cannot be handled by a session in the AUTHENTICATION state."
        );
        assert_closes(&mut client, "a second HELLO after a rejected one closes");
    }
}

/// GOODBYE after a rejected HELLO closes the connection.
#[test]
fn goodbye_after_a_rejected_hello_closes() {
    let mut client = handshaken(ConnConfig::default());
    send_struct(&mut client, Message::Hello.signature(), vec![map(vec![])]);
    failure_map(read_msg(&mut client));
    send_struct(&mut client, Message::Goodbye.signature(), vec![]);
    assert_closes(&mut client, "GOODBYE closes");
}

/// The `user_agent` check runs before the ADR-0126 auth check: under basic
/// auth a HELLO without one is the Request.Invalid FAILURE, at once (no
/// ADR-0139 delay), not Unauthorized.
#[test]
fn user_agent_is_checked_before_auth() {
    let mut client = handshaken(ConnConfig {
        auth: Arc::new(crate::auth::AuthConfig::basic("relate", "token")),
        ..ConnConfig::default()
    });
    let started = Instant::now();
    send_struct(
        &mut client,
        Message::Hello.signature(),
        vec![map(vec![
            ("scheme", Pack::String("basic".into())),
            ("principal", Pack::String("relate".into())),
            ("credentials", Pack::String("wrong".into())),
        ])],
    );
    let failure = failure_map(read_msg(&mut client));
    assert_eq!(field(&failure, "code"), REQUEST_INVALID_CODE);
    assert!(started.elapsed() < Duration::from_millis(FAILED_HELLO_DELAY_MS / 2));
}
