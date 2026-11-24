//! `with_keep_alive` (ADR-0312): the NOOP writer each engine tick drives,
//! work that finishes first, a failed write surfaced once the work has
//! finished, and the zero-interval rejection (ADR-0334).

use std::io::Write;
use std::time::Duration;

use relate::Database;

use super::*;

/// The keep-alive writer: each engine tick writes one NOOP chunk, work
/// that finishes first never ticks, and a failed NOOP write is returned
/// once the work has finished.
#[test]
fn keep_alive_ticks_write_noops_and_report_a_failed_write() {
    // Work that finishes inside the interval never ticks and never writes.
    let mut out: Vec<u8> = Vec::new();
    let value = with_keep_alive(&mut out, Duration::from_secs(60), |_heartbeat| 7).unwrap();
    assert_eq!((value, out), (7, Vec::new()));

    // A statement that outlives the interval ticks through the engine
    // (ADR-0312): each tick writes exactly one NOOP chunk.
    let db = Database::open(":memory:").unwrap();
    let mut conn = db.connect().unwrap();
    let stage = "UNWIND range(1, 100000) AS x WITH count(x) AS c ";
    let long_query = format!("{}RETURN c", stage.repeat(120));
    let mut out: Vec<u8> = Vec::new();
    let staged = with_keep_alive(&mut out, Duration::from_millis(5), |heartbeat| {
        conn.query_staged(
            &long_query,
            std::collections::BTreeMap::new(),
            relate::CancelToken::new(),
            Some(heartbeat),
        )
        .unwrap()
    })
    .unwrap();
    assert!(matches!(staged, relate::Executed::Rows(_)));
    assert!(
        !out.is_empty() && out.iter().all(|&b| b == 0) && out.len().is_multiple_of(2),
        "each tick writes one NOOP chunk: {out:?}"
    );

    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut finished = false;
    let err = with_keep_alive(&mut Broken, Duration::from_millis(5), |heartbeat| {
        conn.query_staged(
            &long_query,
            std::collections::BTreeMap::new(),
            relate::CancelToken::new(),
            Some(heartbeat),
        )
        .unwrap();
        finished = true;
    })
    .unwrap_err();
    assert!(finished, "the work still completes");
    assert_eq!(err.kind(), std::io::ErrorKind::BrokenPipe);
}

/// A zero keep-alive interval is rejected before any work runs: it would
/// tick without ever waiting (ADR-0334).
#[test]
fn a_zero_keep_alive_interval_fails_before_the_work() {
    let mut out: Vec<u8> = Vec::new();
    let mut ran = false;
    let err = with_keep_alive(&mut out, Duration::ZERO, |_heartbeat| {
        ran = true;
    })
    .unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(!ran, "the work must not run with an invalid interval");
    assert!(out.is_empty());
}
