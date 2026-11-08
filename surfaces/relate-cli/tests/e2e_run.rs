//! `relate run <name> [flags]` end-to-end (ADR-0429 §1, LABS-552):
//! the mount's user-facing contract through the real binary — the
//! pinned discovery error (exit 2, never a fetch), the missing-name
//! listing, the two help pages, the action-scope usage error with its
//! manifest-data synopsis and suggestion, the fail-closed registry
//! load, and (on unix lanes, like the wire suites) one installed `sh`
//! child driven through the wire into a store. Every test pins
//! `$XDG_CONFIG_HOME` to a tempdir — the registry never reads the
//! machine's real extensions dir from a test.

#![cfg(feature = "extensions")]

mod support;

use support::extensions::{
    install_package, install_sh_child, mcp_child_body, run_in_xdg, xdg_root, RUN_HINT, RUN_USAGE,
};
use support::{run_cli, tempdir, CliRunOptions as Opts};

#[cfg(all(unix, feature = "telemetry"))]
use support::MockMixpanel;

/// A fixture manifest: `name`, every flag kind, and a positional —
/// the fixture the help-page and usage-error tests install.
fn manifest_for(name: &str) -> String {
    format!(
        r#"{{"kind":"command","name":"{name}","version":"0.1.0","protocol":"2025-06-18","flags":[{{"kind":"string","name":"message","default":"hello"}},{{"kind":"integer","name":"count","default":3}},{{"kind":"boolean","name":"shout","default":false}},{{"kind":"repeatable","name":"tag","default":[]}}],"positional":{{"name":"DIR"}},"commit":"per-batch","executable":"run.sh"}}"#
    )
}

/// The `greet` fixture: the every-kind manifest with a no-op child (the
/// parse, help, and error tests spawn nothing).
fn install_greet(xdg: &std::path::Path) {
    let package = install_package(xdg, "greet-pkg", &manifest_for("greet"));
    install_sh_child(&package, "");
}

/// The zero-installed missing-name error, byte for byte.
#[test]
fn test_bare_run_names_the_zero_state_and_the_install_path() {
    let (_tmp, xdg) = xdg_root();
    let (status, stdout, stderr) = run_in_xdg(&["run"], &xdg);
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert_eq!(
        stderr,
        format!(
            "Error: run requires an action name (no actions installed; \
              relate extension install <name> adds one)\n\
              {RUN_USAGE}\n{RUN_HINT}\n"
        )
    );
}

/// ADR-0429 §1: the dynamic installed-actions section lives on the run
/// usage error — a bare `run` with an installed action lists it.
#[test]
fn test_bare_run_lists_the_installed_actions() {
    let (tmp, xdg) = xdg_root();
    install_greet(tmp.path());
    let (status, _, stderr) = run_in_xdg(&["run"], &xdg);
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("run requires an action name (installed: greet)"),
        "{stderr}"
    );
    assert!(stderr.contains(RUN_USAGE), "{stderr}");
    assert!(stderr.contains(RUN_HINT), "{stderr}");
}

/// ADR-0429 §7's pinned discovery text, byte for byte (LABS-552 §4): an
/// unresolvable name fails closed naming the first-party install path —
/// never a fetch (the parse refuses before anything runs).
#[test]
fn test_an_uninstalled_name_fails_closed_with_the_pinned_discovery_text() {
    let (_tmp, xdg) = xdg_root();
    let (status, stdout, stderr) = run_in_xdg(&["run", "bogus"], &xdg);
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert_eq!(
        stderr,
        format!(
            "Error: no installed action 'bogus' — first-party extension available: \
              relate extension install bogus\n\
              {RUN_USAGE}\n{RUN_HINT}\n"
        )
    );
}

/// A flag where the name belongs is the verb's own unexpected argument.
#[test]
fn test_a_flag_where_the_name_belongs_is_an_unexpected_argument() {
    let (_tmp, xdg) = xdg_root();
    let (status, _, stderr) = run_in_xdg(&["run", "--bogus"], &xdg);
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("Error: unexpected argument to run: --bogus"),
        "{stderr}"
    );
    assert!(stderr.contains(RUN_USAGE), "{stderr}");
}

/// The verb's own page: the fresh-install zero listing (the dynamic
/// installed-actions section, ADR-0429 §1), the shared store block, and
/// the discovery note.
#[test]
fn test_run_help_lists_the_zero_state() {
    let (_tmp, xdg) = xdg_root();
    let (status, stdout, stderr) = run_in_xdg(&["run", "--help"], &xdg);
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(stderr.is_empty(), "{stderr}");
    assert!(stdout.starts_with(RUN_USAGE), "{stdout}");
    assert!(
        stdout.contains("Installed actions:\n  none (relate extension install <name> adds one)"),
        "{stdout}"
    );
    assert!(stdout.contains("-d, --database PATH"), "{stdout}");
    assert_eq!(stdout.matches("Usage: relate").count(), 1, "{stdout}");
}

/// The resolved action's own page (`run <name> --help`): the host's
/// renderer over the manifest's flag table, reached through the verb's
/// pre-scan.
#[test]
fn test_run_name_help_prints_the_actions_own_page() {
    let (tmp, xdg) = xdg_root();
    install_greet(tmp.path());
    let (status, stdout, stderr) = run_in_xdg(&["run", "greet", "--help"], &xdg);
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(stderr.is_empty(), "{stderr}");
    assert!(
        stdout.starts_with(
            "Usage: relate run greet [DIR] [--message VALUE] [--count N] \
             [--shout] [--tag VALUE]"
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains("A string flag (default 'hello')"),
        "{stdout}"
    );
    assert_eq!(stdout.matches("Usage: relate").count(), 1, "{stdout}");
}

/// A help token with an unresolvable name wins and prints the verb's
/// page (the `help_or` rule: nothing after the word is validated).
#[test]
fn test_a_help_token_with_an_unresolvable_name_prints_the_verb_page() {
    let (_tmp, xdg) = xdg_root();
    let (status, stdout, stderr) = run_in_xdg(&["run", "bogus", "--help"], &xdg);
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(stderr.is_empty(), "{stderr}");
    assert!(stdout.starts_with(RUN_USAGE), "{stdout}");
}

/// The action-scope usage error, byte for byte: the refusal, the
/// did-you-mean drawn from the action's own manifest flag table
/// (manifest data, sanitized), the action's own synopsis, and the hint
/// naming the action — exit 2.
#[test]
fn test_an_action_flag_typo_suggests_from_the_actions_own_table() {
    let (tmp, xdg) = xdg_root();
    install_greet(tmp.path());
    let (status, stdout, stderr) = run_in_xdg(&["run", "greet", "--mesage", "hi"], &xdg);
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert_eq!(
        stderr,
        "Error: unexpected argument to run greet: --mesage\n\
         Did you mean '--message'?\n\
         Usage: relate run greet [DIR] [--message VALUE] [--count N] [--shout] [--tag VALUE]\n\
         Run 'relate run greet --help' for more information.\n"
    );
}

/// ADR-0383's removed store spelling keeps its no-hint rejection at the
/// action scope too.
#[test]
fn test_the_removed_db_spelling_is_rejected_without_a_hint() {
    let (tmp, xdg) = xdg_root();
    install_greet(tmp.path());
    let (status, _, stderr) = run_in_xdg(&["run", "greet", "--db", "./g"], &xdg);
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("Error: unexpected argument to run greet: --db"),
        "{stderr}"
    );
    assert!(!stderr.contains("Did you mean"), "{stderr}");
}

/// A broken install fails the verb closed, help included (the module's
/// one rule: the grammar `run` accepts is machine state, so a registry
/// load refusal — exit 2, the refusal text, the run synopsis and hint —
/// answers every invocation shape).
#[test]
fn a_broken_install_fails_the_verb_closed_help_included() {
    let (tmp, xdg) = xdg_root();
    install_package(tmp.path(), "broken-pkg", "{ nope");
    for args in [
        vec!["run", "greet"],
        vec!["run", "greet", "--help"],
        vec!["run", "--help"],
        vec!["run"],
    ] {
        let (status, stdout, stderr) = run_in_xdg(&args, &xdg);
        assert_eq!(status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stdout.is_empty(), "{args:?}: stdout: {stdout}");
        assert!(
            stderr.contains("Error: extension 'broken-pkg': invalid manifest:"),
            "{args:?}: {stderr}"
        );
        assert!(stderr.contains(RUN_USAGE), "{args:?}: {stderr}");
        assert!(stderr.contains(RUN_HINT), "{args:?}: {stderr}");
    }
}

// --- the mounted run (unix lanes: the shell fixture child, like the
// host and protocol suites' own) ---

/// The `greet` child: a clean session through the bash MCP kit — the
/// handshake, one `write_cypher` of two rows, and the summary with a
/// starter.
#[cfg(unix)]
const GREET_CHILD: &str = r#"mcp_call write_cypher '{"statement":"UNWIND $rows AS row CREATE (n:N {id: row.id})","parameters":{"rows":[{"id": 1}, {"id": 2}]}}' >/dev/null
mcp_call report_output '{"text":"imported 2 nodes","starters":["MATCH (n:N) RETURN count(n) AS n"]}' >/dev/null"#;

#[cfg(unix)]
#[test]
fn an_installed_action_runs_through_the_wire_into_a_store() {
    let (tmp, xdg) = xdg_root();
    let package = install_package(tmp.path(), "greet-pkg", &manifest_for("greet"));
    install_sh_child(&package, &mcp_child_body("greet", GREET_CHILD));
    let cwd = tempdir();
    let db = cwd.path().join("graph");
    let db_path = db.to_str().expect("the store path is UTF-8");

    // An explicit target: the run exits after the summary (ADR-0286).
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["run", "greet", "-d", db_path])
            .with_envs(&[("XDG_CONFIG_HOME", xdg.as_str())])
            .in_dir(cwd.path()),
    );
    assert_eq!(status.code(), Some(0), "{stderr}");
    // The summary line, then each starter indented two spaces.
    assert!(
        stdout.contains("imported 2 nodes\n  MATCH (n:N) RETURN count(n) AS n"),
        "the summary and its starter:\n{stdout}"
    );
    // The host wrote the child's rows: the store holds the batch's
    // nodes.
    let home = tempdir();
    assert_eq!(
        support::query_count(home.path(), &db, &[], "MATCH (n:N) RETURN count(n) AS n"),
        2
    );
}

// The `:memory:` default's non-TTY exit leg — exit after the summary,
// no store behind — is not re-spelled here: the scenario is pinned where
// its children live, the fixture-language pair
// (`e2e_extension_fixtures.rs`, both children) and the first-party
// importer (`e2e_fs_to_graph.rs`), each with the cwd asserted.

// --- telemetry (ADR-0429 §9: command `run`, action `extension`) ---

/// The COMMAND report says command `run`, action `extension` — never
/// the installed action's name (ADR-0079 anonymity): the user-chosen
/// word appears nowhere in the events.
#[cfg(all(unix, feature = "telemetry"))]
#[test]
fn test_run_reports_command_run_action_extension_and_never_the_name() {
    let mock = MockMixpanel::start();
    let (tmp, xdg) = xdg_root();
    let package = install_package(tmp.path(), "greet-pkg", &manifest_for("greet"));
    install_sh_child(&package, &mcp_child_body("greet", GREET_CHILD));
    let cwd = tempdir();
    let db = cwd.path().join("graph");
    let db_path = db.to_str().expect("the store path is UTF-8");
    // The COMMAND event spools at the greet run's exit (ADR-0545 §1);
    // short enabled follow-ups against the same config root deliver it
    // (ADR-0545 §2) — a plain query run, whose engine work gives its
    // drain thread the window, and which avoids re-running the extension
    // child for delivery.
    let events = mock.deliver_spooled(
        || {
            let (status, _, stderr) = run_cli(
                Opts::new(&["run", "greet", "-d", db_path])
                    .with_envs(&[("XDG_CONFIG_HOME", xdg.as_str())])
                    .in_dir(cwd.path())
                    .with_endpoint(&mock.url()),
            );
            assert_eq!(status.code(), Some(0), "{stderr}");
        },
        support::FollowupRun {
            args: &[support::DELIVERY_WORK_QUERY],
            envs: &[("XDG_CONFIG_HOME", xdg.as_str())],
            home: None,
            cwd: Some(cwd.path()),
        },
        |events| {
            events.iter().any(|e| {
                e["event"] == "RELATE-CLI_COMMAND"
                    && e["properties"]["command"] == "run"
                    && e["properties"]["action"] == "extension"
                    && e["properties"]["success"] == true
            })
        },
    );
    // The anonymity rule, walked not claimed: the user-chosen name (and
    // the store path) appear nowhere in any event's serialized body.
    // The delivery follow-ups' own STARTUP/QUERY events are memory-mode
    // oneshots, so they carry neither.
    assert!(!events.is_empty());
    for event in &events {
        let body = event.to_string();
        assert!(
            !body.contains("greet") && !body.contains(db_path),
            "a user-chosen name or store path leaked into telemetry: {body}"
        );
    }
}
