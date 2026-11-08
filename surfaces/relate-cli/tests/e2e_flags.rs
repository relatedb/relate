//! End-to-end tests for the Relate CLI's flag parsing: the `--help`
//! output channel, value validation, and the `--command`-vs-positional-query
//! mutual exclusion. Split from `e2e_cli.rs` to keep both files well under the
//! repo's 1k-line budget.

#[allow(dead_code)]
mod support;

use support::{run_cli, CliRunOptions as Opts};

// --- Flag parsing ---

#[test]
fn test_help_flag_long_exits_zero_to_stdout() {
    // `--help` is an explicit success: usage prints to STDOUT (not stderr) and
    // the process exits 0, so `relate --help | less` works and scripts can
    // tell it apart from an error.
    let (status, stdout, stderr) = run_cli(Opts::new(&["--help"]));
    assert!(
        status.success(),
        "--help should exit 0, got code {:?}, stderr: {}",
        status.code(),
        stderr
    );
    assert_eq!(status.code(), Some(0), "--help should exit with code 0");
    assert!(
        stdout.contains("Usage: relate"),
        "expected usage on stdout, got stdout: {stdout} stderr: {stderr}"
    );
    assert!(
        !stderr.contains("Usage:"),
        "usage must go to stdout, not stderr, for --help; stderr: {stderr}"
    );
    assert_eq!(
        stdout.contains("readline-style editing"),
        cfg!(feature = "repl"),
        "TTY editing note on --help iff the `repl` feature is on, got: {stdout}"
    );
}

#[cfg(feature = "skill")]
#[test]
fn test_help_force_describes_clobber_of_refused_destination() {
    // ADR-0262: `skill install` adopts a marker-less *recognizable* bundle
    // without `--force`, so the help text must no longer describe `--force` as
    // "touch a dir lacking the marker". It is the clobber for a destination
    // the command would otherwise refuse. The hand-written help is the source
    // of truth, so pin the wording here (the spec.md/cli-flags.md flag tables
    // carry the same sentence).
    let (status, stdout, stderr) = run_cli(Opts::new(&["skill", "--help"]));
    assert!(
        status.success(),
        "skill --help should exit 0, stderr: {stderr}"
    );
    assert!(
        stdout.contains("--force") && stdout.contains("otherwise refuse it"),
        "skill help must describe --force as clobbering a refused destination, got: {stdout}"
    );
}

#[test]
fn test_help_flag_short_exits_zero() {
    // `-h` is the short form of `--help`; previously it fell into the
    // unknown-flag catch-all and exited non-zero.
    let (status, stdout, _stderr) = run_cli(Opts::new(&["-h"]));
    assert!(status.success(), "-h should exit 0");
    assert_eq!(status.code(), Some(0));
    assert!(
        stdout.contains("Usage: relate"),
        "expected usage on stdout, got: {stdout}"
    );
}

#[test]
fn test_help_wins_over_later_unknown_flag() {
    // `-h` stops argument parsing the moment it is seen: a later unknown
    // flag is never validated and help still exits 0.
    let (status, stdout, _stderr) = run_cli(Opts::new(&["-h", "--bogus"]));
    assert!(status.success(), "-h --bogus should exit 0");
    assert_eq!(status.code(), Some(0));
    assert!(
        stdout.contains("Usage: relate"),
        "expected usage on stdout, got: {stdout}"
    );
}

#[test]
fn test_removed_follow_spelling_is_the_generic_unknown_flag_error() {
    // ADR-0480: `--follow` folded into `--read-only[=snapshot|live]` and is
    // removed with no alias and no migration hint (AGENTS.md §3) — the
    // top-level grammar answers it with the generic unknown-flag error.
    let (status, _stdout, stderr) = run_cli(Opts::new(&["--follow", "-c", "RETURN 1"]));
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("unknown flag") && stderr.contains("--follow"),
        "expected the generic unknown-flag error naming --follow, got: {stderr}"
    );
}

// The serve half needs the command compiled in: without it the word answers
// "not included in this build", which is a different (and correct) refusal.
#[cfg(feature = "serve")]
#[test]
fn test_removed_follow_spelling_is_the_generic_unknown_argument_error_on_serve() {
    let (status, _stdout, stderr) = run_cli(Opts::new(&["serve", "--follow"]));
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("unexpected argument to serve") && stderr.contains("--follow"),
        "expected the generic unexpected-argument error naming --follow, got: {stderr}"
    );
}

#[test]
fn test_unknown_flag_before_help_errors() {
    // An unknown flag seen BEFORE `-h` is a usage error: help never gets a
    // chance to win because parsing already failed.
    let (status, _stdout, stderr) = run_cli(Opts::new(&["--bogus", "-h"]));
    assert!(!status.success(), "--bogus -h must exit non-zero");
    assert_eq!(status.code(), Some(2));
    assert!(
        stderr.contains("unknown flag") && stderr.contains("Usage: relate"),
        "expected an unknown-flag error and usage on stderr, got: {stderr}"
    );
}

#[test]
fn test_invalid_mode_value_errors() {
    // A bogus `--mode` value used to silently fall back to table output; it is
    // now an explicit usage error (exit 2, message on stderr).
    let (status, _stdout, stderr) = run_cli(Opts::new(&["--mode", "bogus", "RETURN 1;"]));
    assert!(
        !status.success(),
        "invalid --mode value must exit non-zero, got code {:?}",
        status.code()
    );
    assert_eq!(status.code(), Some(2), "invalid --mode should exit code 2");
    assert!(
        stderr.to_lowercase().contains("invalid mode"),
        "expected an 'invalid mode' message on stderr, got: {stderr}"
    );
}

#[test]
fn test_missing_mode_value_errors() {
    // `--mode` with no following value used to silently do nothing; it is now
    // an explicit usage error.
    let (status, _stdout, stderr) = run_cli(Opts::new(&["--mode"]));
    assert!(
        !status.success(),
        "missing --mode value must exit non-zero, got code {:?}",
        status.code()
    );
    assert_eq!(
        status.code(),
        Some(2),
        "missing --mode value should exit code 2"
    );
    assert!(
        stderr.contains("requires a value"),
        "expected a 'requires a value' message on stderr, got: {stderr}"
    );
}

#[test]
fn test_valid_mode_flag_still_works() {
    // Regression guard: a valid `--mode json` still selects JSON output.
    let (status, stdout, stderr) = run_cli(Opts::new(&["--mode", "json", "RETURN 42 AS answer;"]));
    assert!(status.success(), "--mode json failed: {stderr}");
    assert!(
        stdout.contains("\"answer\""),
        "expected JSON output for --mode json, got: {stdout}"
    );
}

#[test]
fn test_format_alias_and_toon_mode() {
    // ADR-0239 (LABS-232): `--format` is the documented alias of `--mode`;
    // `toon` is an output mode with a `--toon` shortcut.
    let (status, stdout, stderr) =
        run_cli(Opts::new(&["--format", "json", "RETURN 42 AS answer;"]));
    assert!(status.success(), "--format json failed: {stderr}");
    assert!(
        stdout.contains("\"answer\""),
        "expected JSON output for --format json, got: {stdout}"
    );

    let (status, stdout, stderr) = run_cli(Opts::new(&["--toon", "RETURN 42 AS answer;"]));
    assert!(status.success(), "--toon failed: {stderr}");
    // LABS-1120: the terminal timings/planCache entries follow the rows.
    assert!(
        stdout.starts_with("rows[1]{answer}:\n  42\n"),
        "the rows block leads the toon document: {stdout}"
    );
    assert!(
        stdout.contains("\nplanCache:\n"),
        "the outcome entry: {stdout}"
    );

    let (status, stdout, stderr) = run_cli(Opts::new(&[
        "--format",
        "toon",
        "RETURN 1 AS a, 'x,y' AS b;",
    ]));
    assert!(status.success(), "--format toon failed: {stderr}");
    assert!(
        stdout.starts_with("rows[1]{a,b}:\n  1,\"x,y\"\n"),
        "the rows block leads the toon document: {stdout}"
    );
    assert!(
        stdout.contains("\nplanCache:\n"),
        "the outcome entry: {stdout}"
    );
}

#[test]
fn test_piped_json_is_compact_and_pretty_restores_indentation() {
    // ADR-0240 (LABS-233): with stdout piped (never a TTY under the test
    // harness), `--json` prints one compact line; `--pretty` opts back into
    // the indented form.
    let (status, stdout, stderr) = run_cli(Opts::new(&["--json", "RETURN 42 AS answer;"]));
    assert!(status.success(), "--json failed: {stderr}");
    // LABS-1120: the rows ride under `data` beside the timing rider, one
    // compact line (ADR-0240 unchanged).
    assert_eq!(stdout.lines().count(), 1, "one compact line: {stdout}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(v["data"], serde_json::json!([{"answer": 42}]));
    assert!(
        v["timings"]["parse_ms"].is_number(),
        "the timing rider: {stdout}"
    );

    let (status, stdout, stderr) =
        run_cli(Opts::new(&["--json", "--pretty", "RETURN 42 AS answer;"]));
    assert!(status.success(), "--json --pretty failed: {stderr}");
    assert!(
        stdout.lines().count() > 1 && stdout.contains("\n    {\n"),
        "expected indented JSON with --pretty, got: {stdout}"
    );
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(parsed["data"][0]["answer"], 42);
}

#[test]
fn test_output_spelling_suggests_format() {
    // ADR-0239 (LABS-232): `--output` is still an error, but the
    // did-you-mean names the real flag.
    let (status, _stdout, stderr) = run_cli(Opts::new(&["--output", "json", "RETURN 1;"]));
    assert_eq!(status.code(), Some(2), "--output should exit 2");
    assert!(
        stderr.contains("unknown flag") && stderr.contains("Did you mean '--format'?"),
        "expected a --format suggestion, got: {stderr}"
    );
}

#[test]
fn test_skill_bundle_advertises_only_real_jsonl_spellings() {
    // SKILL.md once advertised a `-jsonl` shortcut the CLI never implemented
    // (only `--mode jsonl` existed). The CLI now ships `-jsonl`/`--jsonl`
    // (LABS-107), so both spellings are real; guard against the bundle
    // documenting a jsonl spelling the CLI does not accept.
    let skill = include_str!("../src/skill/bundle/SKILL.md");
    assert!(
        skill.contains("-jsonl") || skill.contains("--mode jsonl"),
        "SKILL.md should document a real jsonl spelling:\n{skill}"
    );
}

#[test]
fn test_unknown_flag_errors_with_usage() {
    let (status, _stdout, stderr) = run_cli(Opts::new(&["--totally-bogus"]));
    assert!(!status.success(), "unknown flag must exit non-zero");
    assert_eq!(status.code(), Some(2), "unknown flag should exit code 2");
    assert!(
        stderr.contains("unknown flag") && stderr.contains("Usage: relate"),
        "expected an unknown-flag error and usage on stderr, got: {stderr}"
    );
}

#[test]
fn test_database_flag_accepts_a_dashed_path() {
    // `--database` takes the next token as a path, including one that starts
    // with `-`. Run in a temp dir so the store lands there, not in the repo.
    let home = tempfile::tempdir().expect("tempdir");
    let dir = tempfile::tempdir().expect("tempdir");
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["--database", "-dashed_db", "RETURN 5 AS n;"])
            .in_home(home.path())
            .in_dir(dir.path()),
    );
    assert!(
        status.success(),
        "dashed --database path should succeed, got code {:?}, stdout: {} stderr: {}",
        status.code(),
        stdout,
        stderr
    );
    assert!(stdout.contains("5"), "expected 5 in output, got: {stdout}");
    assert!(dir.path().join("-dashed_db").is_dir());
}

#[test]
fn test_piped_stdin_detected_non_interactive() {
    // `std::io::IsTerminal` must classify piped stdin as
    // non-interactive so the pipe path (read + execute all of stdin) runs
    // instead of the interactive REPL (which would block waiting for input).
    // A completing process with the piped result is the observable proof.
    let script = "CREATE (:Marker {id: 1});\nMATCH (m:Marker) RETURN m.id AS id;";
    let (status, stdout, stderr) = run_cli(Opts::new(&["query"]).with_stdin(script.as_bytes()));
    assert!(status.success(), "piped stdin run failed: {stderr}");
    assert!(
        stdout.contains('1'),
        "expected the piped statements to execute, got: {stdout}"
    );
}

#[test]
fn test_command_flag_one_shot() {
    let (status, stdout, stderr) = run_cli(Opts::new(&["-c", "RETURN 7 AS n"]));
    assert!(status.success(), "-c failed: {stderr}");
    assert!(stdout.contains("7"), "expected 7: {stdout}");
}

#[test]
fn test_command_flag_and_positional_query_are_mutually_exclusive() {
    // Both argument orders reject with the same error: a positional query
    // must never silently replace the --command value (or vice versa).
    for args in [
        ["-c", "RETURN 1", "RETURN 2"],
        ["RETURN 2", "-c", "RETURN 1"],
    ] {
        let (status, _stdout, stderr) = run_cli(Opts::new(&args));
        assert!(!status.success(), "{args:?} unexpectedly succeeded");
        assert!(
            stderr.contains("--command cannot be combined with a positional query"),
            "{args:?}: {stderr}"
        );
    }
}

#[test]
fn test_help_describes_read_only_and_follow_scope() {
    let (status, stdout, stderr) = run_cli(Opts::new(&["--help"]));
    assert!(status.success(), "--help should exit 0, stderr: {stderr}");
    assert!(
        stdout.contains("opens only while no writer holds it"),
        "help must say a .rl --read-only open needs no writer, got: {stdout}"
    );
    assert!(
        stdout.contains(":memory: rejects writes"),
        "help must say :memory: --read-only creates nothing, got: {stdout}"
    );
    assert!(
        stdout.contains("--read-only[=FRESH]") && stdout.contains("refused for .rl and :memory:"),
        "help must say --read-only=live is directory-only, got: {stdout}"
    );
}

#[test]
fn test_clone_help_describes_the_dir_rl_matrix() {
    let (status, stdout, stderr) = run_cli(Opts::new(&["clone", "--help"]));
    assert!(
        status.success(),
        "clone --help should exit 0, stderr: {stderr}"
    );
    assert!(
        stdout.contains("relate clone")
            && stdout.contains("not write")
            && stdout.contains("Refused for :memory:"),
        "clone help must describe clone across the dir/.rl matrix, got: {stdout}"
    );
}

#[test]
fn test_clone_rejects_a_query_and_an_access_flag() {
    for args in [
        vec!["clone", "src", "dst", "RETURN 1"],
        vec!["clone", "--read-only=live", "src", "dst"],
        vec!["-c", "RETURN 1", "clone", "src", "dst"],
        vec!["--read-only", "clone", "src", "dst"],
    ] {
        let (status, _stdout, stderr) = run_cli(Opts::new(&args));
        assert_eq!(status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stderr.contains("clone"), "{args:?}: {stderr}");
    }
}

#[test]
fn test_clone_copies_a_directory_and_exits() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    let src_s = src.to_str().unwrap();
    let dst_s = dst.to_str().unwrap();
    let (status, _stdout, stderr) = run_cli(Opts::new(&[
        "--database",
        src_s,
        "CREATE (:N {name: 'ada'})",
    ]));
    assert!(status.success(), "create failed: {stderr}");
    let (status, stdout, stderr) = run_cli(Opts::new(&["clone", src_s, dst_s]));
    assert!(status.success(), "clone failed: {stderr}");
    assert!(
        !stdout.contains("Relate"),
        "clone should exit instead of starting a shell, got: {stdout}"
    );
    let (status, stdout, stderr) = run_cli(Opts::new(&[
        "--database",
        dst_s,
        "MATCH (n:N) RETURN n.name AS name",
    ]));
    assert!(status.success(), "clone query failed: {stderr}");
    assert!(
        stdout.contains("ada"),
        "clone missed the source row: {stdout}"
    );
}

#[test]
fn test_bad_logical_graph_size_limit_env_exits_like_a_bad_flag() {
    let (flag_status, _flag_stdout, flag_stderr) = run_cli(Opts::new(&[
        "--logical-graph-size-limit",
        "1tb",
        "RETURN 1 AS n",
    ]));
    assert_eq!(flag_status.code(), Some(2), "{flag_stderr}");
    assert!(
        flag_stderr.contains("Error: invalid --logical-graph-size-limit:"),
        "{flag_stderr}"
    );
    assert!(
        flag_stderr.contains("invalid memory limit unit 'tb'"),
        "{flag_stderr}"
    );

    let (env_status, _env_stdout, env_stderr) = run_cli(
        Opts::new(&["RETURN 1 AS n"]).with_envs(&[("RELATE_LOGICAL_GRAPH_SIZE_LIMIT", "1tb")]),
    );
    assert_eq!(env_status.code(), Some(2), "{env_stderr}");
    assert!(
        env_stderr.contains("Error: invalid RELATE_LOGICAL_GRAPH_SIZE_LIMIT:"),
        "{env_stderr}"
    );
    assert!(
        env_stderr.contains("invalid memory limit unit 'tb'"),
        "{env_stderr}"
    );
    assert!(
        !env_stderr.contains("Usage:"),
        "a bad logical graph size limit is the flag error, not a usage dump: {env_stderr}"
    );

    // The flag is the value; a bad env is not read.
    let (override_status, _override_stdout, override_stderr) = run_cli(
        Opts::new(&["--logical-graph-size-limit", "0", "RETURN 1 AS n"])
            .with_envs(&[("RELATE_LOGICAL_GRAPH_SIZE_LIMIT", "1tb")]),
    );
    assert!(
        override_status.success(),
        "flag must win over a bad env: {override_stderr}"
    );

    let (help_status, help_stdout, help_stderr) =
        run_cli(Opts::new(&["--help"]).with_envs(&[("RELATE_LOGICAL_GRAPH_SIZE_LIMIT", "1tb")]));
    assert!(help_status.success(), "{help_stderr}");
    assert!(help_stdout.contains("Usage: relate"), "{help_stdout}");

    // Lifecycle commands exit before the env default is read.
    let (skill_status, _skill_stdout, skill_stderr) =
        run_cli(Opts::new(&["skill"]).with_envs(&[("RELATE_LOGICAL_GRAPH_SIZE_LIMIT", "1tb")]));
    assert_eq!(skill_status.code(), Some(2), "{skill_stderr}");
    assert!(
        !skill_stderr.contains("RELATE_LOGICAL_GRAPH_SIZE_LIMIT"),
        "a lifecycle command must not reject the logical-graph-size-limit env: {skill_stderr}"
    );
}

#[test]
fn test_workspace_limit_rejects_zero_and_a_bad_unit_like_the_graph_cap() {
    let (flag_status, _flag_stdout, flag_stderr) = run_cli(Opts::new(&[
        "--concurrent-logical-workspace-limit",
        "0",
        "RETURN 1 AS n",
    ]));
    assert_eq!(flag_status.code(), Some(2), "{flag_stderr}");
    assert!(
        flag_stderr
            .contains("Error: invalid --concurrent-logical-workspace-limit: must be positive"),
        "{flag_stderr}"
    );

    let (env_status, _env_stdout, env_stderr) = run_cli(
        Opts::new(&["RETURN 1 AS n"])
            .with_envs(&[("RELATE_CONCURRENT_LOGICAL_WORKSPACE_LIMIT", "1tb")]),
    );
    assert_eq!(env_status.code(), Some(2), "{env_stderr}");
    assert!(
        env_stderr.contains("Error: invalid RELATE_CONCURRENT_LOGICAL_WORKSPACE_LIMIT:"),
        "{env_stderr}"
    );
    assert!(
        !env_stderr.contains("Usage:"),
        "a bad workspace limit is the flag error, not a usage dump: {env_stderr}"
    );

    let (override_status, override_stdout, override_stderr) = run_cli(
        Opts::new(&[
            "--concurrent-logical-workspace-limit",
            "1mib",
            "RETURN 1 AS n",
        ])
        .with_envs(&[("RELATE_CONCURRENT_LOGICAL_WORKSPACE_LIMIT", "1tb")]),
    );
    assert!(
        override_status.success(),
        "flag must win over a bad env: {override_stderr}"
    );
    assert!(
        override_stdout.contains('1'),
        "the statement should still run: {override_stdout}"
    );

    let (skill_status, _skill_stdout, skill_stderr) = run_cli(
        Opts::new(&["skill"]).with_envs(&[("RELATE_CONCURRENT_LOGICAL_WORKSPACE_LIMIT", "1tb")]),
    );
    assert_eq!(skill_status.code(), Some(2), "{skill_stderr}");
    assert!(
        !skill_stderr.contains("RELATE_CONCURRENT_LOGICAL_WORKSPACE_LIMIT"),
        "a lifecycle command must not reject the workspace-limit env: {skill_stderr}"
    );
}

#[test]
fn test_max_traversal_depth_flag_and_env_reject_out_of_range_values() {
    // LABS-610: the flag is an integer in 1..=2000. 0 and 2001 exit 2 with
    // the named usage error, before any store work.
    for bad in ["0", "2001"] {
        let (status, _stdout, stderr) =
            run_cli(Opts::new(&["--max-traversal-depth", bad, "RETURN 1 AS n"]));
        assert_eq!(status.code(), Some(2), "{bad}: {stderr}");
        assert!(
            stderr.contains("Error: invalid --max-traversal-depth: must be between 1 and 2000"),
            "{bad}: {stderr}"
        );
        assert!(!stderr.contains("Usage:"), "{bad}: {stderr}");
    }

    // The environment variable is the default, with the same range.
    let (env_status, _env_stdout, env_stderr) =
        run_cli(Opts::new(&["RETURN 1 AS n"]).with_envs(&[("RELATE_MAX_TRAVERSAL_DEPTH", "0")]));
    assert_eq!(env_status.code(), Some(2), "{env_stderr}");
    assert!(
        env_stderr
            .contains("Error: invalid RELATE_MAX_TRAVERSAL_DEPTH: must be between 1 and 2000"),
        "{env_stderr}"
    );
    assert!(!env_stderr.contains("Usage:"), "{env_stderr}");

    let (parse_status, _parse_stdout, parse_stderr) =
        run_cli(Opts::new(&["RETURN 1 AS n"]).with_envs(&[("RELATE_MAX_TRAVERSAL_DEPTH", "1tb")]));
    assert_eq!(parse_status.code(), Some(2), "{parse_stderr}");
    assert!(
        parse_stderr.contains("Error: invalid RELATE_MAX_TRAVERSAL_DEPTH:"),
        "{parse_stderr}"
    );

    // The flag wins over a bad environment value.
    let (override_status, override_stdout, override_stderr) = run_cli(
        Opts::new(&["--max-traversal-depth", "2000", "RETURN 1 AS n"])
            .with_envs(&[("RELATE_MAX_TRAVERSAL_DEPTH", "1tb")]),
    );
    assert!(
        override_status.success(),
        "flag must win over a bad env: {override_stderr}"
    );
    assert!(override_stdout.contains('1'), "{override_stdout}");
}

#[test]
fn test_max_traversal_depth_admits_and_rejects_statements() {
    // The default (500) rejects a 1,000,000-hop request at admission with
    // the typed error, and the error names the knob.
    let (status, _stdout, stderr) = run_cli(Opts::new(&[
        "MATCH (a)-[*1..1000000]->(b) RETURN count(*) AS n",
    ]));
    assert_eq!(status.code(), Some(6), "{stderr}");
    assert!(stderr.contains("TraversalDepthExceeded"), "{stderr}");
    assert!(stderr.contains("paths up to 1,000,000 hops"), "{stderr}");
    assert!(stderr.contains("--max-traversal-depth"), "{stderr}");

    // Raising the limit admits the same shape at a bound under it.
    let (ok_status, ok_stdout, ok_stderr) = run_cli(Opts::new(&[
        "--max-traversal-depth",
        "2000",
        "MATCH (a)-[*1..1500]->(b) RETURN count(*) AS n",
    ]));
    assert!(ok_status.success(), "{ok_stderr}");
    assert!(ok_stdout.contains('0'), "{ok_stdout}");
}

// --- Safe mode (LABS-680, ADR-0477) ---

#[test]
fn test_safe_refuses_agent_shapes_and_legitimate_controls_pass() {
    // A refused shape is a syntax-class policy error: exit 6, the typed
    // kind, the remedy, and the explicit "not a database limit" wording.
    for (query, shape) in [
        ("MATCH (n) RETURN n", "unbounded read"),
        ("CREATE (n)", "without a label"),
    ] {
        let (status, _stdout, stderr) = run_cli(Opts::new(&["--safe", query]));
        assert_eq!(status.code(), Some(6), "{query}: {stderr}");
        assert!(stderr.contains("SafeModeViolation"), "{query}: {stderr}");
        assert!(stderr.contains(shape), "{query}: {stderr}");
        assert!(
            stderr.contains("This is a policy, not a database limit"),
            "{query}: {stderr}"
        );
        assert!(
            stderr.contains("reopen without --safe"),
            "{query}: {stderr}"
        );
    }

    // The named controls pass with safe mode on.
    for query in [
        "CREATE (n:Person)",
        "CREATE ()-[r:X]->()",
        "MATCH (n) RETURN n LIMIT 10",
        "MATCH (n) RETURN count(n) AS n",
        "RETURN 1 AS one",
        "MATCH (n) SET n.seen = true",
    ] {
        let (status, _stdout, stderr) = run_cli(Opts::new(&["--safe", query]));
        assert!(status.success(), "{query} must pass under --safe: {stderr}");
    }
}

#[test]
fn test_safe_mode_flag_env_ladder_and_bad_env() {
    // `RELATE_SAFE_MODE=1` turns it on...
    let (status, _stdout, stderr) =
        run_cli(Opts::new(&["MATCH (n) RETURN n"]).with_envs(&[("RELATE_SAFE_MODE", "1")]));
    assert_eq!(status.code(), Some(6), "{stderr}");
    assert!(stderr.contains("SafeModeViolation"), "{stderr}");

    // ...`0` leaves it off, and `--safe` overrides `0`.
    let (off_status, _off_stdout, off_stderr) =
        run_cli(Opts::new(&["MATCH (n) RETURN n"]).with_envs(&[("RELATE_SAFE_MODE", "0")]));
    assert!(off_status.success(), "{off_stderr}");
    let (flag_status, _flag_stdout, flag_stderr) = run_cli(
        Opts::new(&["--safe", "MATCH (n) RETURN n"]).with_envs(&[("RELATE_SAFE_MODE", "0")]),
    );
    assert_eq!(flag_status.code(), Some(6), "{flag_stderr}");

    // `--no-safe` overrides an enabled environment default.
    let (no_status, _no_stdout, no_stderr) = run_cli(
        Opts::new(&["--no-safe", "MATCH (n) RETURN n"]).with_envs(&[("RELATE_SAFE_MODE", "1")]),
    );
    assert!(no_status.success(), "{no_stderr}");

    // A bad environment value exits 2 naming the variable; the flag wins.
    let (bad_status, _bad_stdout, bad_stderr) =
        run_cli(Opts::new(&["RETURN 1 AS n"]).with_envs(&[("RELATE_SAFE_MODE", "2")]));
    assert_eq!(bad_status.code(), Some(2), "{bad_stderr}");
    assert!(
        bad_stderr.contains("Error: invalid RELATE_SAFE_MODE: '2' is not 1 or 0"),
        "{bad_stderr}"
    );
    assert!(!bad_stderr.contains("Usage:"), "{bad_stderr}");
    let (override_status, _override_stdout, override_stderr) =
        run_cli(Opts::new(&["--no-safe", "RETURN 1 AS n"]).with_envs(&[("RELATE_SAFE_MODE", "2")]));
    assert!(
        override_status.success(),
        "the flag must win over a bad env: {override_stderr}"
    );
}

// --- Usage-error synopsis shape (ADR-0518's renderer) ---

#[cfg(feature = "extensions")]
#[test]
fn test_extension_usage_error_keeps_the_multi_line_usage_block() {
    // The shared renderer prints the synopsis one line at a time so a
    // multi-line usage block (`extension`'s per-action synopsis) keeps
    // its shape — a whole-block sanitizer pass would flatten the
    // continuation lines into one.
    let (status, _, stderr) = run_cli(Opts::new(&["extension", "--bogus"]));
    assert_eq!(status.code(), Some(2), "stderr: {stderr}");
    assert!(
        stderr.contains("unexpected argument to extension"),
        "stderr: {stderr}"
    );
    assert!(
        stderr.lines().any(|l| l.starts_with("       ")),
        "usage block lost its continuation lines: {stderr}"
    );
}
