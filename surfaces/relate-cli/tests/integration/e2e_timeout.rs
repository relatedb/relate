//! End-to-end tests for `--timeout` (ADR-0241, LABS-234): a statement that
//! overruns the bound is cancelled through the engine's ADR-0111 token and
//! exits 7 with the timeout report; a statement inside the bound runs.

use crate::support;

use support::{run_cli, CliRunOptions as Opts};

/// A query that would run for minutes without a bound (a ~10^11-pair
/// cartesian over ranges) but hits a cooperative cancel point constantly.
/// ADR-0463 is per-allocation, so each range/UNWIND buffer here is under the
/// cap: the original shape must reach the timeout unchanged — this is the
/// regression test for the cumulative meter's false refusal.
const RUNAWAY: &str = "UNWIND range(1, 400000) AS a UNWIND range(1, 400000) AS b RETURN count(*)";

#[test]
fn test_timeout_cancels_a_runaway_statement_with_exit_7() {
    let (status, _stdout, stderr) = run_cli(Opts::new(&["--timeout", "100ms", RUNAWAY]));
    assert_eq!(
        status.code(),
        Some(7),
        "a timed-out statement exits 7, got {status:?}; stderr: {stderr}"
    );
    assert!(
        stderr.contains("query timed out after 100ms"),
        "expected the timeout line on stderr, got: {stderr}"
    );
}

#[test]
fn test_timeout_json_mode_emits_the_error_envelope() {
    let (status, stdout, stderr) = run_cli(Opts::new(&["--timeout", "100ms", "--json", RUNAWAY]));
    assert_eq!(status.code(), Some(7), "stderr: {stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["error"]["code"], "timeout");
    assert_eq!(v["error"]["exit_code"], 7);
    assert_eq!(v["error"]["kind"], "execution");
    assert_eq!(v["error"]["retryable"], false);
}

#[test]
fn test_a_statement_inside_the_bound_runs() {
    let (status, stdout, stderr) =
        run_cli(Opts::new(&["--timeout", "60s", "RETURN 42 AS answer;"]));
    assert!(
        status.success(),
        "fast query under --timeout failed: {stderr}"
    );
    assert!(stdout.contains("42"), "stdout: {stdout}");
}

#[test]
fn test_timeout_zero_disables_the_bound() {
    let (status, stdout, stderr) = run_cli(Opts::new(&["--timeout", "0", "RETURN 1 AS n;"]));
    assert!(status.success(), "--timeout 0 failed: {stderr}");
    assert!(stdout.contains("1"), "stdout: {stdout}");
}

#[test]
fn test_an_invalid_timeout_is_a_usage_error() {
    let (status, _stdout, stderr) = run_cli(Opts::new(&["--timeout", "later", "RETURN 1"]));
    assert_eq!(status.code(), Some(2));
    assert!(
        stderr.contains("invalid --timeout"),
        "expected an invalid-value error, got: {stderr}"
    );
}
