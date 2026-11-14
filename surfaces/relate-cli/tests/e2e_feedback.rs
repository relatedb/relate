//! End-to-end tests for the two feedback channels (ADR-0588 §3): the
//! person's `relate feedback` (a prefilled GitHub issue plus the agent
//! grant's action words) and the agent's `relate agent-feedback` (one
//! consented report under that grant), each mode-gated.
//!
//! Executes the actual `relate` binary against a fake `$HOME` (tempdir) so
//! the consent record and the receipts live in an isolated
//! `$HOME/.config/relate/`, and — for the agent's send tests — against the
//! shared loopback Mixpanel mock (`MockMixpanel`, the telemetry suite's
//! capture sink). The person's issue path is not exercised here: it opens a
//! browser, and its URL derivation is pinned by `feedback_cmd`'s unit
//! tests.

mod support;

use std::path::Path;
use std::time::Duration;

use chrono::{Duration as Days, SecondsFormat, Utc};
use support::{run_cli, tempdir, CliRunOptions as Opts, MockMixpanel, QUIET_GRACE};

/// The agent's structured report, in its shortest valid form.
const AGENT_ARGS: [&str; 5] = [
    "agent-feedback",
    "--expected",
    "the info command to print the version",
    "--observed",
    "it printed help instead",
];

/// The agent surface's mode: the one agent-mode fact the CLI resolves from
/// the environment.
const AGENT_MODE: &[(&str, &str)] = &[("RELATE_AGENT_MODE", "1")];

/// A person's run: the harness scrub leaves no agent marker, and a piped
/// child is never a terminal.
fn human<'a>(home: &'a Path, args: &'a [&'a str]) -> Opts<'a> {
    Opts::new(args).in_home(home)
}

/// An agent's run: the explicit agent-mode variable, so the surface decision
/// does not depend on the host's harness markers.
fn agent<'a>(home: &'a Path, args: &'a [&'a str]) -> Opts<'a> {
    Opts::new(args).in_home(home).with_envs(AGENT_MODE)
}

/// The persisted feedback consent block from the fake home's config.json.
fn config_feedback(home: &Path) -> Option<serde_json::Value> {
    let path = home.join(".config").join("relate").join("config.json");
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get("feedback").cloned()
}

/// Write the standing grant a person's `enable` records: the destination and
/// a deadline inside the allowed horizon.
fn write_grant(home: &Path, endpoint: &str) {
    let dir = home.join(".config").join("relate");
    std::fs::create_dir_all(&dir).unwrap();
    let until = (Utc::now() + Days::days(30)).to_rfc3339_opts(SecondsFormat::Secs, true);
    let config = serde_json::json!({
        "feedback": {"endpoint": endpoint, "authorized-until": until}
    });
    std::fs::write(dir.join("config.json"), config.to_string()).unwrap();
}

/// The receipt log's lines, empty when no receipt was written.
fn receipts(home: &Path) -> Vec<serde_json::Value> {
    let path = home
        .join(".config")
        .join("relate")
        .join("feedback-receipts.jsonl");
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .map(|line| {
            serde_json::from_str(line).unwrap_or_else(|e| panic!("non-JSON receipt: {e}: {line}"))
        })
        .collect()
}

fn outcome(stdout: &str) -> serde_json::Value {
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("agent-feedback prints one JSON outcome line: {e}: {stdout}"))
}

/// The person's bare command prints the standing state plus the synopsis;
/// `enable` is refused on a piped stdin (the grant is a person's act at a
/// terminal) and records nothing.
#[test]
fn test_bare_state_and_the_piped_enable_refusal() {
    let home = tempdir();

    let (status, stdout, stderr) =
        run_cli(human(home.path(), &["feedback"]).with_default_telemetry());
    assert_eq!(status.code(), Some(0), "bare feedback: {stderr}");
    assert!(
        stdout.starts_with("feedback disabled (not enabled)"),
        "got: {stdout} {stderr}"
    );
    assert!(
        stdout.contains("Usage: relate feedback"),
        "the bare command prints its usage line: {stdout}"
    );

    let (status, _, stderr) =
        run_cli(human(home.path(), &["feedback", "enable"]).with_default_telemetry());
    assert_eq!(status.code(), Some(1), "piped enable must refuse: {stderr}");
    assert!(
        stderr.contains("terminal"),
        "the refusal names why: {stderr}"
    );
    assert!(
        config_feedback(home.path()).is_none(),
        "a refused grant records nothing"
    );

    // With a standing grant the bare command names the window and the
    // destination.
    let mock = MockMixpanel::start();
    write_grant(home.path(), &mock.url());
    let (status, stdout, stderr) =
        run_cli(human(home.path(), &["feedback"]).with_default_telemetry());
    assert_eq!(status.code(), Some(0), "state: {stderr}");
    assert!(
        stdout.starts_with("feedback enabled until "),
        "got: {stdout} {stderr}"
    );
    assert!(
        stdout.contains(&mock.url()),
        "the state names the destination: {stdout}"
    );
}

/// Each mode owns one surface; the other is a loud unknown command naming
/// it and the person-run grant. Help on the absent surface is the same
/// refusal.
#[test]
fn test_each_mode_owns_one_feedback_surface() {
    let home = tempdir();

    // A person's run cannot reach the agent's send.
    for args in [
        &AGENT_ARGS[..],
        &["agent-feedback", "--help"][..],
        &["agent-feedback"][..],
    ] {
        let (status, _, stderr) = run_cli(human(home.path(), args));
        assert_eq!(status.code(), Some(2), "{args:?}: {stderr}");
        assert!(
            stderr.contains("unknown command: agent-feedback"),
            "{args:?}: {stderr}"
        );
        assert!(
            stderr.contains("relate feedback enable"),
            "{args:?}: the hint names the person-run grant: {stderr}"
        );
    }

    // An agent's run cannot reach the person's command, its grant included.
    for args in [
        &["feedback"][..],
        &["feedback", "enable"][..],
        &["feedback", "--help"][..],
    ] {
        let (status, _, stderr) = run_cli(agent(home.path(), args));
        assert_eq!(status.code(), Some(2), "{args:?}: {stderr}");
        assert!(
            stderr.contains("unknown command: feedback"),
            "{args:?}: {stderr}"
        );
        assert!(
            stderr.contains("relate agent-feedback"),
            "{args:?}: the hint names the agent surface: {stderr}"
        );
    }
}

/// The top-level help lists the run's own surface only.
#[test]
fn test_help_lists_only_the_runs_surface() {
    let home = tempdir();
    let words = |stdout: &str| -> Vec<String> {
        stdout
            .split("\n\n")
            .find(|chunk| chunk.starts_with("Commands:"))
            .unwrap_or_else(|| panic!("no Commands: section:\n{stdout}"))
            .lines()
            .skip(1)
            .map(|line| line.split_whitespace().next().unwrap().to_string())
            .collect()
    };

    let (status, stdout, stderr) = run_cli(human(home.path(), &["--help"]));
    assert_eq!(status.code(), Some(0), "{stderr}");
    let human_words = words(&stdout);
    assert!(human_words.iter().any(|word| word == "feedback"));
    assert!(!human_words.iter().any(|word| word == "agent-feedback"));

    let (status, stdout, stderr) = run_cli(agent(home.path(), &["--help"]));
    assert_eq!(status.code(), Some(0), "{stderr}");
    let agent_words = words(&stdout);
    assert!(agent_words.iter().any(|word| word == "agent-feedback"));
    assert!(!agent_words.iter().any(|word| word == "feedback"));

    // Each mode's own page prints; the absent one was refused above.
    let (status, stdout, stderr) = run_cli(human(home.path(), &["feedback", "--help"]));
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(
        stdout.starts_with("Usage: relate feedback"),
        "{stdout} {stderr}"
    );
    let (status, stdout, stderr) = run_cli(agent(home.path(), &["agent-feedback", "--help"]));
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(
        stdout.starts_with("Usage: relate agent-feedback"),
        "{stdout} {stderr}"
    );
}

/// Without a standing grant the agent surface refuses with `not_enabled`,
/// exit 1, naming the person-run grant, and sends nothing.
#[test]
fn test_agent_surface_refuses_without_a_grant() {
    let home = tempdir();
    let mock = MockMixpanel::start();
    let (status, _, stderr) =
        run_cli(agent(home.path(), &AGENT_ARGS).with_feedback_endpoint(&mock.url()));
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("not_enabled"), "{stderr}");
    assert!(
        stderr.contains("relate feedback enable"),
        "the refusal names the person-run grant: {stderr}"
    );
    std::thread::sleep(QUIET_GRACE);
    assert_eq!(mock.request_count(), 0, "a refusal sends nothing");
    assert!(receipts(home.path()).is_empty());
}

/// With a standing grant the agent surface sends through the pipeline: the
/// default category is `friction`, the report reaches the sink, and one
/// receipt records it.
#[test]
fn test_agent_surface_sends_under_a_grant() {
    let home = tempdir();
    let mock = MockMixpanel::start();
    write_grant(home.path(), &mock.url());

    let (status, stdout, stderr) =
        run_cli(agent(home.path(), &AGENT_ARGS).with_feedback_endpoint(&mock.url()));
    assert_eq!(status.code(), Some(0), "send: {stderr} {stdout}");
    let doc = outcome(&stdout);
    assert_eq!(doc["outcome"], "sent", "{doc}");
    assert!(
        mock.wait_for_events(Duration::from_secs(5), |events| events.iter().any(|e| {
            e["event"] == "RELATE-CLI_AGENT_FEEDBACK"
                && e["properties"]["expected"] == "the info command to print the version"
                && e["properties"]["observed"] == "it printed help instead"
                && e["properties"]["category"] == "friction"
        })),
        "the report never reached the sink; captured: {:?}",
        mock.events()
    );
    let lines = receipts(home.path());
    assert_eq!(lines.len(), 1, "one receipt per send: {lines:?}");
    assert_eq!(lines[0]["outcome"], "sent", "{lines:?}");
    assert_eq!(lines[0]["request_id"], doc["request_id"], "{lines:?}");
}

/// `DO_NOT_TRACK` suppresses the agent's send before any POST, with a live
/// grant recorded.
#[test]
fn test_do_not_track_suppresses_the_agent_send() {
    let home = tempdir();
    let mock = MockMixpanel::start();
    write_grant(home.path(), &mock.url());

    let (status, stdout, stderr) = run_cli(
        agent(home.path(), &AGENT_ARGS)
            .with_feedback_endpoint(&mock.url())
            .with_envs(&[("DO_NOT_TRACK", "1")]),
    );
    assert_eq!(status.code(), Some(0), "opted-out send: {stderr} {stdout}");
    let doc = outcome(&stdout);
    assert_eq!(doc["outcome"], "skipped", "{doc}");
    assert_eq!(doc["reason"], "do_not_track", "{doc}");
    std::thread::sleep(QUIET_GRACE);
    assert_eq!(mock.request_count(), 0, "DO_NOT_TRACK must send nothing");
    assert!(
        receipts(home.path()).is_empty(),
        "an opt-out skip writes no receipt"
    );
}

/// A report the policy refuses never leaves the machine: exit 1 with the
/// outcome naming the field and detector, no POST, and one receipt that
/// records no payload text. Both rejection classes: an input-shape fault
/// (`invalid_input`) and a redaction hit (`policy_violation`, here the
/// retired category spelling).
#[test]
fn test_a_rejected_report_is_never_sent() {
    let home = tempdir();
    let mock = MockMixpanel::start();
    write_grant(home.path(), &mock.url());

    let cases: [([&str; 7], serde_json::Value); 2] = [
        (
            [
                "agent-feedback",
                "--category",
                "other_friction",
                "--expected",
                "an error",
                "--observed",
                "the run failed",
            ],
            serde_json::json!({"field": "category", "detector": "invalid_input"}),
        ),
        (
            [
                "agent-feedback",
                "--category",
                "friction",
                "--expected",
                "an error",
                "--observed",
                "it mentioned dev@example.com",
            ],
            serde_json::json!({"field": "observed", "detector": "email"}),
        ),
    ];
    for (args, violation) in &cases {
        let (status, stdout, stderr) =
            run_cli(agent(home.path(), &args[..]).with_feedback_endpoint(&mock.url()));
        assert_eq!(status.code(), Some(1), "{args:?}: {stderr}");
        let doc = outcome(&stdout);
        assert_eq!(doc["outcome"], "rejected", "{args:?}: {doc}");
        assert_eq!(&doc["violations"][0], violation, "{args:?}: {doc}");
        assert!(
            doc["payload"].is_null(),
            "{args:?}: a rejected report has no payload: {doc}"
        );
    }
    std::thread::sleep(QUIET_GRACE);
    assert_eq!(mock.request_count(), 0, "a rejected report is never sent");

    let lines = receipts(home.path());
    assert_eq!(
        lines.len(),
        cases.len(),
        "one receipt per rejection: {lines:?}"
    );
    for line in lines {
        assert_eq!(line["outcome"], "rejected", "{line}");
        assert!(
            line["payload"].is_null(),
            "a rejection receipt records no report text: {line}"
        );
    }
}

#[test]
fn test_usage_errors_exit_2() {
    let home = tempdir();
    for (agent_mode, args) in [
        (false, &["feedback", "enable", "--expected", "x"][..]),
        (false, &["feedback", "one", "two"][..]),
        (
            false,
            &["feedback", "--category", "not_a_category", "text"][..],
        ),
        (true, &["agent-feedback"][..]),
        (true, &["agent-feedback", "--observed", "y"][..]),
        (
            true,
            &[
                "agent-feedback",
                "--expected",
                "x",
                "--observed",
                "y",
                "--bogus",
                "z",
            ][..],
        ),
    ] {
        let opts = if agent_mode {
            agent(home.path(), args)
        } else {
            human(home.path(), args)
        };
        let (status, _, stderr) = run_cli(opts);
        assert_eq!(status.code(), Some(2), "{args:?}: {stderr}");
        let usage = if agent_mode {
            "Usage: relate agent-feedback"
        } else {
            "Usage: relate feedback"
        };
        assert!(stderr.contains(usage), "{args:?}: {stderr}");
    }
}
