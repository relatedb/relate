//! End-to-end tests for the ADR-0164 open-transaction exit contract: leaving
//! the CLI with an open `.begin` transaction discards the uncommitted writes,
//! so batch exits (one-shot positional query, piped stdin) print the warning
//! on stderr and exit 1, while a committed transaction exits 0 quietly. The
//! warning wording is an observable contract — wrapper scripts grep stderr
//! for `rolling back` — so these tests pin it byte-for-byte.
//!
//! Each test runs against a fresh tempdir `$HOME` with
//! `RELATE_TELEMETRY=off` so the one-time first-run telemetry notice can
//! never leak into the stderr assertions (it prints on a fresh, unclaimed
//! home when telemetry is enabled).

use crate::support;

use std::process::ExitStatus;

use support::{run_cli, CliRunOptions as Opts, OPEN_TRANSACTION_WARNING};

/// Run the CLI against a fresh tempdir `$HOME` with telemetry disabled via
/// the env layer (`src/telemetry/optout.rs`), so the first-run notice is
/// never printed regardless of the fresh `$HOME`.
fn run_isolated(args: &[&str], stdin: &[u8]) -> (ExitStatus, String, String) {
    let home = tempfile::tempdir().expect("tempdir");
    run_cli(
        Opts::new(args)
            .in_home(home.path())
            .with_envs(&[("RELATE_TELEMETRY", "off")])
            .with_stdin(stdin),
    )
}

#[test]
fn test_piped_stdin_open_transaction_warns_and_exits_1() {
    // Piped stdin that reaches EOF with an open `.begin` transaction: the
    // batch exit path must warn on stderr and force the exit code to 1.
    let (status, _stdout, stderr) = run_isolated(&["query"], b".begin\nCREATE (n);\n");
    assert_eq!(status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains(OPEN_TRANSACTION_WARNING),
        "expected the open-transaction warning in stderr, got: {stderr:?}"
    );
    // Byte-for-byte pin: the warning is one whole stderr line, not a
    // substring of a longer message.
    assert!(
        stderr.lines().any(|line| line == OPEN_TRANSACTION_WARNING),
        "expected the warning verbatim on its own stderr line, got: {stderr:?}"
    );
}

#[test]
fn test_one_shot_open_transaction_warns_and_exits_1() {
    // A positional one-shot run holding multiple statements
    // (`.begin; CREATE (n)`) ends with the transaction still open: same
    // warning, same exit 1. Empty stdin: the one-shot path never reads it.
    let (status, _stdout, stderr) = run_isolated(&[".begin; CREATE (n)"], b"");
    assert_eq!(status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains(OPEN_TRANSACTION_WARNING),
        "expected the open-transaction warning in stderr, got: {stderr:?}"
    );
    assert!(
        stderr.lines().any(|line| line == OPEN_TRANSACTION_WARNING),
        "expected the warning verbatim on its own stderr line, got: {stderr:?}"
    );
}

#[test]
fn test_piped_stdin_committed_transaction_exits_0_with_quiet_stderr() {
    // The committed counterpart: `.begin; CREATE (n); .commit` leaves no open
    // transaction at exit, so there is nothing to warn about — exit 0 and an
    // empty stderr.
    let (status, _stdout, stderr) = run_isolated(&["query"], b".begin\nCREATE (n);\n.commit\n");
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");
    assert!(
        stderr.is_empty(),
        "committed run must not warn on stderr, got: {stderr:?}"
    );
}
