//! End-to-end tests for the Relate CLI binary.
//!
//! Ported from the Go E2E suite (tests/e2e_test.go) so the Go implementation
//! can be removed without losing behavioral coverage. These tests execute the
//! actual `relate` binary to verify the full pipeline:
//! CLI args -> REPL -> Engine -> Storage -> Output.

#[allow(dead_code)]
mod support;

use support::{run_cli, CliRunOptions as Opts};

// --- Query mode (one-shot positional query argument) ---

/// ADR-0200: `relate query [STATEMENT] --database PATH`.
#[test]
fn test_query_command_with_database() {
    let dir = support::tempdir();
    let db = dir.path().join("my-db");
    let db_s = db.to_str().unwrap();
    for args in [
        vec!["query", "RETURN 2 AS n", "--database", db_s],
        vec!["query", "--database", db_s, "RETURN 2 AS n"],
    ] {
        let (status, stdout, stderr) = run_cli(Opts::new(&args));
        assert!(status.success(), "{args:?} failed: {stderr}");
        assert!(
            stdout.contains('2'),
            "{args:?}: expected 2 in stdout, got {stdout}"
        );
        assert!(db.is_dir(), "{args:?} should open the --database directory");
    }
}

#[test]
fn test_query_command_defaults_database_to_memory() {
    let (status, stdout, stderr) = run_cli(Opts::new(&["query", "RETURN 2 AS n"]));
    assert!(status.success(), "query failed: {stderr}");
    assert!(stdout.contains('2'), "stdout: {stdout}");
}

#[test]
fn test_short_database_flag_runs_the_statement() {
    let dir = support::tempdir();
    let db = dir.path().join("my-db");
    let db_s = db.to_str().unwrap();
    for args in [
        vec!["-d", db_s, "RETURN 1 AS n"],
        vec!["query", "-d", db_s, "RETURN 1 AS n"],
    ] {
        let (status, stdout, stderr) = run_cli(Opts::new(&args));
        assert!(status.success(), "{args:?} failed: {stderr}");
        assert!(stdout.contains('1'), "{args:?}: stdout {stdout}");
    }
    assert!(db.is_dir(), "-d should open the store");
}

#[test]
fn test_database_flag_without_query_verb_runs_the_statement() {
    let dir = support::tempdir();
    let db = dir.path().join("my-db");
    let db_s = db.to_str().unwrap();
    let (status, stdout, stderr) = run_cli(Opts::new(&["--database", db_s, "RETURN 1 AS n"]));
    assert!(status.success(), "stderr: {stderr}");
    assert!(stdout.contains('1'), "stdout: {stdout}");
    assert!(db.is_dir(), "--database should open the store");
}

#[test]
fn test_query_missing_database_value_prints_query_synopsis() {
    let (status, stdout, stderr) = run_cli(Opts::new(&["query", "--database"]));
    assert_eq!(status.code(), Some(2), "stderr: {stderr}");
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert!(
        stderr.contains("--database requires a path"),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains("Usage: relate query [STATEMENT] [--database PATH]"),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains("Run 'relate query --help' for more information."),
        "stderr: {stderr}"
    );
}

// --- Piped-output default mode (ADR-0107) ---

#[test]
fn test_piped_explicit_mode_table_wins() {
    let (status, stdout, stderr) = run_cli(Opts::new(&["-c", "RETURN 1 AS n", "--mode", "table"]));
    assert!(status.success(), "piped query failed: {stderr}");
    assert!(
        stdout.contains('+') && stdout.contains('|'),
        "explicit --mode table must render a table: {stdout}"
    );
}

#[test]
fn test_piped_explicit_csv_wins() {
    let (status, stdout, stderr) = run_cli(Opts::new(&["-c", "RETURN 1 AS n", "--csv"]));
    assert!(status.success(), "piped query failed: {stderr}");
    let trimmed = stdout.trim();
    assert!(
        trimmed.contains('n') && trimmed.contains('1') && !trimmed.contains('{'),
        "explicit --csv must render CSV, not JSON: {stdout}"
    );
}

/// With stdout attached to a real PTY the default is the human-facing table
/// (ADR-0107). Unix-only; the PTY driver needs `python3`.
#[cfg(unix)]
#[test]
fn test_tty_default_is_table() {
    let binary = support::binary_path_str();
    support::assert_pty_driver_ok("pty_one_shot_table.py", &[&binary]);
}

#[test]
fn test_regex_match_anchored_one_shot() {
    // `=~` is full-string anchored: 'abc' =~ 'a' is false, 'abc' =~ 'a.c' true.
    let (status, stdout, stderr) = run_cli(Opts::new(&[
        "-json",
        "RETURN 'abc' =~ 'a' AS anchored, 'abc' =~ 'a.c' AS whole;",
    ]));
    assert!(status.success(), "regex query failed: {stderr}");
    assert!(
        stdout.contains("\"anchored\""),
        "expected JSON with anchored, got: {stdout}"
    );
    assert!(
        stdout.contains("\"whole\"") && stdout.contains("true"),
        "expected whole=true, got: {stdout}"
    );
}

#[test]
fn test_regex_match_no_catastrophic_backtracking_one_shot() {
    // Previously hung for >10s; the regex crate is linear-time. Assert the
    // process completes promptly by wrapping the whole invocation in a timer.
    let start = std::time::Instant::now();
    let (status, stdout, stderr) = run_cli(Opts::new(&[
        "RETURN 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' =~ 'a*a*a*a*a*a*a*a*a*a*a*a*a*a*' AS r;",
    ]));
    assert!(status.success(), "regex DoS query failed: {stderr}");
    assert!(
        stdout.contains("true"),
        "expected true (input matches), got: {stdout}"
    );
    assert!(
        start.elapsed() < std::time::Duration::from_secs(10),
        "regex evaluation should be near-instant, took {:?}",
        start.elapsed()
    );
}

#[test]
fn test_single_query_json() {
    let (status, stdout, stderr) = run_cli(Opts::new(&["-json", "RETURN 42 AS answer;"]));
    assert!(status.success(), "json query failed: {stderr}");
    assert!(
        stdout.contains("\"answer\""),
        "expected JSON with 'answer' key, got: {stdout}"
    );
    assert!(stdout.contains("42"), "expected 42, got: {stdout}");
}

#[test]
fn test_single_query_csv() {
    let (status, stdout, stderr) = run_cli(Opts::new(&["-csv", "RETURN 'hello' AS greeting;"]));
    assert!(status.success(), "csv query failed: {stderr}");
    assert!(
        stdout.contains("greeting"),
        "expected CSV header, got: {stdout}"
    );
    assert!(stdout.contains("hello"), "expected hello, got: {stdout}");
}

#[test]
fn test_single_query_markdown() {
    let (status, stdout, stderr) = run_cli(Opts::new(&["-markdown", "RETURN 1 AS x;"]));
    assert!(status.success(), "markdown query failed: {stderr}");
    assert!(
        stdout.contains("| x"),
        "expected markdown table, got: {stdout}"
    );
    assert!(
        stdout.contains("---"),
        "expected markdown separator, got: {stdout}"
    );
}

#[test]
fn test_noheader_flag() {
    let (status, stdout, stderr) =
        run_cli(Opts::new(&["-csv", "-noheader", "RETURN 'test' AS val;"]));
    assert!(status.success(), "noheader query failed: {stderr}");
    // Filter out blank and stats lines (Rust stats lines read e.g. "Nodes created: 1";
    // a pure RETURN emits none, but stay defensive like the Go test).
    let data_lines: Vec<&str> = stdout
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.contains("created") && !l.contains("row(s)"))
        .collect();
    assert_eq!(
        data_lines.len(),
        1,
        "expected 1 data line (no header), got {data_lines:?}"
    );
    assert_eq!(data_lines[0].trim(), "test");
}

// --- Pipe mode ---

#[test]
fn test_pipe_mode() {
    let script = "CREATE (:Person {name: 'Alice'});\nMATCH (p:Person) RETURN p.name;";
    let (status, stdout, stderr) = run_cli(Opts::new(&["query"]).with_stdin(script.as_bytes()));
    assert!(status.success(), "pipe mode failed: {stderr}");
    assert!(
        stdout.contains("Alice"),
        "expected Alice in pipe output, got: {stdout}"
    );
}

#[test]
fn test_pipe_multiple_statements() {
    // Multiple ;-separated statements on a single piped line.
    let script =
        "CREATE (:A {x: 1}); CREATE (:A {x: 2}); CREATE (:A {x: 3}); MATCH (a:A) RETURN count(*);";
    let (status, stdout, stderr) = run_cli(Opts::new(&["query"]).with_stdin(script.as_bytes()));
    assert!(status.success(), "pipe multiple failed: {stderr}");
    assert!(stdout.contains("3"), "expected count 3, got: {stdout}");
}

#[test]
fn test_pipe_invalid_utf8_hard_fails() {
    // A read error on piped stdin (invalid UTF-8 here) must be reported to
    // stderr and fail the process, executing nothing — never a silent
    // partial run of mangled input. Isolated to a temp `$HOME` so telemetry
    // state does not touch the real config dir.
    let home = tempfile::tempdir().expect("tempdir");
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["query"])
            .in_home(home.path())
            .with_stdin(b"RETURN 1\xff"),
    );

    assert!(
        !status.success(),
        "invalid UTF-8 stdin must exit non-zero, stdout: {stdout} stderr: {stderr}"
    );
    assert_eq!(
        status.code(),
        Some(1),
        "invalid UTF-8 stdin should exit 1; stderr: {stderr}"
    );
    assert!(
        stderr.contains("could not read query input from stdin"),
        "expected a stdin read-error message on stderr, got: {stderr}"
    );
    assert!(
        stdout.trim().is_empty(),
        "no result or output must be printed for unreadable stdin, got: {stdout}"
    );
}

// --- `.timer` dot-command ---

/// Count of `Time: <total>ms (parse … / plan … / execute …)` per-statement
/// timing lines (ADR-0221 total, ADR-0491 phase split).
fn timing_lines(stdout: &str) -> usize {
    stdout
        .lines()
        .filter(|l| l.starts_with("Time: ") && l.ends_with(")"))
        .count()
}

#[test]
fn test_timer_on_prints_wall_time_per_statement() {
    let script = ".timer on\nRETURN 1 AS x;\nRETURN 2 AS y;\n";
    let (status, stdout, stderr) = run_cli(Opts::new(&["query"]).with_stdin(script.as_bytes()));
    assert!(status.success(), "timer on failed: {stderr}");
    assert!(
        stdout.contains("Timer: on"),
        "expected toggle confirmation, got: {stdout}"
    );
    assert_eq!(
        timing_lines(&stdout),
        2,
        "expected one Time: line per statement, got: {stdout}"
    );
}

#[test]
fn test_timer_off_by_default() {
    // SQLite-style default: timing is OFF unless `.timer on` is entered.
    let script = "RETURN 1 AS x;\n";
    let (status, stdout, stderr) = run_cli(Opts::new(&["query"]).with_stdin(script.as_bytes()));
    assert!(status.success(), "default timer run failed: {stderr}");
    assert_eq!(
        timing_lines(&stdout),
        0,
        "expected no Time: lines by default, got: {stdout}"
    );
}

#[test]
fn test_timer_off_stops_timing() {
    let script = ".timer on\nRETURN 1 AS x;\n.timer off\nRETURN 2 AS y;\n";
    let (status, stdout, stderr) = run_cli(Opts::new(&["query"]).with_stdin(script.as_bytes()));
    assert!(status.success(), "timer off failed: {stderr}");
    assert!(
        stdout.contains("Timer: off"),
        "expected toggle confirmation, got: {stdout}"
    );
    assert_eq!(
        timing_lines(&stdout),
        1,
        "only the statement between on/off should be timed, got: {stdout}"
    );
}

#[test]
fn test_timer_bad_argument_prints_usage() {
    let script = ".timer\n";
    let (status, stdout, stderr) = run_cli(Opts::new(&["query"]).with_stdin(script.as_bytes()));
    assert!(status.success(), "timer usage failed: {stderr}");
    assert!(
        stdout.contains("Usage: .timer on|off"),
        "expected usage line, got: {stdout}"
    );
}

// --- Persistent database ---

#[test]
fn test_persistent_db() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("test_db");
    let db = db_path.to_str().unwrap();

    // Create data in one invocation
    let (status, stdout, stderr) = run_cli(Opts::new(&[
        "--database",
        db,
        "CREATE (:Person {name: 'Alice'});",
    ]));
    assert!(status.success(), "create failed: {stdout} {stderr}");

    // Query in a fresh process
    let (status, stdout, stderr) = run_cli(Opts::new(&[
        "--database",
        db,
        "MATCH (p:Person) RETURN p.name;",
    ]));
    assert!(status.success(), "query failed: {stderr}");
    assert!(
        stdout.contains("Alice"),
        "expected Alice from persistent DB, got: {stdout}"
    );
}

// --- Constraint persistence across reopen ---

#[test]
fn test_unique_constraint_persists_and_enforces_after_reopen() {
    // Regression test for the JsonStore load-order bug: a unique/key
    // constraint's same-named backing index used to be loaded *before* the
    // constraint itself, so `MemoryStore::create_constraint` saw the name
    // already taken by an index and silently dropped the constraint on
    // reopen. Verifies the full CLI open path (`relate_storage::open`)
    // re-establishes both the constraint metadata and its enforcement.
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("constraints_db");
    let db = db_path.to_str().unwrap();

    // Process 1: create the constraint, then exit (persisting to disk via
    // Drop/flush).
    let (status, _stdout, stderr) = run_cli(Opts::new(&[
        "--database",
        db,
        "CREATE CONSTRAINT uniq_email FOR (u:User) REQUIRE u.email IS UNIQUE;",
    ]));
    assert!(status.success(), "constraint setup run failed: {stderr}");

    // Process 1b: a second, separate process inserts the one row.
    let (status, _stdout, stderr) = run_cli(Opts::new(&[
        "--database",
        db,
        "CREATE (:User {email: 'a@example.com'});",
    ]));
    assert!(status.success(), "row setup run failed: {stderr}");

    // Process 2: a brand-new process reopening the same file must still see
    // the constraint via SHOW CONSTRAINTS... (explicit YIELD/RETURN, not the
    // bare form, to sidestep an unrelated bare-SHOW display gap).
    let (status, stdout, stderr) = run_cli(Opts::new(&[
        "--database",
        db,
        "SHOW CONSTRAINTS YIELD name RETURN name;",
    ]));
    assert!(status.success(), "show constraints failed: {stderr}");
    assert!(
        stdout.contains("uniq_email"),
        "expected uniq_email constraint to survive reopen, got: {stdout}"
    );

    // ...and must still reject a duplicate insert.
    let (_status, stdout, stderr) = run_cli(Opts::new(&[
        "--database",
        db,
        "CREATE (:User {email: 'a@example.com'});",
    ]));
    assert!(
        stderr.contains("Error") || stderr.to_lowercase().contains("constraint"),
        "expected duplicate insert to be rejected after reopen, got stdout: {stdout} stderr: {stderr}"
    );

    // Process 3: the duplicate must not have been persisted.
    let (status, stdout, stderr) = run_cli(Opts::new(&[
        "-csv",
        "--database",
        db,
        "MATCH (u:User) RETURN count(*) AS c;",
    ]));
    assert!(status.success(), "count query failed: {stderr}");
    assert!(
        stdout.lines().any(|l| l.trim() == "1"),
        "expected exactly 1 User node after rejected duplicate, got: {stdout}"
    );
}

// --- Current graph type persistence across reopen ---

#[test]
fn test_current_graph_type_persists_and_renders_after_reopen() {
    // Regression test: `ALTER CURRENT GRAPH TYPE SET { ... }` used to leave
    // the graph-type model only in the (in-process) engine's memory, so a
    // brand-new `relate` process reopening the same file rendered
    // `SHOW CURRENT GRAPH TYPE` empty even though the constraints it
    // derived were still enforced. Exercises the real CLI open path
    // (`relate_storage::open`), not just the engine directly.
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("graph_type_db");
    let db = db_path.to_str().unwrap();

    // Process 1: set the current graph type, then exit (flushing to disk).
    let (status, _stdout, stderr) = run_cli(Opts::new(&[
        "--database",
        db,
        "ALTER CURRENT GRAPH TYPE SET { (:Label => :A {dummy :: STRING}) };",
    ]));
    assert!(status.success(), "graph type setup run failed: {stderr}");

    // Process 2: a brand-new process reopening the same file must render
    // a non-empty specification reflecting the graph type set above.
    // (Explicit YIELD/RETURN: a bare `SHOW CURRENT GRAPH TYPE` is valid
    // Cypher but the CLI's default formatter only renders returned rows.)
    let show_query = "SHOW CURRENT GRAPH TYPE YIELD specification RETURN specification;";
    let (status, stdout_after_one_reopen, stderr) =
        run_cli(Opts::new(&["--database", db, show_query]));
    assert!(status.success(), "show current graph type failed: {stderr}");
    assert!(
        stdout_after_one_reopen.contains("dummy") && stdout_after_one_reopen.contains("STRING"),
        "expected the graph type to survive reopen, got: {stdout_after_one_reopen}"
    );

    // Process 3: yet another reopen must render the exact same canonical
    // body -- not a re-derived or empty one.
    let (status, stdout_after_two_reopens, stderr) =
        run_cli(Opts::new(&["--database", db, show_query]));
    assert!(status.success(), "show current graph type failed: {stderr}");
    // LABS-1120: the timing terminal line carries per-run numbers, so the
    // canonical-body comparison reads the data rows alone (the one typed
    // discriminator, `support::timing_terminal_line` — a filter, not a
    // take_while, so a multi-statement stream's later statements keep
    // their data rows).
    fn data_lines(stdout: &str) -> Vec<&str> {
        stdout
            .lines()
            .filter(|line| !support::timing_terminal_line(line))
            .collect()
    }
    assert_eq!(
        data_lines(&stdout_after_one_reopen),
        data_lines(&stdout_after_two_reopens),
        "SHOW CURRENT GRAPH TYPE must render the same canonical body across repeated reopens"
    );
}

// --- Dot-command in pipe mode ---

#[test]
fn test_pipe_comment_before_dot_command() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("comment_before_dot");
    let db_path = db.to_str().unwrap();

    let script =
        "// comment before the dot-command\n.begin\nCREATE (:Persisted {n: 1});\n.commit\n";
    let (status, _stdout, stderr) =
        run_cli(Opts::new(&["query", "--database", db_path]).with_stdin(script.as_bytes()));
    assert!(
        status.success(),
        "comment-before-dot-command run failed: {stderr}"
    );

    let (status, stdout, stderr) = run_cli(Opts::new(&[
        "-csv",
        "--database",
        db_path,
        "MATCH (n:Persisted) RETURN count(*) AS c;",
    ]));
    assert!(status.success(), "reopen failed: {stderr}");
    assert!(
        stdout.lines().any(|l| l.trim() == "1"),
        "the .begin/.commit transaction must persist its write, got: {stdout}"
    );
}

// --- Error handling ---

#[test]
fn test_query_error() {
    // DIVERGENCE from Go: the Go CLI exits non-zero for an invalid one-shot
    // query; the Rust CLI prints "Error: ..." to stderr but still exits 0.
    // We assert the error is reported on stderr (the Rust-equivalent
    // semantics) and deliberately do not assert the exit code, so this test
    // keeps passing if the Rust CLI later adopts Go's non-zero exit behavior.
    let (_status, stdout, stderr) = run_cli(Opts::new(&["INVALID QUERY;"]));
    assert!(
        stderr.to_lowercase().contains("error") || stderr.to_lowercase().contains("syntax"),
        "expected error on stderr, got stdout: {stdout} stderr: {stderr}"
    );
    assert!(
        !stdout.contains("INVALID QUERY executed"),
        "invalid query must not succeed"
    );
}

// --- Multiple creates and complex query ---

#[test]
fn test_complex_pipeline() {
    let script = "\
CREATE (:Person {name: 'Alice', age: 30});
CREATE (:Person {name: 'Bob', age: 25});
CREATE (:Person {name: 'Charlie', age: 35});
MATCH (p:Person) WHERE p.age >= 30 RETURN p.name ORDER BY p.name;
";
    let (status, stdout, stderr) =
        run_cli(Opts::new(&["query", "-csv"]).with_stdin(script.as_bytes()));
    assert!(status.success(), "complex pipeline failed: {stderr}");
    assert!(
        stdout.contains("Alice") && stdout.contains("Charlie"),
        "expected Alice and Charlie, got: {stdout}"
    );
    assert!(
        !stdout.contains("Bob"),
        "Bob should be filtered out, got: {stdout}"
    );
}

// --- Create and match relationships ---

#[test]
fn test_relationship_pipeline() {
    let script = "\
CREATE (a:Person {name: 'Alice'})-[:KNOWS]->(b:Person {name: 'Bob'});
MATCH (a)-[r:KNOWS]->(b) RETURN a.name, type(r), b.name;
";
    let (status, stdout, stderr) =
        run_cli(Opts::new(&["query", "-csv"]).with_stdin(script.as_bytes()));
    assert!(status.success(), "rel pipeline failed: {stderr}");
    assert!(
        stdout.contains("Alice") && stdout.contains("Bob") && stdout.contains("KNOWS"),
        "expected Alice-KNOWS->Bob, got: {stdout}"
    );
}

// --- Default in-memory ---

#[test]
fn test_default_in_memory() {
    let (status, stdout, stderr) = run_cli(Opts::new(&["RETURN 'hello' AS msg;"]));
    assert!(status.success(), "default in-memory failed: {stderr}");
    assert!(stdout.contains("hello"), "expected hello, got: {stdout}");
}

// --- JSON output format validation ---

#[test]
fn test_json_array_format() {
    let (status, stdout, stderr) = run_cli(Opts::new(&["-json", "RETURN 1 AS a, 2 AS b;"]));
    assert!(status.success(), "json format failed: {stderr}");
    // LABS-1120: a run with its result behind it wraps the rows as
    // `{"data": [...], "timings": {...}, "planCache": ...}` — the
    // PROFILE wrapper's shape — so the rows ride under `data`.
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("invalid JSON ({e}): {stdout}"));
    let arr = parsed["data"].as_array().expect("expected the data array");
    assert_eq!(arr.len(), 1, "expected 1 row, got: {stdout}");
    assert_eq!(arr[0]["a"], serde_json::json!(1));
    assert_eq!(arr[0]["b"], serde_json::json!(2));
    assert!(
        parsed["timings"]["parse_ms"].is_number(),
        "the timing rider: {stdout}"
    );
}

// --- Schema DDL result shape (LABS-184) ---

/// A schema DDL statement without RETURN is an omitted result (CIP-161
/// 00001): the CLI must print an empty JSON array, not one empty record.
#[test]
fn test_create_constraint_json_mode_prints_empty_array() {
    let statement = "CREATE CONSTRAINT c1 IF NOT EXISTS FOR (n:Drug) REQUIRE n.name IS UNIQUE;";
    let (status, stdout, stderr) =
        run_cli(Opts::new(&["query", "--mode", "json"]).with_stdin(statement.as_bytes()));
    assert!(status.success(), "constraint DDL failed: {stderr}");
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("invalid JSON ({e}): {stdout}"));
    // LABS-1120: the empty result rides under `data` beside the timing
    // rider — the omitted result is still the empty array.
    assert_eq!(
        parsed["data"],
        serde_json::json!([]),
        "schema DDL must print an empty JSON array, got: {stdout}"
    );
}

// --- Script file execution ---

#[test]
fn test_script_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script_path = dir.path().join("test.cypher");
    std::fs::write(
        &script_path,
        "CREATE (:Item {name: 'Widget'});\nMATCH (i:Item) RETURN i.name;\n",
    )
    .expect("write script");

    // Pipe the script file's contents into the CLI as stdin.
    let script = std::fs::read_to_string(&script_path).expect("read script");
    let (status, stdout, stderr) = run_cli(Opts::new(&["query"]).with_stdin(script.as_bytes()));
    assert!(status.success(), "script file run failed: {stderr}");
    assert!(stdout.contains("Widget"), "expected Widget, got: {stdout}");
}

// --- Batch exit codes + .bail ---

#[test]
fn test_pipe_bail_default_on_stops_and_exits_nonzero() {
    // In non-interactive mode `.bail` defaults on: a failing statement aborts
    // the run so later statements never execute, and the process exits 1 to
    // reflect the failure (corrupted dump replays used to exit 0).
    let script = "INVALID QUERY;\nRETURN 111 AS ok;";
    let (status, stdout, stderr) =
        run_cli(Opts::new(&["query", "-csv"]).with_stdin(script.as_bytes()));
    assert!(
        !status.success(),
        "a failed statement must fail the process, stderr: {stderr}"
    );
    assert_eq!(
        status.code(),
        Some(6),
        "a rejected statement in a batch exits 6 (ADR-0194)"
    );
    assert!(
        stderr.to_lowercase().contains("error") || stderr.to_lowercase().contains("syntax"),
        "expected an error on stderr, got: {stderr}"
    );
    assert!(
        !stdout.contains("111"),
        "bail on must stop before the later RETURN, got: {stdout}"
    );
}

#[test]
fn test_pipe_all_ok_exits_zero() {
    // Regression guard: a clean batch still exits 0.
    let script = "CREATE (:A);\nMATCH (n) RETURN count(n) AS c;";
    let (status, stdout, stderr) =
        run_cli(Opts::new(&["query", "-csv"]).with_stdin(script.as_bytes()));
    assert!(
        status.success(),
        "clean batch should exit 0, stderr: {stderr}"
    );
    assert!(stdout.contains('1'), "expected count 1, got: {stdout}");
}

// --- Statement splitting: comments ---

#[test]
fn test_pipe_semicolon_in_comment_not_split() {
    // A ';' inside a `/* */` block comment or a `//` line comment is not a
    // statement terminator, so this is a single valid statement that computes
    // 1 + 2 = 3.
    let script = "RETURN 1 + /* a ; b */ 2 AS n // trailing ; comment\n;";
    let (status, stdout, stderr) =
        run_cli(Opts::new(&["query", "-csv"]).with_stdin(script.as_bytes()));
    assert!(
        status.success(),
        "a ; inside a comment must not break splitting, stderr: {stderr}"
    );
    assert!(stdout.contains('3'), "expected 1+2=3, got: {stdout}");
}

// --- Canonical value conversion: temporal / point rendering ---
//
// The CLI routes result values through the canonical lossless converters
// (`relate::value_to_cypher` / `relate::cypher_to_value`), not a
// hand-rolled mapping with a string fallback. These tests pin the observable
// output for values that used to hit the fallback: a date printed with
// embedded quotes in table/JSON mode and doubly-quoted in cypher mode.

#[test]
fn test_temporal_date_table_mode_no_embedded_quotes() {
    let (status, stdout, stderr) = run_cli(Opts::new(&[
        "--mode",
        "table",
        "-c",
        "RETURN date('2024-01-15') AS d;",
    ]));
    assert!(status.success(), "table date query failed: {stderr}");
    assert!(
        stdout.contains("2024-01-15"),
        "expected 2024-01-15 in table output, got: {stdout}"
    );
    assert!(
        !stdout.contains("'2024-01-15'"),
        "table output must not embed quotes around the date, got: {stdout}"
    );
    // The data row renders the bare date between the cell borders.
    assert!(
        stdout.lines().any(|l| l.trim() == "| 2024-01-15 |"),
        "expected a bare 2024-01-15 table cell, got: {stdout}"
    );
}

#[test]
fn test_temporal_date_json_mode_no_embedded_quotes() {
    let (status, stdout, stderr) = run_cli(Opts::new(&[
        "-json",
        "-c",
        "RETURN date('2024-01-15') AS d;",
    ]));
    assert!(status.success(), "json date query failed: {stderr}");
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("invalid JSON ({e}): {stdout}"));
    let arr = parsed["data"].as_array().expect("expected the data array");
    assert_eq!(arr.len(), 1, "expected 1 row, got: {stdout}");
    // Exact string equality: any embedded quoting fails this assert.
    assert_eq!(
        arr[0]["d"],
        serde_json::json!("2024-01-15"),
        "JSON date must be the bare ISO string with no embedded quotes, got: {stdout}"
    );
}

#[test]
fn test_temporal_date_cypher_mode_roundtrip_literal() {
    let (status, stdout, stderr) = run_cli(Opts::new(&[
        "-mode",
        "cypher",
        "-c",
        "RETURN date('2024-01-15') AS d;",
    ]));
    assert!(status.success(), "cypher-mode date query failed: {stderr}");
    assert!(
        stdout.contains("date('2024-01-15')"),
        "expected a re-parseable date('2024-01-15') literal, got: {stdout}"
    );
    // Regression: the old fallback emitted the date as a string of a string,
    // which the cypher literal renderer then escaped to '\''2024-01-15'\''.
    assert!(
        !stdout.contains(r"'\''2024-01-15'\''"),
        "cypher mode must not doubly-quote the date, got: {stdout}"
    );
}

#[test]
fn test_point_cypher_mode_bare_literal() {
    let (status, stdout, stderr) = run_cli(Opts::new(&[
        "-mode",
        "cypher",
        "-c",
        "RETURN point({x: 1.5, y: 2.5}) AS p;",
    ]));
    assert!(status.success(), "cypher-mode point query failed: {stderr}");
    assert!(
        stdout.contains("point({srid: 7203, x: 1.5, y: 2.5})"),
        "expected a bare point literal, got: {stdout}"
    );
    // Regression: the old fallback stringified the point, so cypher mode
    // emitted it as a quoted string ('point({...})') instead of a literal.
    assert!(
        !stdout.contains("'point("),
        "point must render as a bare literal, not a quoted string, got: {stdout}"
    );
}
