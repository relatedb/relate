//! The jsonl stream's contract end-to-end: ADR-0107's piped default and
//! the explicit `--jsonl` / `--mode jsonl` invocations render pure data
//! rows — a zero-row statement prints nothing, an N-row statement exactly
//! N lines — and ADR-0581's `--timings` opts the terminal timing line
//! back in. Split out of `e2e_cli.rs` (the ported Go-suite grab-bag) at
//! the 1k-line budget's edge — the same topical cut the directory makes
//! for the row cap (`e2e_row_cap.rs`) and the envelope
//! (`e2e_envelope.rs`).

#[allow(dead_code)]
mod support;

use support::{run_cli, CliRunOptions as Opts};

#[test]
fn test_single_query() {
    // Piped stdout (not a TTY) defaults to JSONL (ADR-0107).
    let (status, stdout, stderr) = run_cli(Opts::new(&["RETURN 42 AS answer;"]));
    assert!(status.success(), "single query failed: {stderr}");
    // The jsonl stream is pure rows (ADR-0581): the row line is the whole
    // stream — the terminal timing line is the `--timings` opt-in.
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "pure rows, no extra line: {stdout}");
    assert_eq!(
        lines[0], r#"{"answer":42}"#,
        "piped one-shot should default to JSONL: {stdout}"
    );
}

#[test]
fn test_single_query_arithmetic() {
    // Piped stdout (not a TTY) defaults to JSONL (ADR-0107).
    let (status, stdout, stderr) = run_cli(Opts::new(&["RETURN 1 + 1 AS sum;"]));
    assert!(status.success(), "arithmetic query failed: {stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "pure rows, no extra line: {stdout}");
    assert_eq!(
        lines[0], r#"{"sum":2}"#,
        "piped one-shot should default to JSONL: {stdout}"
    );
}

#[test]
fn test_piped_default_is_jsonl() {
    let (status, stdout, stderr) = run_cli(Opts::new(&["-c", "RETURN 1 AS n"]));
    assert!(status.success(), "piped query failed: {stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "pure rows, no extra line: {stdout}");
    assert_eq!(
        lines[0], r#"{"n":1}"#,
        "piped stdout must default to JSONL: {stdout}"
    );
}

#[test]
fn test_single_query_jsonl() {
    // `-jsonl` is shorthand for `--mode jsonl`: one JSON object per line.
    let (status, stdout, stderr) = run_cli(Opts::new(&["-jsonl", "UNWIND [1, 2] AS n RETURN n;"]));
    assert!(status.success(), "jsonl query failed: {stderr}");
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    // ADR-0581: the two data rows and nothing else — the terminal timing
    // line is the `--timings` opt-in.
    assert_eq!(
        lines.len(),
        2,
        "expected one JSON object per line, got: {stdout}"
    );
    assert_eq!(lines[0], r#"{"n":1}"#, "{stdout}");
    assert_eq!(lines[1], r#"{"n":2}"#, "{stdout}");
    for line in &lines {
        let obj: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("invalid JSON line ({e}): {line}"));
        assert!(
            obj.is_object(),
            "expected a JSON object per line, got: {line}"
        );
    }
}

/// ADR-0581: the jsonl stream is pure rows by default — the terminal
/// timing line is the `--timings` opt-in. A zero-row MATCH prints an
/// empty stdout; an N-row UNWIND prints exactly N lines. Both the piped
/// auto-default (ADR-0107) and an explicit `--mode jsonl` under a
/// detected harness — where the piped default would be toon (ADR-0426)
/// — keep the same row-only contract.
#[test]
fn test_jsonl_default_is_pure_rows() {
    // The piped auto-default: no mode flag, stdout piped → jsonl.
    let (status, stdout, stderr) = run_cli(Opts::new(&["MATCH (n:Nope) RETURN n.p AS p"]));
    assert!(status.success(), "{stderr}");
    assert!(
        stdout.is_empty(),
        "a zero-row MATCH prints no line: {stdout}"
    );

    let (status, stdout, stderr) = run_cli(Opts::new(&["UNWIND range(1, 5) AS i RETURN i"]));
    assert!(status.success(), "{stderr}");
    assert_eq!(
        stdout.lines().count(),
        5,
        "exactly one line per row, nothing else: {stdout}"
    );

    // CLAUDECODE=1 detection would default a piped run to toon; an
    // explicit --mode jsonl pins the row-only stream there too.
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["--mode", "jsonl", "MATCH (n:Nope) RETURN n.p AS p"])
            .with_envs(&[("CLAUDECODE", "1")]),
    );
    assert!(status.success(), "{stderr}");
    assert!(
        stdout.is_empty(),
        "a zero-row MATCH prints no line: {stdout}"
    );

    let (status, stdout, stderr) = run_cli(
        Opts::new(&["--mode", "jsonl", "UNWIND range(1, 5) AS i RETURN i"])
            .with_envs(&[("CLAUDECODE", "1")]),
    );
    assert!(status.success(), "{stderr}");
    assert_eq!(
        stdout.lines().count(),
        5,
        "exactly one line per row, nothing else: {stdout}"
    );
}

/// ADR-0581: `--timings` opts the jsonl stream back into the terminal
/// timing line — one keyed line after the rows closes the stream.
#[test]
fn test_jsonl_timings_opt_in() {
    let (status, stdout, stderr) = run_cli(Opts::new(&[
        "--jsonl",
        "--timings",
        "UNWIND [1, 2] AS n RETURN n",
    ]));
    assert!(status.success(), "{stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 3, "two rows, then the timing line: {stdout}");
    assert_eq!(lines[0], r#"{"n":1}"#, "{stdout}");
    assert_eq!(lines[1], r#"{"n":2}"#, "{stdout}");
    // The one typed discriminator every JSONL suite uses for the rider's
    // shape (the two-key object), not a substring re-derivation.
    assert!(
        support::timing_terminal_line(lines[2]),
        "the timing line closes the stream: {stdout}"
    );
}
