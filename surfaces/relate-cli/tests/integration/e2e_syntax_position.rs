//! End-to-end tests for syntax-error positions (ADR-0290): the caret on stderr
//! and `column`/`source`/`line` in the error envelope.

use crate::support;

use support::{run_cli, CliRunOptions as Opts};

fn error_object(stdout: &str) -> serde_json::Value {
    let last = stdout.lines().last().unwrap_or_else(|| panic!("no stdout"));
    serde_json::from_str::<serde_json::Value>(last).unwrap_or_else(|e| panic!("{e}: {last}"))
        ["error"]
        .clone()
}

/// The text of `source` at the character the stderr caret points at.
fn under_caret(stderr: &str) -> String {
    let lines: Vec<&str> = stderr.lines().collect();
    let (source, caret) = (
        lines[1].strip_prefix("  ").expect("indented source"),
        lines[2].strip_prefix("  ").expect("indented caret"),
    );
    let at = caret.chars().position(|c| c == '^').expect("caret");
    source.chars().skip(at).collect()
}

/// The text of the envelope's `source` from the character its `column` names.
fn from_column(error: &serde_json::Value) -> String {
    let column = error["column"].as_u64().expect("column") as usize;
    let source = error["source"].as_str().expect("source");
    source.chars().skip(column - 1).collect()
}

#[test]
fn test_stderr_prints_the_source_and_a_caret_on_their_own_lines() {
    let (status, _, stderr) = run_cli(Opts::new(&["RETURN 1 AS x, 2 AS y RETRN 3"]));
    assert_eq!(status.code(), Some(6));
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 3, "stderr: {stderr}");
    // The first line is what it always was: the prefix and the message.
    assert!(
        lines[0].starts_with("syntax error: parse error: unexpected token"),
        "{stderr}"
    );
    assert_eq!(lines[1], "  RETURN 1 AS x, 2 AS y RETRN 3");
    assert!(under_caret(&stderr).starts_with("RETRN"), "{stderr}");
}

#[test]
fn test_the_envelope_carries_column_and_source_and_keeps_its_message() {
    let (_, stdout, stderr) = run_cli(Opts::new(&["--json", "RETURN 1 AS x, 2 AS y RETRN 3"]));
    let e = error_object(&stdout);
    assert_eq!(e["kind"], "syntax");
    assert_eq!(e["source"], "RETURN 1 AS x, 2 AS y RETRN 3");
    assert!(from_column(&e).starts_with("RETRN"), "{e}");
    // `message` is the established one-line text, and so is stderr's first line.
    let message = e["message"].as_str().expect("message");
    assert!(
        message.starts_with("syntax error: parse error: unexpected token"),
        "{message}"
    );
    assert!(!message.contains('\n'), "{message:?}");
    assert!(
        stderr.starts_with(&message[..message.find("  ").expect("message has a double space")]),
        "{stderr}"
    );
}

#[test]
fn test_the_column_counts_characters_after_multibyte_text() {
    let (_, stdout, stderr) = run_cli(Opts::new(&["--jsonl", "RETURN 1 AS é, 2 AS ü RETRN 3"]));
    let e = error_object(&stdout);
    assert!(from_column(&e).starts_with("RETRN"), "{e}");
    assert!(under_caret(&stderr).starts_with("RETRN"), "{stderr}");
}

#[test]
fn test_a_control_character_before_the_error_is_removed_and_the_caret_follows() {
    // ESC inside a string literal, then a later syntax error: the sanitized
    // source is one character shorter, so the column must move with it.
    let (_, stdout, stderr) = run_cli(
        Opts::new(&["query", "--jsonl"]).with_stdin(b"RETURN 'a\x1b[31mb' AS x RETRN 2;\n"),
    );
    assert!(!stderr.contains('\x1b'), "stderr: {stderr:?}");
    assert!(!stdout.contains('\x1b'), "stdout: {stdout:?}");
    assert!(under_caret(&stderr).starts_with("RETRN"), "{stderr}");
    assert!(from_column(&error_object(&stdout)).starts_with("RETRN"));
}

#[test]
fn test_each_failing_statement_of_a_script_points_into_its_own_text() {
    // The position is relative to the statement the parser was given.
    let (_, stdout, stderr) = run_cli(
        Opts::new(&["query", "--jsonl"]).with_stdin(b"RETURN 1 AS a;\nRETURN 2 AS b RETRN 3;\n"),
    );
    assert!(under_caret(&stderr).starts_with("RETRN"), "{stderr}");
    let e = error_object(&stdout);
    assert_eq!(e["source"], "RETURN 2 AS b RETRN 3");
}

#[test]
fn test_an_error_without_a_known_position_is_reported_as_before() {
    // The parser reports `line 0, col 0` here: no position, so no extra
    // stderr lines and no position fields.
    let (status, stdout, stderr) = run_cli(Opts::new(&["--json", "RETURN ("]));
    assert_eq!(status.code(), Some(6));
    assert_eq!(stderr.lines().count(), 1, "stderr: {stderr}");
    let e = error_object(&stdout);
    for field in ["line", "column", "source"] {
        assert!(e.get(field).is_none(), "{field} present: {e}");
    }
}

#[test]
fn test_other_errors_gain_no_position_fields_and_stay_one_line() {
    let (status, stdout, stderr) = run_cli(Opts::new(&["--json", "--read-only", "CREATE (:A)"]));
    assert_eq!(status.code(), Some(6));
    assert_eq!(stderr.lines().count(), 1, "stderr: {stderr}");
    let e = error_object(&stdout);
    assert_eq!(e["kind"], "semantic");
    for field in ["line", "column", "source"] {
        assert!(e.get(field).is_none(), "{field} present: {e}");
    }
}

#[test]
fn test_non_json_modes_get_the_caret_and_no_stdout() {
    for args in [
        &["--csv", "RETURN 1 AS x RETRN 2"][..],
        &["--markdown", "RETURN 1 AS x RETRN 2"],
        &["--mode", "table", "RETURN 1 AS x RETRN 2"],
        &["--mode", "cypher", "RETURN 1 AS x RETRN 2"],
    ] {
        let (status, stdout, stderr) = run_cli(Opts::new(args));
        assert_eq!(status.code(), Some(6), "{args:?}");
        assert!(stdout.is_empty(), "{args:?} stdout: {stdout}");
        assert!(
            under_caret(&stderr).starts_with("RETRN"),
            "{args:?}: {stderr}"
        );
    }
}
