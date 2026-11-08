//! End-to-end tests for the statement-progress report on a failed
//! multi-statement run (ADR-0199): the stderr line, the envelope fields, and
//! that what the report claims is what the store actually holds.

#[allow(dead_code)]
mod support;

use support::{node_count, run_cli, tempdir, CliRunOptions as Opts};

fn envelope(stdout: &str) -> serde_json::Value {
    let last = stdout.lines().last().expect("stdout");
    serde_json::from_str(last).unwrap_or_else(|e| panic!("not JSON ({e}): {last}"))
}

#[test]
fn test_a_failed_multi_statement_run_reports_its_progress() {
    let (status, stdout, stderr) =
        run_cli(Opts::new(&["--jsonl", "-c", "RETURN 1; RETRN 2; RETURN 3"]));
    assert_eq!(status.code(), Some(6), "{stderr}");
    // The existing error text is first and unchanged (a syntax error that
    // echoed its source adds its source and a caret line, ADR-0290); the
    // progress line is last.
    assert!(
        stderr.lines().next().unwrap().starts_with("syntax error: "),
        "{stderr}"
    );
    assert_eq!(
        stderr.lines().last(),
        Some("statement 2 of 3 failed; 1 completed and committed; 1 not run"),
        "{stderr}"
    );
    let v = envelope(&stdout);
    assert_eq!(v["error"]["statement"], 2);
    assert_eq!(v["error"]["statements"], 3);
    assert_eq!(v["error"]["committed"], 1);
    assert_eq!(v["error"]["exit_code"], 6, "the ADR-0194 fields are intact");
}

#[test]
fn test_the_report_matches_what_the_store_holds() {
    // The point of the report: an agent trusts `committed` to know whether the
    // database changed. Check it against the store.
    let dir = tempdir();
    let (status, stdout, stderr) = run_cli(
        Opts::new(&[
            "--jsonl",
            "--database",
            "./db",
            "-c",
            "CREATE (:A); CREATE (:B); RETRN 3; CREATE (:C)",
        ])
        .in_dir(dir.path()),
    );
    assert_eq!(status.code(), Some(6), "{stderr}");
    let v = envelope(&stdout);
    assert_eq!(v["error"]["statement"], 3);
    assert_eq!(v["error"]["statements"], 4);
    let committed = v["error"]["committed"].as_u64().unwrap();
    assert_eq!(committed, 2);

    assert_eq!(
        node_count(&dir.path().join("db")),
        committed,
        "the store holds as many nodes as the run said it committed"
    );
}

#[test]
fn test_inside_a_transaction_nothing_is_reported_as_committed() {
    let dir = tempdir();
    let (status, stdout, stderr) = run_cli(
        Opts::new(&[
            "--jsonl",
            "--database",
            "./db",
            "-c",
            ".begin; CREATE (:A); RETRN 2; .commit",
        ])
        .in_dir(dir.path()),
    );
    assert_eq!(status.code(), Some(6), "{stderr}");
    assert!(
        stderr.contains(
            "statement 3 of 4 failed; 2 completed, not committed (open transaction); 1 not run"
        ),
        "{stderr}"
    );
    assert_eq!(envelope(&stdout)["error"]["committed"], 0);

    assert_eq!(
        node_count(&dir.path().join("db")),
        0,
        "the open transaction was rolled back, so nothing is durable"
    );
}

#[test]
fn test_a_dot_command_counts_toward_the_numbering_but_a_failed_one_does_not_report() {
    // `.timer on` is statement 1, so the failing Cypher statement is 3 of 3.
    let (_, _, stderr) = run_cli(Opts::new(&[
        "--jsonl",
        "-c",
        ".timer on; RETURN 1; RETRN 2",
    ]));
    assert!(
        stderr.contains("statement 3 of 3 failed; 2 completed and committed; 0 not run"),
        "{stderr}"
    );

    // A failed dot-command stops the run as before: its own error, exit 1, and
    // neither a progress line nor an error object.
    let (status, stdout, stderr) =
        run_cli(Opts::new(&["--jsonl", "-c", "RETURN 1; .commit; RETURN 3"]));
    assert_eq!(status.code(), Some(1), "{stderr}");
    // LABS-1120: the run statement's timing line, nothing after it.
    assert!(
        stdout.starts_with("{\"1\":1}\n") && stdout.matches('\n').count() == 2,
        "the third statement never ran: {stdout}"
    );
    assert!(stderr.contains("no transaction"), "{stderr}");
    assert!(!stderr.contains("statement "), "{stderr}");
}

#[test]
fn test_piped_stdin_reports_progress_too() {
    let (status, _, stderr) =
        run_cli(Opts::new(&["query", "--jsonl"]).with_stdin(b"RETURN 1;\nRETRN 2;\nRETURN 3;\n"));
    assert_eq!(status.code(), Some(6));
    assert!(
        stderr.contains("statement 2 of 3 failed; 1 completed and committed; 1 not run"),
        "{stderr}"
    );
}

#[test]
fn test_single_statements_and_successful_runs_are_unchanged() {
    // One failing statement: no progress line, no extra envelope fields.
    let (status, stdout, stderr) = run_cli(Opts::new(&["--jsonl", "-c", "RETRN 1"]));
    assert_eq!(status.code(), Some(6));
    assert!(!stderr.contains("statement "), "{stderr}");
    let e = &envelope(&stdout)["error"];
    for key in ["statement", "statements", "committed"] {
        assert!(e.get(key).is_none(), "{key} on a single statement");
    }

    // A trailing `;` does not make a second statement.
    let (_, _, stderr) = run_cli(Opts::new(&["--jsonl", "-c", "RETRN 1;"]));
    assert!(!stderr.contains("statement "), "{stderr}");

    // A multi-statement run that succeeds prints no report.
    let (status, stdout, stderr) = run_cli(Opts::new(&["--jsonl", "-c", "RETURN 1; RETURN 2"]));
    assert_eq!(status.code(), Some(0));
    // LABS-1120: each statement's stream closes with its timing line; no
    // progress report on success (the one typed discriminator,
    // `support::timing_terminal_line`).
    let data_lines: Vec<&str> = stdout
        .lines()
        .filter(|line| !support::timing_terminal_line(line))
        .collect();
    assert_eq!(data_lines, ["{\"1\":1}", "{\"2\":2}"], "{stdout}");
    assert_eq!(stdout.matches("\"timings\"").count(), 2, "{stdout}");
    assert!(stderr.is_empty(), "{stderr}");
}

#[test]
fn test_the_report_goes_to_stderr_in_every_mode_but_the_envelope_only_in_json_modes() {
    let (status, stdout, stderr) = run_cli(Opts::new(&["--csv", "-c", "RETURN 1; RETRN 2"]));
    assert_eq!(status.code(), Some(6));
    assert!(stderr.contains("statement 2 of 2 failed"), "{stderr}");
    assert!(
        !stdout.contains("statement"),
        "no envelope in csv: {stdout}"
    );
}
