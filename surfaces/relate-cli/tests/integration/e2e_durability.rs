use crate::support;

use relate_test_support::durability::DURABILITY_MODES;
use support::{run_cli, CliRunOptions};

fn run(args: &[&str]) -> std::process::Output {
    support::cli_command().args(args).output().unwrap()
}

#[test]
fn durability_mode_values_help_and_errors() {
    for &(mode, _) in DURABILITY_MODES {
        let output = run(&["--durability-mode", mode, "RETURN 1 AS n"]);
        assert!(
            output.status.success(),
            "{mode}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let invalid = run(&["--durability-mode", "later", "RETURN 1"]);
    assert_eq!(invalid.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&invalid.stderr)
        .contains("expected full, everysecond, or snapshot"));

    let missing = run(&["--durability-mode"]);
    assert_eq!(missing.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("requires a value"));

    let help = run(&["--help"]);
    let help_text = String::from_utf8_lossy(&help.stdout);
    assert!(help_text.contains("--durability-mode MODE"));
    assert!(help_text.contains("default full"));
    // ADR-0470: the env-var default is part of the row.
    assert!(help_text.contains("RELATE_DURABILITY_MODE"));

    let omitted = run(&["RETURN 1 AS n"]);
    assert!(
        omitted.status.success(),
        "omitted --durability-mode must default to full: {}",
        String::from_utf8_lossy(&omitted.stderr)
    );
}

/// ADR-0470: `RELATE_DURABILITY_MODE` is the flag's default — each
/// mode value is accepted through the variable alone, the flag wins over
/// a set variable, and a bad variable value exits 2 naming it (the same
/// rejection as a bad flag, never a silent default).
#[test]
fn durability_mode_env_default_flag_wins_and_bad_value_fails() {
    for &(mode, _) in DURABILITY_MODES {
        let (status, _stdout, stderr) = run_cli(
            CliRunOptions::new(&["RETURN 1 AS n"]).with_envs(&[("RELATE_DURABILITY_MODE", mode)]),
        );
        assert!(status.success(), "{mode}: {stderr}");
    }

    // The flag wins over the variable: a valid flag with a bad variable
    // still runs — the variable is not consulted for an explicit flag.
    let (status, _stdout, stderr) = run_cli(
        CliRunOptions::new(&["--durability-mode", "full", "RETURN 1 AS n"])
            .with_envs(&[("RELATE_DURABILITY_MODE", "later")]),
    );
    assert!(
        status.success(),
        "the flag must win over the variable: {stderr}"
    );

    let (status, _stdout, stderr) = run_cli(
        CliRunOptions::new(&["RETURN 1 AS n"]).with_envs(&[("RELATE_DURABILITY_MODE", "later")]),
    );
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("invalid RELATE_DURABILITY_MODE"),
        "{stderr}"
    );
    assert!(
        stderr.contains("expected full, everysecond, or snapshot"),
        "{stderr}"
    );
}
