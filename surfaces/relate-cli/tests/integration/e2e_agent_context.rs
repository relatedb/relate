//! End-to-end tests for `relate agent-context [COMMAND]` (ADR-0242,
//! LABS-236): the compact discovery readout rendered from the same
//! COMMANDS table as `--help`.

#![cfg(feature = "agent-context")]

use crate::support;

use support::{run_cli, tempdir, CliRunOptions as Opts};

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

/// The agent-context index obeys the run's surface: the Feedback block and
/// the agent command row appear in an agent run only — the same mode gate
/// the top-level help applies (ADR-0588 §3, revised).
#[test]
fn test_agent_context_feedback_block_is_agent_mode_only() {
    let home = tempdir();

    let agent = Opts::new(&["agent-context"])
        .in_home(home.path())
        .with_envs(&[("RELATE_AGENT_MODE", "1")]);
    let (status, stdout, stderr) = run_cli(agent);
    assert!(status.success(), "{stderr}");
    assert!(stdout.contains("Feedback:"), "{stdout}");
    assert!(
        stdout.contains("relate agent-feedback") && stdout.contains("only after their approval"),
        "the block carries the synopsis and the report-first instruction: {stdout}"
    );

    let (status, stdout, stderr) = run_cli(Opts::new(&["agent-context"]).in_home(home.path()));
    assert!(status.success(), "{stderr}");
    assert!(
        !stdout.contains("Feedback:") && !stdout.contains("agent-feedback"),
        "a person's index carries neither the block nor the agent row: {stdout}"
    );
}

/// The other mode's feedback surface is not reachable through the detail
/// path either: a person's run asking for `agent-feedback` gets the same
/// absent-surface refusal the command dispatch gives, not the agent page
/// (ADR-0588 §3, revised).
#[test]
fn test_agent_context_detail_refuses_the_other_modes_surface() {
    let home = tempdir();
    let (status, stdout, stderr) =
        run_cli(Opts::new(&["agent-context", "agent-feedback"]).in_home(home.path()));
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("unknown command: agent-feedback"),
        "the absent surface is the dispatch's refusal: {stderr}"
    );
    assert!(!stdout.contains("Feedback:"), "{stdout}");

    // The agent's own run prints the detail page as usual.
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["agent-context", "agent-feedback"])
            .in_home(home.path())
            .with_envs(&[("RELATE_AGENT_MODE", "1")]),
    );
    assert!(status.success(), "{stderr}");
    assert!(stdout.contains("agent-feedback"), "{stdout}");
}
