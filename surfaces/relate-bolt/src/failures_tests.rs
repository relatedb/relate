//! ADR-0307 / ADR-0308 pins: every engine variant's Neo4j status code, the
//! ADR-0260 genericization under its new code, and the wire FAILUREs for a
//! syntax error, an unknown `db`, an unconvertible parameter, a missing
//! parameter and the result byte cap.
use std::net::TcpStream;
use std::sync::Arc;

use super::*;
// The engine-classification code constants (ADR-0502 §2 moved them to the
// engine's one classification; the assertions below keep pinning them so a
// policy change cannot hide behind a mirror).
use crate::protocol::{ConnConfig, ConnLimits};
use crate::testutil::{
    assert_ignored, connected_client_with, engine_error, failure_map, field, map, read_msg,
    send_struct, success_map,
};
use relate_error::{
    error_info, ErrorStage, ARITHMETIC_ERROR_CODE, CONSTRAINT_VALIDATION_FAILED_CODE,
    DATABASE_NOT_FOUND_CODE, INTERNAL_MESSAGE, OUTDATED_CODE, PARAMETER_MISSING_CODE,
    PROCEDURE_NOT_FOUND_CODE, SEMANTIC_ERROR_CODE, SYNTAX_ERROR_CODE,
    TRANSACTION_OUT_OF_MEMORY_CODE, TRANSACTION_TIMED_OUT_CODE, TYPE_ERROR_CODE,
    UNKNOWN_ERROR_CODE,
};

/// The wire FAILURE metadata `send_engine_failure` writes for `e`: the
/// class→code and class→message policy is asserted through its one home,
/// never re-derived here.
fn wire_failure(e: &relate::Error) -> Pack {
    let mut buf: Vec<u8> = Vec::new();
    send_engine_failure(&mut buf, e).unwrap();
    failure_map(read_msg(&mut &buf[..]))
}

/// The ADR-0307 table, one row per engine variant, with the literal code
/// and message strings so a policy change cannot hide behind a mirror.
#[test]
fn every_engine_variant_maps_to_its_neo4j_status_code() {
    use relate::{ArithmeticFailure, Error as E};
    let rows: Vec<(E, &str, &str)> = vec![
        (
            E::syntax("parse error"),
            "Neo.ClientError.Statement.SyntaxError",
            "syntax error: parse error",
        ),
        (
            E::procedure_not_found("dbms.noSuchProc"),
            "Neo.ClientError.Procedure.ProcedureNotFound",
            "syntax error: ProcedureNotFound: dbms.noSuchProc",
        ),
        (
            E::parameters_missing(["p"]),
            "Neo.ClientError.Statement.ParameterMissing",
            "semantic error: ParameterMissing: Expected parameter(s): p",
        ),
        (
            E::ty("expected number"),
            "Neo.ClientError.Statement.TypeError",
            "type error: expected number",
        ),
        (
            E::type_error("x"),
            "Neo.ClientError.Statement.TypeError",
            "type error: TypeError: x",
        ),
        (
            E::invalid_argument_type("x"),
            "Neo.ClientError.Statement.TypeError",
            "type error: InvalidArgumentType: x",
        ),
        (
            E::arithmetic(ArithmeticFailure::DivisionByZero),
            "Neo.ClientError.Statement.ArithmeticError",
            "type error: / by zero",
        ),
        (
            E::arithmetic(ArithmeticFailure::ModuloByZero),
            "Neo.ClientError.Statement.ArithmeticError",
            "type error: % by zero",
        ),
        (
            E::arithmetic(ArithmeticFailure::RoundingNecessary),
            "Neo.ClientError.Statement.ArithmeticError",
            "ArithmeticError: rounding necessary",
        ),
        (
            E::database_not_found("nosuchdb"),
            "Neo.ClientError.Database.DatabaseNotFound",
            "semantic error: DatabaseNotFound: Graph not found: nosuchdb",
        ),
        (
            E::uniqueness_violation("p_id"),
            "Neo.ClientError.Schema.ConstraintValidationFailed",
            "semantic error: ConstraintValidationFailed: `p_id` uniqueness",
        ),
        (
            E::cancelled(),
            "Neo.ClientError.Transaction.TransactionTimedOut",
            "query cancelled",
        ),
        (
            E::conflict(),
            "Neo.TransientError.Transaction.Outdated",
            "write-write conflict",
        ),
        (
            E::capacity(7, 5),
            "Neo.ClientError.General.TransactionOutOfMemoryError",
            "capacity exceeded: used=7 limit=5",
        ),
        (
            // ADR-0494: the per-record WAL payload cap refusal joins the
            // memory code — the engine message passes verbatim, never
            // genericized as the internal class it used to share.
            E::capacity_wal_payload(67_108_864, E::WAL_PAYLOAD_CAP_BYTES),
            "Neo.ClientError.General.TransactionOutOfMemoryError",
            "This statement tried to commit a single WAL record of at least \
             64 MiB. This is a safety limit protecting against memory \
             exhaustion — the per-commit WAL payload cap is 64 MiB (ADR-0086); \
             commit the data in smaller transactions (CALL { … } IN \
             TRANSACTIONS OF n ROWS, or client-side batches).",
        ),
        (
            E::semantic("variable `n` not defined"),
            "Neo.ClientError.Statement.SemanticError",
            "semantic error: variable `n` not defined",
        ),
        (
            E::execution("node ID exhausted"),
            "Neo.ClientError.Statement.SemanticError",
            "node ID exhausted",
        ),
        (
            E::internal("boom"),
            "Neo.DatabaseError.General.UnknownError",
            "internal error",
        ),
        (
            E::storage("/x"),
            "Neo.DatabaseError.General.UnknownError",
            "internal error",
        ),
        (
            E::io("os error 2"),
            "Neo.DatabaseError.General.UnknownError",
            "internal error",
        ),
        (
            E::Locked("/x".into()),
            "Neo.DatabaseError.General.UnknownError",
            "internal error",
        ),
    ];
    for (e, code, message) in rows {
        let fields = wire_failure(&e);
        assert_eq!(field(&fields, "code"), code, "{e:?}");
        assert_eq!(field(&fields, "message"), message, "{e:?}");
    }
    // No `Relate.*` code is left (ADR-0307).
    for code in [
        PROCEDURE_NOT_FOUND_CODE,
        UNAUTHORIZED_CODE,
        REQUEST_INVALID_CODE,
        SYNTAX_ERROR_CODE,
        SEMANTIC_ERROR_CODE,
        TYPE_ERROR_CODE,
        ARITHMETIC_ERROR_CODE,
        PARAMETER_MISSING_CODE,
        DATABASE_NOT_FOUND_CODE,
        CONSTRAINT_VALIDATION_FAILED_CODE,
        TRANSACTION_TIMED_OUT_CODE,
        OUTDATED_CODE,
        TRANSACTION_OUT_OF_MEMORY_CODE,
        UNKNOWN_ERROR_CODE,
    ] {
        assert!(code.starts_with("Neo."), "{code}");
    }
    assert_eq!(REQUEST_INVALID_CODE, "Neo.ClientError.Request.Invalid");
}

/// ADR-0260 survives ADR-0307: the internal-class variants still reach the
/// client only as the fixed `internal error` message; only the code moved.
#[test]
fn internal_class_errors_keep_the_fixed_message_under_unknown_error() {
    for e in [
        relate::Error::internal("query worker thread panicked: secret-payload"),
        relate::Error::storage("/host/secret/path"),
        relate::Error::io("No such file or directory (os error 2)"),
        relate::Error::Locked("/host/secret/path".into()),
    ] {
        assert!(
            error_info(&e).stage == ErrorStage::Internal,
            "{e:?} must classify internal"
        );
        let fields = wire_failure(&e);
        assert_eq!(
            field(&fields, "code"),
            "Neo.DatabaseError.General.UnknownError"
        );
        assert_eq!(field(&fields, "message"), "internal error");
    }
}

/// A connected client (HELLO done) on a fresh in-memory database.
fn client_with(config: ConnConfig) -> TcpStream {
    connected_client_with(
        Arc::new(relate::Database::open(":memory:").unwrap()),
        config,
    )
}

/// The FAILURE metadata map: `code` then `message`, nothing else.
#[test]
fn failure_metadata_is_the_code_then_message_pair() {
    assert_eq!(
        failure_metadata(REQUEST_INVALID_CODE, "boom"),
        vec![
            (
                Pack::String("code".into()),
                Pack::String("Neo.ClientError.Request.Invalid".into()),
            ),
            (Pack::String("message".into()), Pack::String("boom".into())),
        ]
    );
    assert_eq!(
        failure_metadata(UNKNOWN_ERROR_CODE, INTERNAL_MESSAGE),
        vec![
            (
                Pack::String("code".into()),
                Pack::String("Neo.DatabaseError.General.UnknownError".into()),
            ),
            (
                Pack::String("message".into()),
                Pack::String(INTERNAL_MESSAGE.into()),
            ),
        ]
    );
}

/// Two engine messages ADR-0307 passes through verbatim, pinned by their
/// text; the table above pins the same property for every row over the
/// wire.
#[test]
fn conflict_and_capacity_messages_pass_through_verbatim() {
    for (e, message) in [
        (relate::Error::conflict(), "write-write conflict"),
        (
            relate::Error::capacity(7, 5),
            "capacity exceeded: used=7 limit=5",
        ),
    ] {
        let fields = wire_failure(&e);
        assert_eq!(field(&fields, "message"), message, "{e:?}");
    }
}

/// ADR-0138: ProcedureNotFound maps to Neo4j's procedure code, and only the
/// typed variant does — a syntax error whose text merely resembles the
/// pinned shape keeps the syntax-error code (ADR-0307).
#[test]
fn procedure_not_found_maps_to_the_neo4j_code() {
    assert_eq!(
        PROCEDURE_NOT_FOUND_CODE,
        "Neo.ClientError.Procedure.ProcedureNotFound"
    );
    assert_eq!(
        field(
            &wire_failure(&relate::Error::procedure_not_found("dbms.noSuchProc")),
            "code"
        ),
        PROCEDURE_NOT_FOUND_CODE
    );

    // Only the typed variant does: a syntax error whose text merely
    // resembles the pinned shape keeps the syntax-error code (ADR-0307).
    for msg in ["unexpected end of input", "ProcedureNotFound: dbms.x"] {
        assert_eq!(
            field(&wire_failure(&relate::Error::syntax(msg)), "code"),
            SYNTAX_ERROR_CODE
        );
    }
    // Internal-class errors still genericize (ADR-0260).
    assert_eq!(
        field(&wire_failure(&relate::Error::internal("boom")), "code"),
        UNKNOWN_ERROR_CODE
    );
}

/// The shared wire-failure shape: the engine's own error text for `cypher`
/// (via [`engine_error`]), then `cypher` over the wire through the file's
/// canonical [`run`] — the FAILURE carries `code` and exactly that message.
fn assert_wire_failure(cypher: &str, code: &str) {
    let expected = engine_error(cypher);
    let mut client = client_with(ConnConfig::default());
    run(&mut client, cypher, map(vec![]), map(vec![]));
    let fields = failure_map(read_msg(&mut client));
    assert_eq!(field(&fields, "code"), code);
    assert_eq!(field(&fields, "message"), expected);
}

#[test]
fn engine_syntax_error_failure_is_verbatim() {
    assert_wire_failure(
        "THIS IS NOT CYPHER",
        "Neo.ClientError.Statement.SyntaxError",
    );
}

#[test]
fn procedure_not_found_failure_carries_the_neo4j_code_on_the_wire() {
    let expected = engine_error("CALL dbms.noSuchProc()");
    assert!(
        expected.contains("ProcedureNotFound"),
        "the engine must raise procedure-not-found: {expected}"
    );
    assert_wire_failure(
        "CALL dbms.noSuchProc()",
        "Neo.ClientError.Procedure.ProcedureNotFound",
    );
}

fn run(client: &mut TcpStream, cypher: &str, params: Pack, extra: Pack) {
    send_struct(
        client,
        0x10,
        vec![Pack::String(cypher.into()), params, extra],
    );
}

fn pull_all(client: &mut TcpStream) {
    send_struct(
        client,
        0x3F,
        vec![Pack::Map(vec![(Pack::String("n".into()), Pack::Int(-1))])],
    );
}

/// RUN fails with `code` and `message`; PULL is IGNORED; RESET recovers.
fn assert_run_fails(client: &mut TcpStream, code: &str, message: &str) {
    let fields = failure_map(read_msg(client));
    assert_eq!(field(&fields, "code"), code);
    assert_eq!(field(&fields, "message"), message);
    pull_all(client);
    assert_ignored(client, "a PULL after a failed RUN");
    send_struct(client, 0x0F, vec![]);
    success_map(read_msg(client));
}

/// The `neo4j-status-codes.bolt` behaviors over `handle_conn`: compile-time
/// failures and a server-side parameter rejection, each with its code.
#[test]
fn wire_failures_carry_neo4j_status_codes() {
    let mut client = client_with(ConnConfig::default());
    run(&mut client, "RETURN 1 +", map(vec![]), map(vec![]));
    let fields = failure_map(read_msg(&mut client));
    assert_eq!(
        field(&fields, "code"),
        "Neo.ClientError.Statement.SyntaxError"
    );
    send_struct(&mut client, 0x0F, vec![]);
    success_map(read_msg(&mut client));

    run(
        &mut client,
        "RETURN 1 AS n",
        map(vec![]),
        map(vec![("db", Pack::String("nosuchdb".into()))]),
    );
    assert_run_fails(
        &mut client,
        "Neo.ClientError.Database.DatabaseNotFound",
        "semantic error: DatabaseNotFound: Graph not found: nosuchdb",
    );

    run(
        &mut client,
        "RETURN $p AS p",
        map(vec![(
            "p",
            Pack::Struct {
                tag: 0x4E,
                fields: vec![Pack::Int(1), Pack::List(vec![]), Pack::Map(vec![])],
            },
        )]),
        map(vec![]),
    );
    assert_run_fails(
        &mut client,
        "Neo.ClientError.Request.Invalid",
        "unsupported parameter value for $p: Struct(0x4E)",
    );

    // A `USE` of an unknown graph is the same typed DatabaseNotFound.
    run(
        &mut client,
        "USE nosuchdb RETURN 1 AS n",
        map(vec![]),
        map(vec![]),
    );
    assert_run_fails(
        &mut client,
        "Neo.ClientError.Database.DatabaseNotFound",
        "semantic error: DatabaseNotFound: Graph not found: nosuchdb",
    );

    // Division by zero is the typed arithmetic failure; it is raised while
    // executing, so it arrives at the first PULL (ADR-0319).
    run(&mut client, "RETURN 1 / 0 AS v", map(vec![]), map(vec![]));
    success_map(read_msg(&mut client));
    pull_all(&mut client);
    let fields = failure_map(read_msg(&mut client));
    assert_eq!(
        field(&fields, "code"),
        "Neo.ClientError.Statement.ArithmeticError"
    );
    assert_eq!(field(&fields, "message"), "type error: / by zero");
}

/// ADR-0308 over the wire (`parameter-missing.bolt`): nothing executes, no
/// RECORD with null, PULL is IGNORED, RESET recovers, and the same RUN with
/// the parameter supplied succeeds.
#[test]
fn missing_parameter_fails_the_run_with_parameter_missing() {
    let mut client = client_with(ConnConfig::default());
    run(&mut client, "RETURN $p AS p", map(vec![]), map(vec![]));
    assert_run_fails(
        &mut client,
        "Neo.ClientError.Statement.ParameterMissing",
        "semantic error: ParameterMissing: Expected parameter(s): p",
    );
    run(
        &mut client,
        "RETURN $p AS p",
        map(vec![("p", Pack::Int(1))]),
        map(vec![]),
    );
    success_map(read_msg(&mut client));
    pull_all(&mut client);
    assert_eq!(
        read_msg(&mut client),
        Pack::Struct {
            tag: 0x71,
            fields: vec![Pack::List(vec![Pack::Int(1)])],
        }
    );
    success_map(read_msg(&mut client));
}

/// The `--max-result-bytes` cap is the non-retryable memory code.
#[test]
fn result_cap_failure_is_transaction_out_of_memory() {
    let mut client = client_with(ConnConfig {
        limits: ConnLimits {
            max_result_bytes: 20,
            ..ConnLimits::default()
        },
        ..Default::default()
    });
    run(
        &mut client,
        "RETURN 'abcdefghijklmnopqrstuvwxyz' AS s",
        map(vec![]),
        map(vec![]),
    );
    assert_run_fails(
        &mut client,
        "Neo.ClientError.General.TransactionOutOfMemoryError",
        "query result exceeds max-result-bytes limit of 20 bytes",
    );
}

#[test]
fn workspace_refusal_is_only_the_logical_workspace_limit() {
    use crate::protocol::QueryOutcome;
    let retryable = relate::Error::resource_limit(
        relate::Error::CONCURRENT_LOGICAL_WORKSPACE_LIMIT,
        4096,
        true,
    );
    let blocked = relate::Error::resource_limit(
        relate::Error::CONCURRENT_LOGICAL_WORKSPACE_LIMIT,
        4096,
        false,
    );
    let draining = relate::Error::resource_limit(relate::Error::WRITER_DRAINING, 0, true);
    assert_eq!(
        workspace_refusal(&retryable),
        Some(QueryOutcome::RetryableRefusal)
    );
    assert_eq!(
        workspace_refusal(&blocked),
        Some(QueryOutcome::NonRetryableRefusal)
    );
    assert_eq!(workspace_refusal(&draining), None);
    assert_eq!(
        workspace_refusal(&relate::Error::Capacity {
            used: 1,
            limit: 1,
            kind: relate::CapacityKind::Graph,
        }),
        None
    );
    assert_eq!(workspace_refusal(&relate::Error::syntax("nope")), None);
}
