//! End-to-end tests for the command did-you-mean fallback (ADR-0230): a
//! failed parse whose first word is plausibly a mistyped command keeps the
//! parse error as the headline and gains one `did you mean` line below it.

use crate::support;

use support::{run_cli, CliRunOptions as Opts};

/// The one spelling of the ADR-0230 suggestion line every assertion
/// compares against.
fn suggestion_line(word: &str, tail: &str) -> String {
    format!("did you mean \"{word}\"?  -- {tail}")
}

/// The suggestion line and its separating blank line are the last thing on
/// stderr; the parse error above is byte-for-byte the established text.
fn assert_suggestion(stderr: &str, word: &str, tail: &str) {
    let lines: Vec<&str> = stderr.lines().collect();
    let [.., blank, suggestion] = lines.as_slice() else {
        panic!("stderr: {stderr}")
    };
    assert!(blank.is_empty(), "stderr: {stderr}");
    assert_eq!(suggestion, &suggestion_line(word, tail), "stderr: {stderr}");
}

/// A near-miss first positional fails the parse (exit 6) and ends stderr
/// with the suggestion for `word` and its synopsis `tail`.
fn near_miss_suggests(typed: &str, word: &str, tail: &str) -> String {
    let (status, _, stderr) = run_cli(Opts::new(&[typed]));
    assert_eq!(status.code(), Some(6), "{typed}");
    assert_suggestion(&stderr, word, tail);
    stderr
}

#[test]
fn test_a_mistyped_command_word_suggests_the_command() {
    let stderr = near_miss_suggests(
        "qu",
        "query",
        "run a statement session (oneshot, stdin, or shell)",
    );
    assert!(
        stderr.starts_with("syntax error: parse error: unexpected token \"qu\""),
        "stderr: {stderr}"
    );
}

#[test]
fn test_a_near_miss_of_clone_suggests_clone() {
    near_miss_suggests("clon", "clone", "copy a database into a new store and exit");
}

#[cfg(feature = "telemetry")]
#[test]
fn test_a_prefix_of_telemetry_suggests_telemetry() {
    near_miss_suggests(
        "telemetr",
        "telemetry",
        "manage anonymous usage metrics (on, off, status)",
    );
}

#[cfg(feature = "upgrade")]
#[test]
fn test_a_near_miss_of_the_update_alias_reports_upgrade() {
    near_miss_suggests(
        "updat",
        "upgrade",
        "update the relate binary (alias: update)",
    );
}

#[test]
fn test_a_genuine_parse_error_gets_no_suggestion() {
    for args in [
        &["MATCH (n RETURN n"][..],
        &["RETURN 1 +"][..],
        &["frobnicate everything"][..],
    ] {
        let (status, _, stderr) = run_cli(Opts::new(args));
        assert_eq!(status.code(), Some(6), "{args:?}");
        assert!(!stderr.contains("did you mean"), "{args:?}: {stderr}");
    }
}

#[test]
fn test_valid_cypher_with_a_command_like_first_token_is_untouched() {
    // `query` as a bare variable is valid Cypher; the fallback never runs
    // for a statement that parses.
    let (status, stdout, stderr) = run_cli(Opts::new(&["WITH 1 AS query RETURN query"]));
    assert!(status.success(), "stderr: {stderr}");
    assert!(stdout.contains('1'), "stdout: {stdout}");
    assert!(!stderr.contains("did you mean"), "stderr: {stderr}");
}

#[test]
fn test_the_suggestion_stays_out_of_the_json_envelope() {
    let (status, stdout, stderr) = run_cli(Opts::new(&["--json", "qu"]));
    assert_eq!(status.code(), Some(6));
    assert!(
        stderr.contains("did you mean \"query\""),
        "stderr: {stderr}"
    );
    let error = serde_json::from_str::<serde_json::Value>(stdout.lines().last().expect("envelope"))
        .expect("envelope is JSON")["error"]
        .clone();
    assert_eq!(error["kind"], "syntax");
    assert!(
        error.get("suggestion").is_none(),
        "envelope gains no field: {error}"
    );
}

/// A usage error raised after a mistyped command word (ADR-0518): the
/// usage error stays the headline, the suggestion line follows it after a
/// blank line, and the usage synopsis and hint follow — the same line the
/// engine fallback renders, on the exit that fires before the engine.
/// Exit code stays 2.
fn usage_error_after_near_miss(args: &[&str], headline: &str, word: &str, tail: &str) {
    let (status, _, stderr) = run_cli(Opts::new(args));
    assert_eq!(status.code(), Some(2), "{args:?}");
    let lines: Vec<&str> = stderr.lines().collect();
    let at = lines
        .iter()
        .position(|l| *l == headline)
        .unwrap_or_else(|| panic!("no headline {headline:?} in {stderr:?}"));
    let [blank, suggestion, usage, ..] = &lines[at + 1..] else {
        panic!("stderr: {stderr}")
    };
    assert!(blank.is_empty(), "stderr: {stderr}");
    assert_eq!(*suggestion, suggestion_line(word, tail), "stderr: {stderr}");
    assert!(usage.starts_with("Usage: relate"), "stderr: {stderr}");
}

#[test]
fn test_an_unknown_flag_after_a_mistyped_command_suggests_the_command() {
    // The exact repro from the ticket: `service` alone reaches the engine
    // fallback; `service --bolt 7687` exits on the flag first.
    usage_error_after_near_miss(
        &["qu", "--bolt", "7687"],
        "Error: unknown flag: --bolt",
        "query",
        "run a statement session (oneshot, stdin, or shell)",
    );
}

#[cfg(feature = "serve")]
#[test]
fn test_the_serve_repro_suggests_serve() {
    usage_error_after_near_miss(
        &["service", "--bolt", "7687"],
        "Error: unknown flag: --bolt",
        "serve",
        "host the HTTP Query API and MCP surfaces on a database",
    );
}

#[cfg(feature = "serve")]
#[test]
fn test_an_unexpected_argument_after_a_mistyped_command_suggests_it() {
    usage_error_after_near_miss(
        &["service", "extra"],
        "Error: unexpected argument: extra",
        "serve",
        "host the HTTP Query API and MCP surfaces on a database",
    );
}

#[test]
fn test_the_command_suggestion_replaces_the_flag_suggestion() {
    // ADR-0518 §3: when the pending statement matches a command, the flag
    // suggestion — computed from the query run's flag list, the wrong
    // scope for the command the user meant — is not printed beside it.
    let (status, _, stderr) = run_cli(Opts::new(&["querry", "--databse", "x"]));
    assert_eq!(status.code(), Some(2));
    assert!(
        stderr.contains("did you mean \"query\"?"),
        "stderr: {stderr}"
    );
    assert!(
        !stderr.contains("Did you mean '--database'?"),
        "stderr: {stderr}"
    );
}

#[test]
fn test_a_valid_statement_with_an_unknown_flag_gains_no_command_suggestion() {
    // No near-miss first word: the exit prints exactly the pre-ADR-0518
    // output, flag suggestion rules included (ADR-0192).
    let (status, _, stderr) = run_cli(Opts::new(&["RETURN 1", "--bolt"]));
    assert_eq!(status.code(), Some(2));
    assert!(!stderr.contains("did you mean"), "stderr: {stderr}");
    assert!(stderr.contains("unknown flag: --bolt"), "stderr: {stderr}");
}
