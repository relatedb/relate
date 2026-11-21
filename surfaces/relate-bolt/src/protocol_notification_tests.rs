//! The Bolt 5.2 notifications filter keys at 5.4 (ADR-0484 decision 8):
//! `notifications_minimum_severity` and `notifications_disabled_categories`
//! accepted in the HELLO, BEGIN and RUN extras, filtering the ending
//! SUCCESS notifications list (ADR-0363) — severity threshold including
//! `OFF`, category exclusion — with the probed `Request.Invalid` failures
//! for values that do not parse. At 4.4 the keys are ignored, exactly as
//! every unknown extra key is today.

use std::net::TcpStream;

use super::*;
use crate::failures::REQUEST_INVALID_CODE;
use crate::fields::{NOTIFICATIONS_DISABLED_CATEGORIES, NOTIFICATIONS_MINIMUM_SEVERITY};
use crate::packstream::Pack;
use crate::requests::Message;
use crate::testutil::{
    connected_client, connected_client_at_with, failure_map, field, handshaken, hello_extra,
    hello_extra_at, keys, read_msg, send_begin, send_commit, send_reset, send_rollback,
    send_run_with, send_struct, success_map, two_label_db, TWO_LABEL_CARTESIAN,
};
use crate::version::BoltVersion;

/// The WARNING/UNRECOGNIZED trigger: a node pattern naming a label the
/// catalog does not hold.
const UNKNOWN_LABEL: &str = "EXPLAIN MATCH (n:NotALabel) RETURN n";

/// The `notifications_minimum_severity` extra entry the config tests
/// share, in the key's one spelling.
fn severity(value: &str) -> (&'static str, Pack) {
    (NOTIFICATIONS_MINIMUM_SEVERITY, Pack::String(value.into()))
}

/// The `notifications_disabled_categories` extra entry, from the listed
/// free strings.
fn categories(values: &[&str]) -> (&'static str, Pack) {
    (
        NOTIFICATIONS_DISABLED_CATEGORIES,
        Pack::List(
            values
                .iter()
                .map(|value| Pack::String((*value).into()))
                .collect(),
        ),
    )
}

/// An open 5.4 session (HELLO + LOGON, ADR-0484) against the shared
/// two-label store ([`two_label_db`]) the triggers read, whose HELLO
/// carried `entries` beside the required fields: the fixture for the
/// HELLO-level notifications config.
fn open_client_with(entries: Vec<(&str, Pack)>) -> TcpStream {
    connected_client_at_with(two_label_db(), BoltVersion::V5_4, entries)
}

/// Run one EXPLAIN trigger and drain it: whether its ending SUCCESS
/// carries the `notifications` key — the one fact every filter test reads.
fn ending_has_notifications(
    client: &mut TcpStream,
    cypher: &str,
    entries: Vec<(&str, Pack)>,
) -> bool {
    send_run_with(client, cypher, Some(entries));
    success_map(read_msg(client));
    let (_, ending) = crate::testutil::drain_all(client, Message::Pull);
    ending.get("notifications").is_some()
}

/// The probed HELLO rejection: an invalid severity in the HELLO extra
/// answers `Neo.ClientError.Request.Invalid` with the pinned text — the
/// parse failure riding as the quoted field name — into the rejected-HELLO
/// state (ADR-0313): the connection stays, everything after is IGNORED.
#[test]
fn an_invalid_severity_in_hello_answers_the_probed_failure() {
    let mut client = crate::testutil::handshaken_at(ConnConfig::default(), BoltVersion::V5_4);
    send_struct(
        &mut client,
        Message::Hello.signature(),
        vec![hello_extra_at(BoltVersion::V5_4, vec![severity("INVALID")])],
    );
    let failure = failure_map(read_msg(&mut client));
    assert_eq!(field(&failure, "code"), REQUEST_INVALID_CODE);
    assert_eq!(
        field(&failure, "message"),
        "Illegal value for field \"Could not parse a NotificationConfig's minimum severity.\""
    );
    crate::testutil::send_run(&mut client, "RETURN 1 AS x");
    crate::testutil::assert_ignored(&mut client, "the HELLO was rejected, no session is open");
}

/// The probed RUN rejection: the same invalid value in a RUN extra answers
/// the same code with the text nested under the extra's `metadata` field
/// name, and the failed state follows (nothing executes).
#[test]
fn an_invalid_severity_in_run_extra_answers_the_probed_failure() {
    let mut client = open_client_with(vec![]);
    send_run_with(
        &mut client,
        "RETURN 1 AS n",
        Some(vec![severity("INVALID")]),
    );
    let failure = failure_map(read_msg(&mut client));
    assert_eq!(field(&failure, "code"), REQUEST_INVALID_CODE);
    assert_eq!(
        field(&failure, "message"),
        "Illegal value for field \"metadata\": Illegal value for field \"Could not parse a \
         NotificationConfig's minimum severity.\""
    );
    crate::testutil::send_run(&mut client, "RETURN 1 AS n");
    crate::testutil::assert_ignored(&mut client, "the failed state follows the failure");
    send_reset(&mut client);
    success_map(read_msg(&mut client));
}

/// The BEGIN rejection: the same value in a BEGIN extra answers the RUN
/// spelling — the metadata-nested text (unpinned for BEGIN by the probe;
/// the RUN form is its one analogue) — and nothing begins.
#[test]
fn an_invalid_severity_in_begin_extra_answers_the_probed_failure() {
    let mut client = open_client_with(vec![]);
    send_begin(&mut client, vec![severity("INVALID")]);
    let failure = failure_map(read_msg(&mut client));
    assert_eq!(field(&failure, "code"), REQUEST_INVALID_CODE);
    assert_eq!(
        field(&failure, "message"),
        "Illegal value for field \"metadata\": Illegal value for field \"Could not parse a \
         NotificationConfig's minimum severity.\""
    );
    send_reset(&mut client);
    success_map(read_msg(&mut client));
    // Nothing began: a COMMIT is the no-transaction breach.
    send_commit(&mut client);
    crate::testutil::assert_breach_failure(&mut client, Message::Commit, "READY");
}

/// A wrong-typed value fails the same way (ADR-0484 decision 8: "a
/// wrong-typed value or an unknown severity"): a non-string severity and a
/// categories value that is not a list of strings answer their parse
/// failures.
#[test]
fn a_wrong_typed_value_fails_the_request() {
    let mut client = crate::testutil::handshaken_at(ConnConfig::default(), BoltVersion::V5_4);
    send_struct(
        &mut client,
        Message::Hello.signature(),
        vec![hello_extra_at(
            BoltVersion::V5_4,
            vec![(NOTIFICATIONS_MINIMUM_SEVERITY, Pack::Int(4))],
        )],
    );
    let failure = failure_map(read_msg(&mut client));
    assert_eq!(
        field(&failure, "message"),
        "Illegal value for field \"Could not parse a NotificationConfig's minimum severity.\""
    );

    let mut client = open_client_with(vec![]);
    send_run_with(
        &mut client,
        "RETURN 1 AS n",
        Some(vec![(
            NOTIFICATIONS_DISABLED_CATEGORIES,
            Pack::String("PERFORMANCE".into()),
        )]),
    );
    let failure = failure_map(read_msg(&mut client));
    assert_eq!(
        field(&failure, "message"),
        "Illegal value for field \"metadata\": Illegal value for field \"Could not parse a \
         NotificationConfig's disabled categories.\""
    );
    // A list entry that is not a string fails the same way — after RESET
    // clears the failed state the first failure left.
    send_reset(&mut client);
    success_map(read_msg(&mut client));
    send_run_with(
        &mut client,
        "RETURN 1 AS n",
        Some(vec![(
            NOTIFICATIONS_DISABLED_CATEGORIES,
            Pack::List(vec![Pack::Int(1)]),
        )]),
    );
    let failure = failure_map(read_msg(&mut client));
    assert_eq!(
        field(&failure, "message"),
        "Illegal value for field \"metadata\": Illegal value for field \"Could not parse a \
         NotificationConfig's disabled categories.\""
    );
}

/// The severity threshold (ADR-0484 decision 8): `INFORMATION` keeps both
/// severities, `WARNING` drops the INFORMATION cartesian and keeps the
/// WARNING unknown-label, `OFF` drops the whole list. Unset, the default
/// filters nothing.
#[test]
fn the_minimum_severity_threshold_filters_the_ending_list() {
    let mut client = open_client_with(vec![]);
    assert!(ending_has_notifications(
        &mut client,
        TWO_LABEL_CARTESIAN,
        vec![]
    ));
    assert!(ending_has_notifications(&mut client, UNKNOWN_LABEL, vec![]));

    for (value, cartesian_kept, warning_kept) in [
        ("INFORMATION", true, true),
        ("WARNING", false, true),
        ("OFF", false, false),
    ] {
        let mut client = open_client_with(vec![severity(value)]);
        assert_eq!(
            ending_has_notifications(&mut client, TWO_LABEL_CARTESIAN, vec![]),
            cartesian_kept,
            "minimum_severity {value}: the cartesian (INFORMATION)"
        );
        assert_eq!(
            ending_has_notifications(&mut client, UNKNOWN_LABEL, vec![]),
            warning_kept,
            "minimum_severity {value}: the unknown label (WARNING)"
        );
    }
}

/// The category exclusion: a listed category's notifications drop, a free
/// string naming no engine category (Neo4j's wider `HINT`) filters
/// nothing, and an unset key disables no category.
#[test]
fn the_disabled_categories_filter_the_ending_list() {
    for (disabled, cartesian_kept, warning_kept) in [
        (vec!["PERFORMANCE"], false, true),
        (vec!["UNRECOGNIZED"], true, false),
        (vec!["HINT"], true, true),
    ] {
        let mut client = open_client_with(vec![categories(&disabled)]);
        assert_eq!(
            ending_has_notifications(&mut client, TWO_LABEL_CARTESIAN, vec![]),
            cartesian_kept,
            "disabled {disabled:?}: the cartesian (PERFORMANCE)"
        );
        assert_eq!(
            ending_has_notifications(&mut client, UNKNOWN_LABEL, vec![]),
            warning_kept,
            "disabled {disabled:?}: the unknown label (UNRECOGNIZED)"
        );
    }
}

/// A statement's keys override the HELLO's, and a null or absent key
/// falls back to it (ADR-0484 decision 8): over a HELLO's `OFF`, a RUN
/// may reopen the list per-statement — or fall back to the closed one.
#[test]
fn run_extra_keys_override_the_hello_config_and_null_falls_back() {
    let mut client = open_client_with(vec![severity("OFF")]);
    assert!(ending_has_notifications(
        &mut client,
        TWO_LABEL_CARTESIAN,
        vec![severity("INFORMATION")]
    ));
    assert!(!ending_has_notifications(
        &mut client,
        TWO_LABEL_CARTESIAN,
        vec![(NOTIFICATIONS_MINIMUM_SEVERITY, Pack::Null)]
    ));
    assert!(!ending_has_notifications(
        &mut client,
        TWO_LABEL_CARTESIAN,
        vec![]
    ));

    // The override also closes: a per-statement OFF over an open HELLO.
    let mut client = open_client_with(vec![severity("INFORMATION")]);
    assert!(!ending_has_notifications(
        &mut client,
        TWO_LABEL_CARTESIAN,
        vec![severity("OFF")]
    ));
}

/// The BEGIN keys overlay the HELLO config for the transaction's
/// statements, and the overlay ends with the transaction: inside, the
/// BEGIN's `INFORMATION` reopens the list over the HELLO's `OFF`; after
/// COMMIT the HELLO's config governs again, and a keyless BEGIN inherits
/// it.
#[test]
fn begin_extra_keys_overlay_the_hello_config_for_the_transaction() {
    let mut client = open_client_with(vec![severity("OFF")]);
    send_begin(&mut client, vec![severity("INFORMATION")]);
    success_map(read_msg(&mut client));
    assert!(ending_has_notifications(
        &mut client,
        TWO_LABEL_CARTESIAN,
        vec![]
    ));
    send_commit(&mut client);
    success_map(read_msg(&mut client));
    // The transaction ended: the HELLO's OFF governs the next statement.
    assert!(!ending_has_notifications(
        &mut client,
        TWO_LABEL_CARTESIAN,
        vec![]
    ));
    // A keyless BEGIN inherits the HELLO config for its statements.
    send_begin(&mut client, vec![]);
    success_map(read_msg(&mut client));
    assert!(!ending_has_notifications(
        &mut client,
        TWO_LABEL_CARTESIAN,
        vec![]
    ));
    send_rollback(&mut client);
    success_map(read_msg(&mut client));
}

/// At 4.4 the keys are ignored as today (ADR-0484: a 4.4 connection is
/// byte-identical): a HELLO or BEGIN carrying an invalid severity still
/// succeeds, and a RUN carrying the keys filters nothing.
#[test]
fn the_notification_keys_are_ignored_at_4_4() {
    let mut client = handshaken(ConnConfig::default());
    send_struct(
        &mut client,
        Message::Hello.signature(),
        vec![hello_extra(vec![severity("INVALID")])],
    );
    assert_eq!(
        keys(&success_map(read_msg(&mut client))),
        ["server", "connection_id", "hints"]
    );

    let mut client = connected_client();
    send_begin(
        &mut client,
        vec![severity("INVALID"), categories(&["UNRECOGNIZED"])],
    );
    success_map(read_msg(&mut client));
    send_rollback(&mut client);
    success_map(read_msg(&mut client));
    assert!(ending_has_notifications(
        &mut client,
        UNKNOWN_LABEL,
        vec![severity("OFF"), categories(&["UNRECOGNIZED"])]
    ));
}
