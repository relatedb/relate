//! The 5.4 session's authentication flow (ADR-0484 decisions 3 and 4): the
//! HELLO that no longer authenticates — `bolt_agent` required, the auth
//! fields and `patch_bolt` ignored, the telemetry hint advertised — the
//! AUTHENTICATION state where only LOGON is answered, LOGON's two schemes
//! and its probed failure, and LOGOFF back to awaiting LOGON. The
//! notifications keys the HELLO also accepts are pinned by the
//! notification module; the telemetry message by the telemetry module.

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::*;
use crate::failures::{FAILED_HELLO_DELAY_MS, REQUEST_INVALID_CODE, UNAUTHORIZED_CODE};
use crate::packstream::Pack;
use crate::requests::Message;
use crate::testutil::{
    assert_breach, assert_breach_failure, assert_closes, assert_ignored,
    assert_ignored_in_failed_state_then_recovers, awaiting_logon_client, connected_client_at,
    failure_map, field, handshaken_at, hello_extra_at, hello_extra_at_overriding, map, read_msg,
    send_logoff, send_logon, send_run, send_struct, success_map,
};
use crate::version::BoltVersion;

/// The basic-auth config the LOGON tests run under.
fn basic_auth() -> Arc<crate::auth::AuthConfig> {
    Arc::new(crate::auth::AuthConfig::basic("relate", "token"))
}

/// ADR-0484 decision 3: a 5.4 HELLO — auth fields and `patch_bolt`
/// included — answers plain SUCCESS with `server`, `connection_id` and
/// both hints, and no `patch_bolt` acknowledgment key; the session is NOT
/// authenticated: a RUN before LOGON is the ADR-0314 breach naming the
/// AUTHENTICATION state, then the close.
#[test]
fn hello_at_5_4_answers_success_and_leaves_the_session_unauthenticated() {
    let mut client = handshaken_at(ConnConfig::default(), BoltVersion::V5_4);
    send_struct(
        &mut client,
        Message::Hello.signature(),
        vec![hello_extra_at(
            BoltVersion::V5_4,
            vec![
                ("scheme", Pack::String("basic".into())),
                ("principal", Pack::String("neo4j".into())),
                ("credentials", Pack::String("wrong".into())),
                ("patch_bolt", Pack::List(vec![Pack::String("utc".into())])),
            ],
        )],
    );
    let hello = success_map(read_msg(&mut client));
    assert_eq!(
        crate::testutil::keys(&hello),
        ["server", "connection_id", "hints"],
        "no patch_bolt key: the patch request is ignored, not acknowledged"
    );
    assert_eq!(
        Pack::Map(hello).get("hints"),
        Some(&map(vec![
            ("connection.recv_timeout_seconds", Pack::Int(120)),
            ("telemetry.enabled", Pack::Bool(false)),
        ]))
    );
    // The HELLO did not authenticate: RUN before LOGON breaches.
    send_run(&mut client, "RETURN 1 AS n");
    assert_breach_failure(&mut client, Message::Run, "AUTHENTICATION");
}

/// ADR-0484 decision 3 and ADR-0313: each required HELLO key with a bad
/// value — absent, null, or the wrong shape — answers the probed
/// `Request.Invalid` failure with its pinned text: `user_agent` exactly
/// as at 4.4 (the 2026.09 probe's code and text), the string-to-string
/// `bolt_agent` map a 5.x requirement. One row per required field, one
/// spelling of the exchange (testutil's `hello_extra_at_overriding`).
#[test]
fn hello_at_5_4_rejects_a_bad_value_for_each_required_field() {
    for (key, bad_values, expected) in [
        (
            "user_agent",
            vec![None, Some(Pack::Null), Some(Pack::Int(4))],
            crate::session::USER_AGENT_REQUIRED,
        ),
        (
            "bolt_agent",
            vec![
                None,
                Some(Pack::Null),
                Some(Pack::String("agent".into())),
                Some(map(vec![("product", Pack::Int(1))])),
            ],
            crate::session::BOLT_AGENT_REQUIRED,
        ),
    ] {
        for value in bad_values {
            let mut client = handshaken_at(ConnConfig::default(), BoltVersion::V5_4);
            send_struct(
                &mut client,
                Message::Hello.signature(),
                vec![hello_extra_at_overriding(
                    BoltVersion::V5_4,
                    key,
                    value.clone(),
                )],
            );
            let failure = failure_map(read_msg(&mut client));
            assert_eq!(
                field(&failure, "code"),
                REQUEST_INVALID_CODE,
                "{key} = {value:?}"
            );
            assert_eq!(field(&failure, "message"), expected, "{key} = {value:?}");
        }
    }
}

/// ADR-0484 decision 3: the rejected HELLO leaves the ADR-0313
/// rejected-HELLO state — IGNORED after, a second HELLO the
/// AUTHENTICATION breach.
#[test]
fn a_rejected_5_4_hello_ignores_work_and_breaches_the_second_hello() {
    let mut client = handshaken_at(ConnConfig::default(), BoltVersion::V5_4);
    send_struct(
        &mut client,
        Message::Hello.signature(),
        vec![hello_extra_at_overriding(
            BoltVersion::V5_4,
            "bolt_agent",
            None,
        )],
    );
    failure_map(read_msg(&mut client));
    send_run(&mut client, "RETURN 1 AS x");
    assert_ignored(&mut client, "RUN after a rejected HELLO");
    assert_breach(
        &mut client,
        Message::Hello,
        vec![hello_extra_at(BoltVersion::V5_4, vec![])],
        "AUTHENTICATION",
    );
}

/// At 5.4 the session opens at LOGON, never at the HELLO — so even under
/// `--auth none` a 5.4 connection requires the HELLO first (the 4.4
/// auth-none connection starts open; the 5.4 one never does).
#[test]
fn a_5_4_connection_requires_hello_even_under_auth_none() {
    let mut client = handshaken_at(ConnConfig::default(), BoltVersion::V5_4);
    send_run(&mut client, "RETURN 1 AS x");
    let failure = failure_map(read_msg(&mut client));
    assert_eq!(field(&failure, "code"), UNAUTHORIZED_CODE);
    assert_closes(&mut client, "the pre-HELLO gate closes");
}

/// ADR-0484 decision 4: LOGON with the configured basic credentials
/// answers SUCCESS `{}` and opens the session — a RUN runs afterwards.
#[test]
fn logon_basic_opens_the_session() {
    let mut client = awaiting_logon_client(ConnConfig {
        auth: basic_auth(),
        ..ConnConfig::default()
    });
    send_logon(
        &mut client,
        vec![
            ("scheme", Pack::String("basic".into())),
            ("principal", Pack::String("relate".into())),
            ("credentials", Pack::String("token".into())),
        ],
    );
    assert_eq!(success_map(read_msg(&mut client)), vec![]);
    send_run(&mut client, "RETURN 1 AS n");
    success_map(read_msg(&mut client));
    crate::testutil::drain_all(&mut client, Message::Pull);
}

/// ADR-0484 decision 4: a LOGON that does not authenticate answers the
/// probed Unauthorized code and text — after the ADR-0139 guessing delay —
/// and the connection is DEFUNCT: closed, nothing after it answered.
#[test]
fn logon_with_wrong_credentials_fails_and_closes() {
    let mut client = awaiting_logon_client(ConnConfig {
        auth: basic_auth(),
        ..ConnConfig::default()
    });
    let started = Instant::now();
    send_logon(
        &mut client,
        vec![
            ("scheme", Pack::String("basic".into())),
            ("principal", Pack::String("relate".into())),
            ("credentials", Pack::String("wrongpass123".into())),
        ],
    );
    let failure = failure_map(read_msg(&mut client));
    assert_eq!(field(&failure, "code"), UNAUTHORIZED_CODE);
    assert_eq!(
        field(&failure, "message"),
        crate::session::LOGON_UNAUTHORIZED
    );
    assert!(
        started.elapsed() >= Duration::from_millis(FAILED_HELLO_DELAY_MS),
        "the ADR-0139 delay travels with the credential check"
    );
    assert_closes(&mut client, "a failed LOGON makes the connection DEFUNCT");
}

/// ADR-0484 decision 4: scheme `none` — the explicit no-auth scheme —
/// authenticates under `--auth none` (where every LOGON does, as every
/// 4.4 HELLO does today) and only there: under configured basic auth it
/// is the probed Unauthorized failure and the close.
#[test]
fn logon_scheme_none_authenticates_only_under_auth_none() {
    let mut client = awaiting_logon_client(ConnConfig::default());
    send_logon(&mut client, vec![("scheme", Pack::String("none".into()))]);
    assert_eq!(success_map(read_msg(&mut client)), vec![]);
    send_run(&mut client, "RETURN 1 AS n");
    success_map(read_msg(&mut client));

    let mut client = awaiting_logon_client(ConnConfig {
        auth: basic_auth(),
        ..ConnConfig::default()
    });
    send_logon(&mut client, vec![("scheme", Pack::String("none".into()))]);
    let failure = failure_map(read_msg(&mut client));
    assert_eq!(field(&failure, "code"), UNAUTHORIZED_CODE);
    assert_eq!(
        field(&failure, "message"),
        crate::session::LOGON_UNAUTHORIZED
    );
    assert_closes(
        &mut client,
        "scheme none does not authenticate under basic auth",
    );
}

/// ADR-0484 decision 4: LOGOFF from an open session answers SUCCESS `{}`
/// and returns to awaiting LOGON — a RUN breaches in the AUTHENTICATION
/// state, and a LOGON opens the session again.
#[test]
fn logoff_returns_the_session_to_awaiting_logon() {
    let mut client = connected_client_at(BoltVersion::V5_4);
    send_logoff(&mut client);
    assert_eq!(success_map(read_msg(&mut client)), vec![]);
    send_run(&mut client, "RETURN 1 AS n");
    assert_breach_failure(&mut client, Message::Run, "AUTHENTICATION");
}

/// The LOGOFF / LOGON cycle: after LOGOFF a fresh LOGON re-opens the
/// session and work runs again.
#[test]
fn a_fresh_logon_reopens_the_session_after_logoff() {
    let mut client = connected_client_at(BoltVersion::V5_4);
    send_logoff(&mut client);
    success_map(read_msg(&mut client));
    send_logon(&mut client, vec![("scheme", Pack::String("none".into()))]);
    assert_eq!(success_map(read_msg(&mut client)), vec![]);
    send_run(&mut client, "RETURN 1 AS n");
    success_map(read_msg(&mut client));
    crate::testutil::drain_all(&mut client, Message::Pull);
}

/// The failed state IGNOREs LOGOFF (this feature's contract — Neo4j
/// 2026.09 answers a FAILURE there, the probe's `logoff-in-failed-state`
/// finding): the shared ignore-then-recover exchange, which the
/// telemetry module drives over TELEMETRY for the same rule.
#[test]
fn logoff_in_the_failed_state_answers_ignored() {
    let mut client = connected_client_at(BoltVersion::V5_4);
    assert_ignored_in_failed_state_then_recovers(&mut client, Message::Logoff, vec![]);
}

/// ADR-0484 decision 4 (the 5.1 state table): LOGON is received in the
/// AUTHENTICATION state only — from an open session it is the ADR-0314
/// breach naming the session's work state, then the close.
#[test]
fn logon_from_an_open_session_is_a_breach() {
    let mut client = connected_client_at(BoltVersion::V5_4);
    send_logon(&mut client, vec![("scheme", Pack::String("none".into()))]);
    assert_breach_failure(&mut client, Message::Logon, "READY");
}

/// While the session awaits LOGON, every message but LOGON and GOODBYE is
/// the AUTHENTICATION breach (the Bolt 5.1 state machine's AUTHENTICATION
/// state accepts only LOGON) — RUN is criterion-pinned through its
/// canonical sender, the rest follow the same gate.
#[test]
fn only_logon_is_answered_while_awaiting_logon() {
    let mut client = awaiting_logon_client(ConnConfig::default());
    send_run(&mut client, "RETURN 1 AS n");
    assert_breach_failure(&mut client, Message::Run, "AUTHENTICATION");
    for (message, fields) in [
        (Message::Reset, vec![]),
        (
            Message::Route,
            vec![map(vec![]), Pack::List(vec![]), map(vec![])],
        ),
        (Message::Logoff, vec![]),
        (Message::Telemetry, vec![Pack::Int(0)]),
    ] {
        let mut client = awaiting_logon_client(ConnConfig::default());
        assert_breach(&mut client, message, fields, "AUTHENTICATION");
    }
}
