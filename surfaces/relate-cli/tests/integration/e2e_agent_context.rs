//! End-to-end tests for `relate agent-context [COMMAND]` (ADR-0242,
//! LABS-236): the compact discovery readout rendered from the same
//! COMMANDS table as `--help`.

#![cfg(feature = "agent-context")]

use crate::support;

use support::{run_cli, CliRunOptions as Opts};

#[test]
fn test_agent_context_index_lists_commands_flags_and_exit_codes() {
    let (status, stdout, stderr) = run_cli(Opts::new(&["agent-context"]));
    assert!(status.success(), "agent-context failed: {stderr}");
    // The index names the always-compiled commands and the exit codes.
    for expected in [
        "agent context",
        "query",
        "clone",
        "Query-run flags",
        "--database",
        "--timeout",
        "Exit codes:",
        "7  timeout",
        "6  validation_error",
        "2  usage_error",
    ] {
        assert!(stdout.contains(expected), "{expected:?} missing:\n{stdout}");
    }
}

#[test]
fn test_agent_context_detail_matches_the_help_table() {
    let (status, stdout, stderr) = run_cli(Opts::new(&["agent-context", "query"]));
    assert!(status.success(), "agent-context query failed: {stderr}");
    for expected in [
        "Usage: relate query [STATEMENT] [--database PATH]",
        "Flags:",
        "--database",
        "--format",
        "Examples:",
        "relate query \"RETURN 1 AS n\" --database ./my-db",
        "Exit codes:",
    ] {
        assert!(stdout.contains(expected), "{expected:?} missing:\n{stdout}");
    }

    // The `update` alias resolves to the canonical `upgrade` detail.
    #[cfg(feature = "upgrade")]
    {
        let (status, stdout, stderr) = run_cli(Opts::new(&["agent-context", "update"]));
        assert!(status.success(), "agent-context update failed: {stderr}");
        assert!(
            stdout.contains("Usage: relate upgrade [--yes] [--check]"),
            "got: {stdout}"
        );
    }
}

#[test]
fn test_agent_context_unknown_command_is_a_usage_error() {
    let (status, _stdout, stderr) = run_cli(Opts::new(&["agent-context", "bogus"]));
    assert_eq!(
        status.code(),
        Some(2),
        "expected usage error, got: {stderr}"
    );
    assert!(
        stderr.contains("unknown command 'bogus'"),
        "expected the valid-words error, got: {stderr}"
    );
}

#[test]
fn test_agent_context_rejects_a_query() {
    // It runs no query engine (ADR-0069 rejects_query), like clone/info.
    let (status, _stdout, stderr) = run_cli(Opts::new(&["agent-context", "RETURN 1"]));
    assert_eq!(status.code(), Some(2), "stderr: {stderr}");
}

#[test]
fn test_agent_context_help_page() {
    let (status, stdout, stderr) = run_cli(Opts::new(&["agent-context", "--help"]));
    assert!(status.success(), "agent-context --help failed: {stderr}");
    assert!(
        stdout.starts_with("Usage: relate agent-context [COMMAND]"),
        "got: {stdout}"
    );
}

#[test]
fn test_agent_context_output_stays_compact() {
    // ADR-0242: the discovery readout must itself respect the compactness
    // rule (neo4j-cli's equivalent is ~405 KB); ours stays in the low KB.
    let (_status, stdout, _stderr) = run_cli(Opts::new(&["agent-context"]));
    assert!(
        stdout.len() < 8 * 1024,
        "agent-context index grew to {} bytes",
        stdout.len()
    );
}
