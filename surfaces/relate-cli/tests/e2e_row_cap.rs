//! End-to-end tests for the agent-run row cap (ADR-0196): the default cap under
//! a detected harness, `--max-rows`, `RELATE_MAX_ROWS`, the stderr warning,
//! and the envelope's `truncated` flag.

#[allow(dead_code)]
mod support;

use support::{run_cli, run_row_cap, CliRunOptions as Opts};

#[test]
fn test_no_cap_outside_an_agent_harness() {
    let (status, stdout, stderr) = run_row_cap(&[], &[]);
    assert!(status.success(), "{stderr}");
    // ADR-0581: the pure-row default — 250 rows and nothing else.
    assert_eq!(stdout.lines().count(), 250);
    assert!(!stderr.contains("truncated"), "stderr: {stderr}");
}

#[test]
fn test_agent_harness_caps_at_100_and_warns_on_stderr() {
    let (status, stdout, stderr) = run_row_cap(&[], &[("CLAUDECODE", "1")]);
    assert_eq!(status.code(), Some(0), "truncation is not a failure");
    // ADR-0403: the machine mode carries the cut in-band — 100 row lines
    // plus the keyed terminal record naming both counts.
    let lines: Vec<&str> = stdout.lines().collect();
    // 100 row lines, then the keyed terminal record.
    assert_eq!(lines.len(), 101, "{stdout}");
    assert_eq!(
        lines[100], r#"{"rowCap":{"kept":100,"total":250}}"#,
        "{stdout}"
    );
    assert!(
        stderr.contains(
            "warning: result truncated to 100 rows (the statement returned 250); pass --max-rows 0 for all rows, or page with SKIP/LIMIT"
        ),
        "stderr: {stderr}"
    );
}

#[test]
fn test_every_detected_harness_variable_triggers_the_cap() {
    for name in support::AGENT_ENV_VARS {
        let (_, stdout, _) = run_row_cap(&[], &[(name, "1")]);
        assert_eq!(stdout.lines().count(), 101, "{name}"); // rows + the rowCap record
    }
}

#[test]
fn test_max_rows_zero_lifts_the_agent_cap() {
    let (status, stdout, stderr) = run_row_cap(&["--max-rows", "0"], &[("CLAUDECODE", "1")]);
    assert!(status.success(), "{stderr}");
    assert_eq!(stdout.lines().count(), 250); // pure rows
    assert!(!stderr.contains("truncated"), "stderr: {stderr}");
}

#[test]
fn test_explicit_max_rows_applies_outside_a_harness_too() {
    let (_, stdout, stderr) = run_row_cap(&["--max-rows", "7"], &[]);
    let lines: Vec<&str> = stdout.lines().collect();
    // rows + the rowCap record.
    assert_eq!(lines.len(), 8, "{stdout}");
    assert_eq!(lines[7], r#"{"rowCap":{"kept":7,"total":250}}"#, "{stdout}");
    assert!(stderr.contains("truncated to 7 rows"), "stderr: {stderr}");
}

#[test]
fn test_a_result_within_the_cap_is_not_truncated() {
    let (_, stdout, stderr) = run_row_cap(&["--max-rows", "250"], &[]);
    assert_eq!(stdout.lines().count(), 250); // pure rows
    assert!(!stderr.contains("truncated"), "stderr: {stderr}");
}

#[test]
fn test_env_var_sets_the_default_and_the_flag_overrides_it() {
    let (_, stdout, _) = run_row_cap(&[], &[("RELATE_MAX_ROWS", "3")]);
    assert_eq!(stdout.lines().count(), 4); // rows + the rowCap record
    let (_, stdout, _) = run_row_cap(&["--max-rows", "5"], &[("RELATE_MAX_ROWS", "3")]);
    assert_eq!(stdout.lines().count(), 6); // rows + the rowCap record
    let (_, stdout, _) = run_row_cap(&[], &[("RELATE_MAX_ROWS", "0"), ("CLAUDECODE", "1")]);
    assert_eq!(stdout.lines().count(), 250);
}

#[test]
fn test_bad_values_exit_2_before_running() {
    let (status, stdout, stderr) = run_row_cap(&["--max-rows", "lots"], &[]);
    assert_eq!(status.code(), Some(2));
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert!(stderr.contains("invalid --max-rows"), "stderr: {stderr}");

    let (status, stdout, stderr) = run_row_cap(&[], &[("RELATE_MAX_ROWS", "lots")]);
    assert_eq!(status.code(), Some(2));
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert!(
        stderr.contains("invalid RELATE_MAX_ROWS"),
        "stderr: {stderr}"
    );

    // A flag with nothing after it: the shared helper would append the
    // database and query, which the flag would take as its value.
    let (status, _, stderr) = run_cli(Opts::new(&["--max-rows"]));
    assert_eq!(status.code(), Some(2));
    assert!(stderr.contains("--max-rows requires"), "stderr: {stderr}");
}

#[test]
fn test_envelope_reports_truncated_true_only_when_it_cut() {
    for (cap, truncated, rows) in [("2", true, 2), ("9", false, 5)] {
        let (_, stdout, _) = run_cli(Opts::new(&[
            "--mode",
            "envelope",
            "--max-rows",
            cap,
            "UNWIND range(1, 5) AS i RETURN i",
        ]));
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        assert_eq!(v["truncated"], truncated, "cap {cap}");
        assert_eq!(v["rows"].as_array().unwrap().len(), rows, "cap {cap}");
        // ADR-0403: the counts in-band beside the legacy bool.
        if truncated {
            assert_eq!(v["rowCap"]["kept"], 2, "cap {cap}");
            assert_eq!(v["rowCap"]["total"], 5, "cap {cap}");
        } else {
            assert!(v.get("rowCap").is_none(), "uncut envelopes carry no rowCap");
        }
    }
}

#[test]
fn test_each_statement_is_capped_independently() {
    let (_, stdout, stderr) = run_cli(Opts::new(&[
        "--jsonl",
        "--timings",
        "--max-rows",
        "2",
        "UNWIND range(1, 5) AS i RETURN i; UNWIND range(1, 3) AS j RETURN j; RETURN 1 AS k",
    ]));
    // rows: 2 + 2 + 1, each capped statement's rowCap record, each
    // statement's LABS-1120 timings line (2 capped + 1 plain) — the
    // timings lines under the ADR-0581 opt-in.
    assert_eq!(stdout.lines().count(), 10, "stdout: {stdout}");
    assert_eq!(stdout.matches("rowCap").count(), 2, "stdout: {stdout}");
    assert_eq!(stdout.matches("\"timings\"").count(), 3, "stdout: {stdout}");
    assert_eq!(stderr.matches("warning: result truncated").count(), 2);
}
