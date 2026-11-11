//! The typed error variants of `docs/api/rust/errors.md` (ADR-0307,
//! ADR-0308, ADR-0494) and the engine's one error classification of them
//! (ADR-0502 §2): the general showcase of the public `Error` vocabulary,
//! exercised through the public query API. CI's engine lane builds and
//! runs every example in this directory, so the asserts below gate the
//! documented behavior.

use std::collections::BTreeMap;

use relate::{error_info, ArithmeticFailure, CapacityKind, Database, Error, ErrorStage, Value};

fn main() {
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    let missing = conn.query("RETURN $p AS p").unwrap_err();
    assert_eq!(missing, Error::parameters_missing(["p"]));
    // ADR-0502 §2: the engine's one classification — the Neo4j status code,
    // the GQLSTATUS, the compile/runtime/internal stage (ADR-0319) and the
    // drivers' transient test — single-sourced in `error_info`, the one
    // every surface derives from.
    let info = error_info(&missing);
    assert_eq!(
        info.neo4j_code,
        "Neo.ClientError.Statement.ParameterMissing"
    );
    assert_eq!(info.gql_status, "42N02");
    assert_eq!(info.stage, ErrorStage::Compile);
    assert!(!info.retryable);
    // The drivers' transient test: a write-write conflict is the one
    // caller-retryable class beside a retryable processing cap.
    let conflict_info = error_info(&Error::conflict());
    assert_eq!(
        conflict_info.neo4j_code,
        "Neo.TransientError.Transaction.Outdated"
    );
    assert!(conflict_info.retryable);
    let params = BTreeMap::from([("p".to_string(), Value::Null)]);
    assert_eq!(
        conn.query_with_params("RETURN $p AS p", params)
            .unwrap()
            .rows,
        vec![vec![Value::Null]]
    );
    assert_eq!(
        conn.query("RETURN 1 / 0 AS v").unwrap_err(),
        Error::Arithmetic(ArithmeticFailure::DivisionByZero)
    );
    assert!(matches!(
        conn.query("USE nosuch RETURN 1"),
        Err(Error::DatabaseNotFound { .. })
    ));
    // ADR-0494: a single commit whose record crosses the 64 MiB WAL payload
    // cap is refused as the capacity family's WalPayload kind — the
    // memory-limit class — before any frame is written, with the message
    // naming the record and the cap.
    let big = BTreeMap::from([(
        "p".to_string(),
        Value::String("x".repeat(Error::WAL_PAYLOAD_CAP_BYTES as usize).into()),
    )]);
    let err = conn
        .query_with_params("CREATE (:Oversize {p: $p})", big)
        .unwrap_err();
    assert!(matches!(
        &err,
        Error::Capacity {
            kind: CapacityKind::WalPayload,
            ..
        }
    ));
    assert!(err
        .to_string()
        .contains("per-record WAL payload cap is 64 MB"));
    // ADR-0567: a shortest-path statement whose two endpoint maps each match
    // more than the listing limit is refused, never truncated. Reaching the
    // limit needs over a million nodes, so the example builds the refusal
    // with its public constructor and checks its kind and message.
    let capped = Error::capacity_seek_candidates(1_048_577, 1_048_576);
    assert!(matches!(
        &capped,
        Error::Capacity {
            kind: CapacityKind::SeekCandidates,
            ..
        }
    ));
    assert!(capped.to_string().contains("lists at most 1048576"));
}
