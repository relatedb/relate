//! Cargo feature bundles (ADR-0104). Runs in every bundle (`full`, `agent`,
//! `minimal`): the core query paths always work, `--help` lists exactly the
//! compiled-in commands, and a compiled-out command word is still a command
//! that fails with the not-included error (exit 2) instead of opening a
//! database of that name.

mod support;

use support::{
    command_gates, commands_section, commands_section_words, compiled_commands, run_cli,
    snapshot_format_values, tempdir, CliRunOptions as Opts,
};

const COMMAND_WORDS: &[&str] = &[
    "query",
    "clone",
    "skill",
    "upgrade",
    "update",
    "telemetry",
    "pull",
    "push",
    "run",
    "info",
    "serve",
    "agent-context",
    "extension",
];

#[test]
fn test_piped_stdin_query_runs_in_every_bundle() {
    let home = tempdir();
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["query"])
            .in_home(home.path())
            .with_stdin(b"RETURN 1 AS n;"),
    );
    assert!(status.success(), "piped query failed: {stderr}");
    assert!(
        stdout.contains('n') && stdout.contains('1'),
        "got: {stdout}"
    );
}

#[test]
fn test_one_shot_query_runs_in_every_bundle() {
    let home = tempdir();
    let (status, stdout, stderr) = run_cli(Opts::new(&["RETURN 1 AS n"]).in_home(home.path()));
    assert!(status.success(), "one-shot query failed: {stderr}");
    assert!(
        stdout.contains('n') && stdout.contains('1'),
        "got: {stdout}"
    );
}

#[test]
fn test_help_lists_only_compiled_in_commands() {
    let home = tempdir();
    let (status, stdout, stderr) = run_cli(Opts::new(&["--help"]).in_home(home.path()));
    assert!(status.success(), "--help failed: {stderr}");
    assert!(stderr.is_empty(), "--help stderr: {stderr}");
    // The Commands section lists exactly the compiled-in words, in the
    // stable order, one line each.
    assert_eq!(
        commands_section_words(&stdout),
        compiled_commands(),
        "--help commands section:\n{stdout}"
    );
    // The upgrade line names its update alias.
    assert_eq!(
        commands_section(&stdout).contains("update"),
        cfg!(feature = "upgrade"),
        "--help upgrade line must name the update alias:\n{stdout}"
    );
    // Feature-gated query-run options follow the build too; the rest of the
    // top-level help contract is pinned in e2e_help.
    assert_eq!(
        stdout.contains("--fetch-params N=SRC"),
        cfg!(feature = "fetch-params")
    );
    assert_eq!(
        stdout.contains("readline-style editing"),
        cfg!(feature = "repl")
    );
}

/// Each command's block moved off the top-level help onto its own
/// `relate <COMMAND> --help` page; a compiled-out word keeps the
/// not-included error instead.
#[test]
fn test_command_help_pages_follow_the_build() {
    let home = tempdir();
    // The gate ↔ word mapping is shared (`command_gates`); this test owns
    // only each word's page needle. An unknown word panics, so a new
    // command fails loudly here instead of being silently skipped.
    let needle = |word: &str| -> &'static str {
        match word {
            "query" => "ADR-0200",
            "clone" => "--to-snapshot-format",
            "info" => "--json",
            "pull" => "--allow-plaintext",
            "push" => "--rel-key",
            "skill" => "otherwise refuse it",
            "upgrade" => "--yes",
            "telemetry" => "on|off|status",
            // The dynamic installed-actions section: present in the
            // zero-installed state too (ADR-0429 §1).
            "run" => "Installed actions",
            "serve" => "--auth USER:PASSWORD",
            "agent-context" => "cannot drift",
            "extension" => "Trust tiers",
            other => panic!("no help-page needle for new command {other:?}"),
        }
    };
    for (compiled_in, word) in command_gates() {
        let needle = needle(word);
        let (status, stdout, stderr) = run_cli(Opts::new(&[word, "--help"]).in_home(home.path()));
        assert_eq!(status.success(), compiled_in, "{word} --help: {stderr}");
        if compiled_in {
            assert!(stderr.is_empty(), "{word} --help stderr: {stderr}");
            assert!(
                stdout.starts_with(&format!("Usage: relate {word}")),
                "{word} --help must start with its synopsis:\n{stdout}"
            );
            assert!(
                stdout.contains(needle),
                "{word} --help must list {needle:?}:\n{stdout}"
            );
        } else {
            assert_eq!(status.code(), Some(2), "{word}: {stderr}");
            assert!(
                stderr.contains(&format!(
                    "Error: the '{word}' command is not included in this build of relate"
                )),
                "{word}: {stderr}"
            );
        }
    }
}

/// Codec expectations in these suites are keyed on this crate's own
/// `parquet` feature, so a codec leaking into a lean bundle fails here.
#[test]
fn test_parquet_codec_is_compiled_exactly_with_the_parquet_feature() {
    assert_eq!(
        relate::SnapshotCodecKind::Parquet.is_compiled(),
        cfg!(feature = "parquet")
    );
}

#[test]
fn test_help_lists_only_compiled_in_snapshot_codecs() {
    let home = tempdir();
    let (status, stdout, stderr) = run_cli(Opts::new(&["--help"]).in_home(home.path()));
    assert!(status.success(), "--help failed: {stderr}");
    // The codec set and its feature gating come from the one typed source
    // (`support::snapshot_format_values`, itself over `compiled()`):
    // `compiled()` already excludes Parquet in a build without the
    // feature, and its declaration order pins the listing order. A Parquet
    // build additionally lists the `parquet-wide` opt-in (ADR-0534) — an
    // option on Parquet, never a codec of its own.
    let fixed = format!("Snapshot codec ({})", snapshot_format_values());
    assert!(stdout.contains(&fixed), "missing {fixed:?}:\n{stdout}");
    assert_eq!(stdout.matches("--snapshot-format").count(), 1, "{stdout}");
}

/// Without the `fetch-params` feature, a `--fetch-params` value still
/// parses and then fails closed at resolution with the typed message
/// (ADR-0270) — never an unknown-flag usage error, and never a fetch.
#[cfg(not(feature = "fetch-params"))]
#[test]
fn test_fetch_params_fails_closed_without_the_fetch_params_feature() {
    let home = tempdir();
    let (status, _, stderr) = run_cli(
        Opts::new(&["--fetch-params", "v=/any/path.json", "RETURN 1"]).in_home(home.path()),
    );
    assert_eq!(status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains(
            r#"--fetch-params is not compiled into this build (enable the "fetch-params" feature)"#
        ),
        "stderr: {stderr}"
    );
    assert!(!stderr.contains("unknown flag"), "{stderr}");
}

/// Without the `remote-database` feature, a URL `--load` source still
/// parses and then fails closed at the fetch with the typed message
/// (ADR-0104) — never an unknown-value usage error, and never a network
/// attempt. The port is the discard service: a fetch would surface as a
/// transport error (exit 1 with `request failed`), so the not-compiled
/// message proves no fetch happened.
#[cfg(not(feature = "remote-database"))]
#[test]
fn test_remote_load_fails_closed_without_the_remote_database_feature() {
    let home = tempdir();
    let (status, _, stderr) = run_cli(
        Opts::new(&["--load", "http://127.0.0.1:9/store.rl", "-c", "RETURN 1"])
            .in_home(home.path()),
    );
    assert_eq!(status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains(
            r#"the remote bundle fetch is not compiled into this build (enable the "remote-database" feature)"#
        ),
        "stderr: {stderr}"
    );
    assert!(!stderr.contains("request failed"), "{stderr}");
}

/// The remote `--load` policy refusals precede the capability gate in
/// every build (LABS-872, ADR-0496; the grammar is LABS-903, ADR-0500):
/// safe mode refuses a remote source (ADR-0477) and live freshness is a
/// parse-time flag conflict (ADR-0480's freshness axis never applies to a
/// source), both as usage errors with the fetch compiled in or out. The
/// discard port means a fetch would fail as a transport error (exit 1) —
/// exit 2 proves the refusal happened before the network was touched.
#[test]
fn test_remote_load_policy_refusals_hold_in_every_bundle() {
    let home = tempdir();
    for (args, needle) in [
        (
            vec!["--safe", "--load", "http://127.0.0.1:9/store.rl"],
            "--safe refuses a remote --load source",
        ),
        (
            vec!["--read-only=live", "--load", "http://127.0.0.1:9/store.rl"],
            "--load cannot follow",
        ),
        (
            vec!["--load", "http://example.com/store.rl"],
            "plain http:// is only allowed for a loopback host",
        ),
    ] {
        let mut run_args = args.clone();
        run_args.extend(["-c", "RETURN 1"]);
        let (status, _, stderr) = run_cli(Opts::new(&run_args).in_home(home.path()));
        assert_eq!(status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stderr.contains(needle), "{args:?}: {stderr}");
        assert!(!stderr.contains("request failed"), "{args:?}: {stderr}");
    }
    // The URL-shape refusal is ungated too: a GitHub folder URL never
    // reaches a fetch in any build.
    let (status, _, stderr) = run_cli(
        Opts::new(&[
            "--load",
            "https://github.com/o/r/tree/main/stores",
            "-c",
            "RETURN 1",
        ])
        .in_home(home.path()),
    );
    assert_eq!(status.code(), Some(2), "stderr: {stderr}");
    assert!(stderr.contains("invalid --load"), "stderr: {stderr}");
}

/// Without the codec, an explicit Parquet request still parses and then
/// fails closed at open with the typed storage error (ADR-0105).
#[cfg(not(feature = "parquet"))]
#[test]
fn test_parquet_snapshot_format_fails_closed_without_parquet_feature() {
    let home = tempdir();
    let db = tempdir();
    let (status, stdout, stderr) = run_cli(
        Opts::new(&[
            "--snapshot-format",
            "parquet",
            "--database",
            db.path().to_str().unwrap(),
            "RETURN 1",
        ])
        .in_home(home.path()),
    );
    assert!(!status.success(), "stdout: {stdout}");
    assert!(
        stderr.contains(
            r#"parquet snapshot codec is not compiled into this build (enable the "parquet" feature)"#
        ),
        "stderr: {stderr}"
    );
    assert!(!stderr.contains("invalid --snapshot-format"), "{stderr}");
}

#[test]
fn test_command_words_are_database_paths_when_passed_to_database() {
    // ADR-0200: the store is `--database`, so a path that is also a command
    // word never has to hide behind `./` or `--`.
    for word in COMMAND_WORDS {
        let home = tempdir();
        let cwd = tempdir();
        let args = ["--database", word, "RETURN 1 AS n"];
        let (status, stdout, stderr) =
            run_cli(Opts::new(&args).in_home(home.path()).in_dir(cwd.path()));
        assert!(status.success(), "{word} failed: {stderr}");
        assert!(
            stdout.contains('1'),
            "{word}: expected output, got {stdout}"
        );
        assert!(
            cwd.path().join(word).is_dir(),
            "--database {word} should open a database directory named {word}"
        );
    }
}

/// Assert that `args` hits the compiled-out command path: exit 2, the
/// ADR-0104 message on stderr, nothing on stdout, and no database opened.
#[allow(dead_code)] // Unused in `full`, where every command is compiled in.
fn assert_not_included(args: &[&str], command: &str) {
    let home = tempdir();
    let cwd = tempdir();
    let (status, stdout, stderr) = run_cli(Opts::new(args).in_home(home.path()).in_dir(cwd.path()));
    assert_eq!(status.code(), Some(2), "{args:?}: stderr: {stderr}");
    assert!(
        stderr.contains("not included in this build"),
        "{args:?}: stderr: {stderr}"
    );
    assert!(
        stderr.contains(&format!(
            "Error: the '{command}' command is not included in this build of relate"
        )),
        "{args:?}: stderr: {stderr}"
    );
    assert!(stdout.is_empty(), "{args:?}: stdout: {stdout}");
    assert_eq!(
        std::fs::read_dir(cwd.path()).unwrap().count(),
        0,
        "{args:?} must not create a database"
    );
}

#[cfg(not(feature = "pull"))]
#[test]
fn test_pull_is_not_included_without_pull_feature() {
    assert_not_included(&["pull", "--query", "RETURN 1", "--database", "db"], "pull");
}

/// `push` is compiled out of a `minimal` build (ADR-0401 §8: the same
/// feature-bundle family as `pull`): the word is still a command, so it
/// fails with the not-included error instead of opening a database.
#[cfg(not(feature = "push"))]
#[test]
fn test_push_is_not_included_without_push_feature() {
    assert_not_included(&["push", "--to", "n.env", "--database", "db"], "push");
}

#[cfg(not(feature = "serve"))]
#[test]
fn test_serve_is_not_included_without_the_surface_features() {
    assert_not_included(&["serve", "--database", "db", "--http", "7474"], "serve");
}

#[cfg(not(feature = "skill"))]
#[test]
fn test_skill_is_not_included_without_skill_feature() {
    assert_not_included(&["skill", "install"], "skill");
}

#[cfg(not(feature = "upgrade"))]
#[test]
fn test_upgrade_is_not_included_without_upgrade_feature() {
    assert_not_included(&["upgrade", "--check"], "upgrade");
    assert_not_included(&["update", "--check"], "update");
}

#[cfg(not(feature = "telemetry"))]
#[test]
fn test_telemetry_is_not_included_without_telemetry_feature() {
    assert_not_included(&["telemetry", "status"], "telemetry");
}

/// `run` rides `extensions` at `full` + `agent` (ADR-0429 §8), never
/// `minimal`: the word is still a command, so it fails with the
/// not-included error instead of opening a database named `run`.
#[cfg(not(feature = "extensions"))]
#[test]
fn test_run_is_not_included_without_extensions_feature() {
    assert_not_included(&["run", "greet"], "run");
}

#[cfg(not(feature = "info"))]
#[test]
fn test_info_is_not_included_without_info_feature() {
    assert_not_included(&["info", "--database", "db"], "info");
}

#[cfg(not(feature = "agent-context"))]
#[test]
fn test_agent_context_is_not_included_without_agent_context_feature() {
    assert_not_included(&["agent-context"], "agent-context");
}

/// `extension` is compiled out of a `minimal` build (ADR-0431 §3: the
/// `extensions` feature rides `full` + `agent`, never `minimal`): the word
/// is still a command, so it fails with the not-included error instead of
/// opening a database.
#[cfg(not(feature = "extensions"))]
#[test]
fn test_extension_is_not_included_without_extensions_feature() {
    assert_not_included(&["extension", "list"], "extension");
}

/// With telemetry compiled out, a query run makes no request to the
/// configured endpoint, prints no first-run notice, and writes no config.
#[cfg(not(feature = "telemetry"))]
#[test]
fn test_no_telemetry_requests_without_telemetry_feature() {
    let (sink, port) = relate_test_support::loopback::spawn_capture_server("200 OK", "1");
    let endpoint = format!("http://127.0.0.1:{port}/track");
    let home = tempdir();
    let (status, _stdout, stderr) = support::run_cli(
        support::CliRunOptions::new(&["RETURN 1 AS n"])
            .in_home(home.path())
            .with_endpoint(&endpoint),
    );
    assert!(status.success(), "query failed: {stderr}");
    assert!(
        !stderr.contains("anonymous usage metrics"),
        "no first-run notice without telemetry: {stderr}"
    );
    // A telemetry-enabled binary joins its worker before exiting, so any
    // request it was going to make has already been served and recorded.
    assert_eq!(
        sink.request_count(),
        0,
        "the binary must not connect to the telemetry endpoint: {:?}",
        sink.bodies()
    );
    assert!(!home.path().join(".config/relate").exists());
}
