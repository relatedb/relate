//! RUN-parameter conversion wire tests (ADR-0177): a parameter the engine
//! can represent decodes to the value its Cypher constructor produces, and
//! one it cannot fails the RUN with `Neo.ClientError.Request.Invalid`
//! (ADR-0307) and a path-qualified
//! message, sets the failed state (PULL is IGNORED until RESET), never
//! executes the query, and never writes anything. Bytes parameters round
//! trip and cannot be stored (ADR-0328).
use crate::failures::REQUEST_INVALID_CODE;
use crate::packstream::Pack;
use crate::requests::Message;
use crate::testutil::{
    assert_ignored, failure_map, field, read_msg, send_pull, send_reset, send_struct, success_map,
};
use crate::value_structs::{
    DATE, DATE_TIME, DATE_TIME_UTC, DATE_TIME_ZONE_ID, DURATION, LOCAL_DATE_TIME, LOCAL_TIME,
    POINT_2D, POINT_3D, TIME,
};
use crate::version::BoltVersion;
use std::net::TcpStream;

/// Open a connection with a fresh in-memory database, negotiate Bolt 4.4,
/// and complete HELLO.
/// Send RUN with `fields` and assert the FAILURE carries
/// `Neo.ClientError.Request.Invalid` and exactly `message`.
fn assert_rejected(client: &mut TcpStream, fields: Vec<Pack>, message: &str) {
    send_struct(client, Message::Run.signature(), fields);
    let metadata = failure_map(read_msg(client));
    assert_eq!(field(&metadata, "code"), REQUEST_INVALID_CODE);
    assert_eq!(field(&metadata, "message"), message);
}

/// Send RUN with `fields`, consume its SUCCESS, PULL `n: -1`, and return
/// the first reply frame — the RECORD when the query emitted one, otherwise
/// the PULL SUCCESS.
fn run_and_pull(client: &mut TcpStream, fields: Vec<Pack>) -> Pack {
    send_struct(client, Message::Run.signature(), fields);
    success_map(read_msg(client));
    send_pull(client, Message::Pull, vec![("n", Pack::Int(-1))]);
    read_msg(client)
}

/// Send RUN with `cypher` and `params`, PULL -1, and assert the single
/// RECORD is `[true]`; both SUCCESS maps are consumed.
fn assert_answers_true(client: &mut TcpStream, cypher: &str, params: Vec<(Pack, Pack)>) {
    let reply = run_and_pull(client, vec![Pack::String(cypher.into()), Pack::Map(params)]);
    assert_eq!(
        reply,
        Pack::Struct {
            tag: 0x71,
            fields: vec![Pack::List(vec![Pack::Bool(true)])],
        },
        "RUN {cypher} must answer one RECORD of [true]"
    );
    success_map(read_msg(client));
}

/// Send RUN with `cypher` and `params`, PULL -1, and assert both SUCCEED
/// without emitting a RECORD (e.g. a CREATE).
fn run_and_drain(client: &mut TcpStream, cypher: &str, params: Vec<(Pack, Pack)>) {
    success_map(run_and_pull(
        client,
        vec![Pack::String(cypher.into()), Pack::Map(params)],
    ));
}

/// Assert the connection is in the failed state — the next PULL answers
/// IGNORED — then RESET answers SUCCESS and a following `RETURN 1` RUN
/// executes to a RECORD of `1`.
fn assert_failed_state_then_recovers(client: &mut TcpStream) {
    send_pull(client, Message::Pull, vec![]);
    assert_ignored(client, "a PULL after a rejected RUN");
    send_reset(client);
    success_map(read_msg(client));
    assert_eq!(
        run_and_pull(
            client,
            vec![Pack::String("RETURN 1 AS n".into()), Pack::Map(vec![])],
        ),
        Pack::Struct {
            tag: 0x71,
            fields: vec![Pack::List(vec![Pack::Int(1)])],
        }
    );
    success_map(read_msg(client));
}

#[test]
fn absent_params_field_stays_an_empty_map() {
    let mut client = crate::testutil::connected_client();
    // The RUN carries no params field at all.
    assert_eq!(
        run_and_pull(&mut client, vec![Pack::String("RETURN 1 AS n".into())]),
        Pack::Struct {
            tag: 0x71,
            fields: vec![Pack::List(vec![Pack::Int(1)])],
        }
    );
    success_map(read_msg(&mut client));
}

/// ADR-0342 (security F3): a RUN whose statement field is absent or not a
/// string is rejected like an unrepresentable parameter — Request.Invalid,
/// failed state, nothing executes — instead of running the empty statement
/// and reporting a misleading engine syntax error.
#[test]
fn a_run_without_a_string_statement_is_rejected() {
    let mut client = crate::testutil::connected_client();
    for (fields, message) in [
        (
            vec![],
            "missing RUN statement (RUN statement must be a string)",
        ),
        (
            vec![Pack::Null],
            "unsupported request value for statement: Null (RUN statement must be a string)",
        ),
        (
            vec![Pack::Int(4)],
            "unsupported request value for statement: Int (RUN statement must be a string)",
        ),
        (
            vec![Pack::List(vec![])],
            "unsupported request value for statement: List (RUN statement must be a string)",
        ),
    ] {
        assert_rejected(&mut client, fields, message);
        assert_failed_state_then_recovers(&mut client);
    }
}

/// F-64 / D-25: a Bytes parameter round-trips as PackStream Bytes.
#[test]
fn bytes_parameter_round_trips() {
    let mut client = crate::testutil::connected_client();
    assert_eq!(
        run_and_pull(
            &mut client,
            vec![
                Pack::String("RETURN $p AS x".into()),
                Pack::Map(vec![(
                    Pack::String("p".into()),
                    Pack::Bytes(vec![0, 1, 255])
                )]),
            ],
        ),
        Pack::Struct {
            tag: 0x71,
            fields: vec![Pack::List(vec![Pack::Bytes(vec![0, 1, 255])])],
        }
    );
    success_map(read_msg(&mut client));
    // Byte-wise equality, and BINARY as the column type.
    assert_answers_true(
        &mut client,
        "RETURN $a = $b AND $a <> $c AS ok",
        vec![
            (Pack::String("a".into()), Pack::Bytes(vec![1, 2])),
            (Pack::String("b".into()), Pack::Bytes(vec![1, 2])),
            (Pack::String("c".into()), Pack::Bytes(vec![1, 3])),
        ],
    );
    send_struct(
        &mut client,
        Message::Run.signature(),
        vec![
            Pack::String("RETURN $p AS x".into()),
            Pack::Map(vec![(Pack::String("p".into()), Pack::Bytes(vec![9]))]),
        ],
    );
    let run = Pack::Map(success_map(read_msg(&mut client)));
    assert_eq!(
        run.get("columnTypes"),
        Some(&Pack::List(vec![Pack::String("BINARY".into())]))
    );
}

#[test]
fn bytes_nested_in_a_list_in_a_map_round_trip() {
    let mut client = crate::testutil::connected_client();
    let nested = Pack::Map(vec![(
        Pack::String("xs".into()),
        Pack::List(vec![Pack::Bytes(vec![3]), Pack::Bytes(vec![])]),
    )]);
    assert_eq!(
        run_and_pull(
            &mut client,
            vec![
                Pack::String("RETURN $p AS x".into()),
                Pack::Map(vec![(Pack::String("p".into()), nested.clone())]),
            ],
        ),
        Pack::Struct {
            tag: 0x71,
            fields: vec![Pack::List(vec![nested])],
        }
    );
    success_map(read_msg(&mut client));
}

/// ADR-0327: a byte array is statement-scoped; storing it as a property is
/// the typed TypeError and writes nothing. The failure may come on the RUN
/// or on its PULL, so both replies are read.
#[test]
fn a_bytes_property_fails_and_writes_nothing() {
    let mut client = crate::testutil::connected_client();
    send_struct(
        &mut client,
        Message::Run.signature(),
        vec![
            Pack::String("CREATE (:T {b: $b})".into()),
            Pack::Map(vec![(Pack::String("b".into()), Pack::Bytes(vec![7]))]),
        ],
    );
    send_pull(&mut client, Message::Pull, vec![]);
    let replies = [read_msg(&mut client), read_msg(&mut client)];
    let failure = replies
        .iter()
        .find(|reply| matches!(reply, Pack::Struct { tag: 0x7F, .. }))
        .cloned()
        .expect("storing a byte array must fail the statement");
    assert_eq!(
        field(&failure_map(failure), "message"),
        "type error: TypeError: byte array values cannot be stored as node or relationship \
         properties; they live only inside a statement (parameter to result)"
    );
    send_reset(&mut client);
    success_map(read_msg(&mut client));
    assert_eq!(
        run_and_pull(
            &mut client,
            vec![
                Pack::String("MATCH (n:T) RETURN count(n) AS c".into()),
                Pack::Map(vec![]),
            ],
        ),
        Pack::Struct {
            tag: 0x71,
            fields: vec![Pack::List(vec![Pack::Int(0)])],
        },
        "the failed CREATE must not have created a node"
    );
    success_map(read_msg(&mut client));
}

#[test]
fn struct_parameter_fails_the_run() {
    let mut client = crate::testutil::connected_client();
    assert_rejected(
        &mut client,
        vec![
            Pack::String("RETURN $p AS x".into()),
            Pack::Map(vec![(
                Pack::String("p".into()),
                Pack::Struct {
                    tag: 0x4E,
                    fields: vec![
                        Pack::Int(1),
                        Pack::List(vec![Pack::String("T".into())]),
                        Pack::Map(vec![]),
                    ],
                },
            )]),
        ],
        "unsupported parameter value for $p: Struct(0x4E)",
    );
    assert_failed_state_then_recovers(&mut client);
}

#[test]
fn nested_non_string_map_key_fails_the_run() {
    let mut client = crate::testutil::connected_client();
    assert_rejected(
        &mut client,
        vec![
            Pack::String("RETURN $p AS x".into()),
            Pack::Map(vec![(
                Pack::String("p".into()),
                Pack::Map(vec![(Pack::Int(1), Pack::Int(2))]),
            )]),
        ],
        "unsupported parameter map key for $p: Int (map keys must be strings)",
    );
    assert_failed_state_then_recovers(&mut client);
}

#[test]
fn top_level_non_string_map_key_fails_the_run() {
    let mut client = crate::testutil::connected_client();
    assert_rejected(
        &mut client,
        vec![
            Pack::String("RETURN 1 AS n".into()),
            Pack::Map(vec![(Pack::Int(1), Pack::Int(2))]),
        ],
        "unsupported parameter map key for $: Int (map keys must be strings)",
    );
    assert_failed_state_then_recovers(&mut client);
}

#[test]
fn non_map_params_field_fails_the_run() {
    let mut client = crate::testutil::connected_client();
    assert_rejected(
        &mut client,
        vec![Pack::String("RETURN 1 AS n".into()), Pack::Int(1)],
        "unsupported parameter value for $: Int (RUN params must be a map)",
    );
    assert_failed_state_then_recovers(&mut client);
}

/// A rejected RUN must not reach the engine: the CREATE writes nothing, so
/// after RESET the label is empty.
#[test]
fn a_rejected_run_writes_nothing() {
    let mut client = crate::testutil::connected_client();
    assert_rejected(
        &mut client,
        vec![
            Pack::String("CREATE (:T {b: $b})".into()),
            Pack::Map(vec![(
                Pack::String("b".into()),
                Pack::Struct {
                    tag: 0x4E,
                    fields: vec![],
                },
            )]),
        ],
        "unsupported parameter value for $b: Struct(0x4E)",
    );

    // The failed state is set — PULL is IGNORED until RESET clears it — and
    // RESET recovers the session.
    assert_failed_state_then_recovers(&mut client);

    // The rejected CREATE wrote nothing: `:T` has no nodes.
    assert_eq!(
        run_and_pull(
            &mut client,
            vec![
                Pack::String("MATCH (n:T) RETURN count(n) AS c".into()),
                Pack::Map(vec![]),
            ],
        ),
        Pack::Struct {
            tag: 0x71,
            fields: vec![Pack::List(vec![Pack::Int(0)])],
        },
        "the rejected RUN must not have created a node"
    );
    success_map(read_msg(&mut client));
}

/// Every worked example in ADR-0177's conversion table crosses the wire as
/// `$p` and equals the value its Cypher constructor produces.
#[test]
fn worked_example_structs_decode_as_run_parameters() {
    let cases: [(u8, Vec<Pack>, &str); 9] = [
        (DATE, vec![Pack::Int(0)], "date('1970-01-01')"),
        (
            TIME,
            vec![Pack::Int(45_000_000_000_000), Pack::Int(3600)],
            "time('12:30:00+01:00')",
        ),
        (
            LOCAL_TIME,
            vec![Pack::Int(45_000_000_000_000)],
            "localtime('12:30:00')",
        ),
        (
            DATE_TIME,
            vec![Pack::Int(8100), Pack::Int(42), Pack::Int(3600)],
            "datetime('1970-01-01T02:15:00.000000042+01:00')",
        ),
        (
            DATE_TIME_ZONE_ID,
            vec![
                Pack::Int(8100),
                Pack::Int(42),
                Pack::String("Europe/Paris".into()),
            ],
            "datetime('1970-01-01T02:15:00.000000042+01:00[Europe/Paris]')",
        ),
        (
            LOCAL_DATE_TIME,
            vec![Pack::Int(86_400), Pack::Int(42)],
            "localdatetime('1970-01-02T00:00:00.000000042')",
        ),
        (
            DURATION,
            vec![Pack::Int(1), Pack::Int(2), Pack::Int(3), Pack::Int(4)],
            "duration('P1M2DT3.000000004S')",
        ),
        (
            POINT_2D,
            vec![Pack::Int(7203), Pack::Float(1.0), Pack::Float(2.0)],
            "point({srid: 7203, x: 1.0, y: 2.0})",
        ),
        (
            POINT_3D,
            vec![
                Pack::Int(9157),
                Pack::Float(1.0),
                Pack::Float(2.0),
                Pack::Float(3.0),
            ],
            "point({srid: 9157, x: 1.0, y: 2.0, z: 3.0})",
        ),
    ];
    for (tag, fields, constructor) in cases {
        // A fresh connection per case keeps one failure from cascading.
        let mut client = crate::testutil::connected_client();
        let cypher = format!("RETURN $p = {constructor} AS ok");
        assert_answers_true(
            &mut client,
            &cypher,
            vec![(Pack::String("p".into()), Pack::Struct { tag, fields })],
        );
    }
}

/// The 0x66 worked example survives a CREATE and reads back equal through a
/// MATCH on the stored property.
#[test]
fn zoned_datetime_parameter_round_trips_through_a_node_property() {
    let mut client = crate::testutil::connected_client();
    run_and_drain(
        &mut client,
        "CREATE (:T {d: $p})",
        vec![(
            Pack::String("p".into()),
            Pack::Struct {
                tag: DATE_TIME_ZONE_ID,
                fields: vec![
                    Pack::Int(8100),
                    Pack::Int(42),
                    Pack::String("Europe/Paris".into()),
                ],
            },
        )],
    );
    assert_answers_true(
        &mut client,
        "MATCH (n:T) RETURN n.d = \
         datetime('1970-01-01T02:15:00.000000042+01:00[Europe/Paris]') AS ok",
        vec![],
    );
}

/// A struct nested in a list in a map decodes at any depth; its path is
/// `$m.xs[0]`.
#[test]
fn struct_nested_in_a_list_in_a_map_decodes() {
    let mut client = crate::testutil::connected_client();
    assert_answers_true(
        &mut client,
        "RETURN $m.xs[0] = date('1970-01-01') AS ok",
        vec![(
            Pack::String("m".into()),
            Pack::Map(vec![(
                Pack::String("xs".into()),
                Pack::List(vec![Pack::Struct {
                    tag: DATE,
                    fields: vec![Pack::Int(0)],
                }]),
            )]),
        )],
    );
}

/// The Bolt 5.0 UTC DateTime tag is never negotiated (ADR-0177 item 5), so
/// 0x49 fails the RUN like any other unsupported struct.
#[test]
fn utc_datetime_struct_fails_the_run() {
    let mut client = crate::testutil::connected_client();
    assert_rejected(
        &mut client,
        vec![
            Pack::String("RETURN $p AS x".into()),
            Pack::Map(vec![(
                Pack::String("p".into()),
                Pack::Struct {
                    tag: DATE_TIME_UTC,
                    fields: vec![Pack::Int(0), Pack::Int(0)],
                },
            )]),
        ],
        "unsupported parameter value for $p: Struct(0x49)",
    );
    assert_failed_state_then_recovers(&mut client);
}

/// ADR-0484 decision 7: at 5.4 the UTC DateTime pair is unconditional in
/// the parameter direction too — a 0x49 parameter decodes to the value
/// its Cypher constructor builds — while the legacy 0x46, removed after
/// 4.4, is rejected like any other unsupported struct.
#[test]
fn at_5_4_the_utc_datetime_pair_decodes_and_the_legacy_pair_is_rejected() {
    let mut client = crate::testutil::connected_client_at(BoltVersion::V5_4);
    assert_answers_true(
        &mut client,
        "RETURN $p = datetime('2024-01-02T03:04:05.123456789+01:00') AS ok",
        vec![(
            Pack::String("p".into()),
            Pack::Struct {
                tag: DATE_TIME_UTC,
                fields: vec![Pack::Int(1704161045), Pack::Int(123456789), Pack::Int(3600)],
            },
        )],
    );
    assert_rejected(
        &mut client,
        vec![
            Pack::String("RETURN $p AS x".into()),
            Pack::Map(vec![(
                Pack::String("p".into()),
                Pack::Struct {
                    tag: DATE_TIME,
                    fields: vec![Pack::Int(0), Pack::Int(0), Pack::Int(0)],
                },
            )]),
        ],
        "unsupported parameter value for $p: Struct(0x46)",
    );
    assert_failed_state_then_recovers(&mut client);
}
