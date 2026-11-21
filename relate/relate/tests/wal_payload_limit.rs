//! ADR-0650: the per-commit WAL payload cap is an open-time knob. The
//! refusal a lowered cap produces names the *configured* value (never a
//! re-spelled constant) and the smaller-transaction remedy, and nothing
//! lands — the record is refused before any frame is written.

mod common;

use relate::{Database, OpenOptions, Value};

#[test]
fn a_lowered_cap_refuses_the_oversized_commit_naming_the_configured_value() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open_with(
        dir.path().to_str().unwrap(),
        OpenOptions {
            wal_payload_limit: Some(1 << 20), // 1 MiB
            ..OpenOptions::default()
        },
    )
    .unwrap();
    let mut c = db.connect().unwrap();
    let big = "x".repeat(2 << 20);
    let err = c
        .query_with_params(
            "CREATE (:Big {p: $p})",
            common::params(&[("p", Value::String(big.into()))]),
        )
        .unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains("per-commit WAL payload cap is 1 MB"),
        "the refusal names the configured cap: {text}"
    );
    assert!(
        text.contains("IN TRANSACTIONS"),
        "the refusal keeps the smaller-transaction remedy: {text}"
    );
    // Nothing landed: the refusal precedes any frame.
    assert_eq!(
        common::rows(&mut c, "MATCH (n) RETURN count(n)"),
        vec![vec![Value::Int(0)]]
    );
}
