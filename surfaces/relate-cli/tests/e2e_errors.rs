//! End-to-end tests for the query-run error contract (ADR-0194): the exit-code
//! map, the JSON error envelope in `json`/`jsonl` modes, and the unchanged
//! stderr line.

#[allow(dead_code)]
mod support;

use std::io::Write;
use std::process::Stdio;
use std::time::{Duration, Instant};

use support::{run_cli, CliRunOptions as Opts};

fn envelope(stdout: &str) -> serde_json::Value {
    let last = stdout.lines().last().unwrap_or_else(|| panic!("no stdout"));
    serde_json::from_str(last).unwrap_or_else(|e| panic!("not JSON ({e}): {last}"))
}

#[test]
fn test_syntax_error_exits_6_with_a_validation_envelope() {
    for mode in ["--json", "--jsonl"] {
        let (status, stdout, stderr) = run_cli(Opts::new(&[mode, "RETRN 1"]));
        assert_eq!(status.code(), Some(6), "{mode}: {stderr}");
        assert!(
            stderr.starts_with("syntax error: "),
            "{mode} stderr: {stderr}"
        );
        let v = envelope(&stdout);
        assert_eq!(v["error"]["code"], "validation_error", "{mode}");
        assert_eq!(v["error"]["exit_code"], 6, "{mode}");
        assert_eq!(v["error"]["kind"], "syntax", "{mode}");
        assert_eq!(v["error"]["retryable"], false, "{mode}");
        assert!(
            v["error"]["message"]
                .as_str()
                .unwrap()
                .starts_with("syntax error: "),
            "{mode}: {v}"
        );
    }
}

#[test]
fn test_read_only_write_is_a_rejected_statement() {
    let (status, stdout, _) = run_cli(Opts::new(&["--json", "--read-only", "CREATE (:A)"]));
    assert_eq!(status.code(), Some(6));
    let v = envelope(&stdout);
    assert_eq!(v["error"]["kind"], "semantic");
    assert_eq!(
        v["error"]["message"],
        "semantic error: WriteOnReadOnlyAccessDatabase: No write operations are allowed on this database. The database is in read-only mode on this Neo4j instance."
    );
}

#[test]
fn test_non_json_modes_print_no_envelope_but_keep_the_exit_code() {
    for args in [
        &["--csv", "RETRN 1"][..],
        &["--markdown", "RETRN 1"],
        &["--mode", "table", "RETRN 1"],
        &["--mode", "cypher", "RETRN 1"],
    ] {
        let (status, stdout, stderr) = run_cli(Opts::new(args));
        assert_eq!(status.code(), Some(6), "{args:?}: {stderr}");
        assert!(stdout.is_empty(), "{args:?} stdout: {stdout}");
        assert!(stderr.starts_with("syntax error: "), "{args:?}: {stderr}");
    }
}

#[test]
fn test_rows_before_a_failing_statement_stay_valid_json_lines() {
    let (status, stdout, _) = run_cli(Opts::new(&["--jsonl", "RETURN 1 AS a; RETRN 2"]));
    assert_eq!(status.code(), Some(6));
    // LABS-1120: the successful statement's stream closes with its
    // timing line before the failing statement's error envelope.
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 3, "stdout: {stdout}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(lines[0]).unwrap()["a"],
        1
    );
    assert!(
        lines[1].contains("\"timings\""),
        "the timing line closes the successful statement's stream: {stdout}"
    );
    assert_eq!(envelope(&stdout)["error"]["exit_code"], 6);
}

#[test]
fn test_success_and_empty_result_still_exit_0_without_an_envelope() {
    let (status, stdout, stderr) = run_cli(Opts::new(&["--jsonl", "MATCH (n) RETURN n"]));
    assert_eq!(status.code(), Some(0), "{stderr}");
    // LABS-1120: no rows, but the statement's timing line still closes the
    // stream — and no error envelope: success stays success.
    assert!(
        !stdout.contains("\"error\""),
        "a successful empty result prints no error envelope: {stdout}"
    );
    assert!(
        stdout.lines().count() == 1 && stdout.contains("\"timings\""),
        "the timing line is the stream's only line: {stdout}"
    );
}

#[test]
fn test_message_is_sanitized_in_both_streams() {
    // A crafted ESC sequence reaches the engine error text through the
    // statement; neither the stderr line nor the envelope may carry it raw.
    let (status, stdout, stderr) = run_cli(Opts::new(&["--jsonl", "RETURN `a\x1b[31mb`.x"]));
    assert_ne!(status.code(), Some(0));
    assert!(!stderr.contains('\x1b'), "stderr: {stderr:?}");
    assert!(!stdout.contains('\x1b'), "stdout: {stdout:?}");
}

#[test]
fn test_locked_store_exits_5_with_a_conflict_envelope() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("held");
    let db_path = db.to_str().unwrap();

    // Hold the store open: an interactive-style session that blocks on stdin.
    let mut holder = support::cli_command()
        .args(["query", "--database", db_path])
        .env("RELATE_TELEMETRY", "off")
        .env("DO_NOT_TRACK", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn holder");
    let mut stdin = holder.stdin.take().expect("holder stdin");
    stdin.write_all(b"RETURN 1;\n").unwrap();
    stdin.flush().unwrap();

    // Wait until the holder has finished creating the store layout (lock file,
    // writer marker and WAL directory), then let it settle: a second opener
    // that races the creation sees a half-built layout, not the lock.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !(db.join(".cypherlite.lock").exists()
        && db.join(".cypherlite.writer").exists()
        && db.join("wal").is_dir())
    {
        assert!(Instant::now() < deadline, "holder never opened the store");
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(500));

    let (status, stdout, stderr) =
        run_cli(Opts::new(&["--json", "--database", db_path, "RETURN 2"]));
    drop(stdin);
    let _ = holder.wait();

    assert_eq!(status.code(), Some(5), "stderr: {stderr}");
    assert!(
        stderr.starts_with("Failed to open Relate: storage locked"),
        "stderr: {stderr}"
    );
    let v = envelope(&stdout);
    assert_eq!(v["error"]["code"], "conflict");
    assert_eq!(v["error"]["kind"], "locked");
    assert_eq!(v["error"]["exit_code"], 5);
    assert_eq!(v["error"]["retryable"], false);
}

#[test]
fn test_usage_errors_and_unknown_dot_commands_keep_their_codes() {
    let (status, stdout, _) = run_cli(Opts::new(&["--json", "--bogus"]));
    assert_eq!(status.code(), Some(2));
    assert!(
        stdout.is_empty(),
        "usage errors print no envelope: {stdout}"
    );

    let (status, stdout, _) = run_cli(Opts::new(&["query", "--json"]).with_stdin(b".nope\n"));
    assert_eq!(status.code(), Some(1));
    assert!(stdout.is_empty(), "stdout: {stdout}");
}
