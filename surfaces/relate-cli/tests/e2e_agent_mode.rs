//! The agent-mode ladder (ADR-0407) end-to-end: the flag/env/detection
//! precedence, the one-line announcement the explicit acts print (and
//! detection never does), the exit-2 usage error, and the generic
//! cross-harness markers — the row cap itself lives in `e2e_row_cap.rs`
//! (ADR-0196); this file carries the mode that feeds it.

mod support;

use support::{run_cli, CliRunOptions as Opts};

const QUERY: &str = "UNWIND range(1, 250) AS i RETURN i";

fn run(args: &[&str], envs: &[(&str, &str)]) -> (std::process::ExitStatus, String, String) {
    let mut all = vec!["--jsonl"];
    all.extend_from_slice(args);
    all.push(QUERY);
    run_cli(Opts::new(&all).with_envs(envs))
}

/// The agent-mode override ladder (ADR-0407): the flag, then
/// `RELATE_AGENT_MODE`, then detection — the explicit acts are
/// unconditional (and announce themselves once), detection stays
/// gated and silent.
#[test]
fn test_the_agent_override_ladder() {
    // --no-agent-mode beats detection: no cap, no record, no announcement.
    let (status, stdout, stderr) = run(&["--no-agent-mode"], &[("CLAUDECODE", "1")]);
    assert!(status.success(), "{stderr}");
    // LABS-1120: the machine stream closes with the timing terminal line
    // (250 rows + 1), so an agent reads where the time went in-band.
    assert_eq!(stdout.lines().count(), 251, "stdout: {stdout}");
    assert!(
        stdout
            .lines()
            .last()
            .is_some_and(|l| l.contains("\"timings\"")),
        "the timings line closes the stream: {stdout}"
    );
    assert!(!stderr.contains("truncated"), "stderr: {stderr}");
    assert!(!stderr.contains("agent mode on"), "stderr: {stderr}");

    // RELATE_AGENT_MODE=0 is the same explicit off.
    let (_, stdout, stderr) = run(&[], &[("RELATE_AGENT_MODE", "0"), ("CLAUDECODE", "1")]);
    assert_eq!(stdout.lines().count(), 251, "stdout: {stdout}");
    assert!(!stderr.contains("agent mode on"), "stderr: {stderr}");

    // --agent-mode without any harness: the explicit act is unconditional.
    let (status, stdout, stderr) = run(&["--agent-mode"], &[]);
    assert_eq!(status.code(), Some(0), "{stderr}");
    // 100 capped rows + the rowCap record + the timings line.
    assert_eq!(stdout.lines().count(), 102, "stdout: {stdout}");
    assert!(
        stderr.contains("relate agent mode on: --agent-mode (--no-agent-mode to turn off)"),
        "stderr: {stderr}"
    );

    // RELATE_AGENT_MODE=1 announces itself too — detection never does.
    let (_, stdout, stderr) = run(&[], &[("RELATE_AGENT_MODE", "1")]);
    assert_eq!(stdout.lines().count(), 102, "stdout: {stdout}");
    assert!(
        stderr.contains("relate agent mode on: RELATE_AGENT_MODE=1"),
        "stderr: {stderr}"
    );
    let (_, stdout, stderr) = run(&[], &[("CLAUDECODE", "1")]);
    assert_eq!(stdout.lines().count(), 102, "stdout: {stdout}");
    assert!(!stderr.contains("agent mode on"), "stderr: {stderr}");

    // An invalid value exits 2 with the usage error, like MAX_ROWS.
    let (status, _, stderr) = run(&[], &[("RELATE_AGENT_MODE", "yes")]);
    assert_eq!(status.code(), Some(2), "stderr: {stderr}");
    assert!(
        stderr.contains("invalid RELATE_AGENT_MODE"),
        "stderr: {stderr}"
    );
}

/// The generic cross-harness markers count as detection (ADR-0407): the
/// AI_AGENT proposal's own pair caps a piped run like the harness-specific
/// variables always have.
#[test]
fn test_the_generic_agent_markers_detect() {
    for name in ["AI_AGENT", "AGENT"] {
        let (_, stdout, stderr) = run(&[], &[(name, "claude-code")]);
        assert_eq!(stdout.lines().count(), 102, "{name}");
        assert!(
            !stderr.contains("agent mode on"),
            "detection stays silent: {name}"
        );
    }
}

/// The toon piped default (ADR-0426): agent ergonomics in effect default
/// a piped run's mode to `toon` — the row cap's terminal record rides
/// the mode — while a human pipeline keeps JSONL, and an explicit
/// `--mode jsonl` pins the object lines for an agent that wants them.
#[test]
fn test_agent_piped_runs_default_to_toon() {
    // Detection: piped + harness, no mode flag → toon, capped at 100,
    // with the rowCap terminal entry in toon's shape (header + 100 rows
    // + the rowCap key line and its inline value).
    let (status, stdout, stderr) = run_cli(Opts::new(&[QUERY]).with_envs(&[("CLAUDECODE", "1")]));
    assert!(status.success(), "{stderr}");
    assert!(stdout.starts_with("rows[100]{i}:"), "stdout: {stdout}");
    assert!(stdout.contains("\nrowCap:\n"), "stdout: {stdout}");
    // LABS-1120: the timing block's three terminal entries (timings:, its
    // inline object, planCache:, its token) ride after the rowCap record.
    assert!(stdout.contains("\ntimings:\n"), "stdout: {stdout}");
    assert!(stdout.contains("\nplanCache:\n"), "stdout: {stdout}");
    assert_eq!(stdout.lines().count(), 107, "stdout: {stdout}");

    // The explicit env act resolves the same default.
    let (_, stdout, _) = run_cli(Opts::new(&[QUERY]).with_envs(&[("RELATE_AGENT_MODE", "1")]));
    assert!(stdout.starts_with("rows[100]{i}:"), "stdout: {stdout}");

    // A human pipeline — no harness, mode off — keeps the JSONL objects.
    let (_, stdout, _) = run_cli(Opts::new(&[QUERY]));
    assert!(stdout.starts_with("{\"i\":1}"), "stdout: {stdout}");

    // An explicit --mode jsonl pins the object lines even under detection.
    let (_, stdout, _) =
        run_cli(Opts::new(&["--mode", "jsonl", QUERY]).with_envs(&[("CLAUDECODE", "1")]));
    assert!(stdout.starts_with("{\"i\":1}"), "stdout: {stdout}");
}
