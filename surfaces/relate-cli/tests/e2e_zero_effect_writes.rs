//! End-to-end tests for the ADR-0402 zero-effect write notes: the batch-only
//! stderr lines, byte-for-byte, with the unchanged exit codes and stdout —
//! and the store checked to hold exactly what the notes claim (nothing).
//!
//! The criterion is the engine's effect record, so the negative cases matter
//! as much as the positive one: a catalog change that leaves every mutation
//! counter at zero (`ALTER DATABASE`, the graph-type DDL, the ontology
//! procedures) applied something and stays silent, and so does a read-only
//! statement the planner conservatively classifies as a write (`CALL { … }`
//! with a read-only body, `LOAD CSV … RETURN`).

#[allow(dead_code)]
mod support;

use support::{
    node_count, run_cli, statement_note, tempdir, timing_terminal_line, CliRunOptions as Opts,
    COMMIT_NOTE,
};

/// Run one one-shot batch against `./db` in `dir` (JSONL output).
fn run_in(dir: &std::path::Path, statement: &str) -> (std::process::ExitStatus, String, String) {
    run_cli(Opts::new(&["--jsonl", "--database", "./db", "-c", statement]).in_dir(dir))
}

/// A statement's JSONL stream may close with its timing terminal line
/// (the `--timings` opt-in, ADR-0581), so the zero-effect contract is
/// "no data rows" — the timing lines are per-statement metadata, never
/// rows (the one typed discriminator, `support::timing_terminal_line`).
fn data_rows(stdout: &str) -> Vec<&str> {
    stdout
        .lines()
        .filter(|line| !timing_terminal_line(line))
        .collect()
}

#[test]
fn test_the_labs_313_block_reports_both_notes_and_commits_nothing() {
    // LABS-313's repro on a store where the `Label.id` uniqueness constraint
    // is in place and `id: 1` is taken: a zero-row MATCH short-circuits the
    // pipeline, so the violating CREATE never runs and the constraint never
    // fires.
    let dir = tempdir();
    for statement in [
        "CREATE CONSTRAINT label_id FOR (n:Label) REQUIRE n.id IS UNIQUE",
        "CREATE (:Label {id: 1})",
    ] {
        let (status, _, stderr) = run_in(dir.path(), statement);
        assert_eq!(status.code(), Some(0), "{statement}: {stderr}");
    }

    let (status, stdout, stderr) = run_in(
        dir.path(),
        ".begin; MATCH (m:NoSuchLabel) DELETE m CREATE (n:Label {id: 1}); .commit",
    );
    assert_eq!(status.code(), Some(0), "the no-op keeps its earned code");
    assert!(data_rows(&stdout).is_empty(), "stdout bytes are unchanged");
    assert_eq!(
        stderr,
        format!("{}{COMMIT_NOTE}", statement_note(Some((2, 3)))),
        "the statement note names the position, the summary states the commit"
    );
    assert_eq!(
        node_count(&dir.path().join("db")),
        1,
        "the report is true: the graph is unchanged"
    );

    // And the constraint is real: the same CREATE alone is rejected, so the
    // block's exit 0 above is the short-circuit, not an absent constraint.
    let (status, _, stderr) = run_in(dir.path(), "CREATE (:Label {id: 1})");
    assert_eq!(status.code(), Some(6), "{stderr}");
    assert!(
        stderr.contains("ConstraintValidationFailed"),
        "the uniqueness violation fires on its own statement: {stderr}"
    );
}

#[test]
fn test_a_one_shot_zero_effect_write_reports_one_unnumbered_note() {
    let dir = tempdir();
    let (status, stdout, stderr) = run_in(dir.path(), "MATCH (n:Ghost) SET n.seen = true");
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(data_rows(&stdout).is_empty(), "no data rows");
    assert_eq!(
        stderr,
        statement_note(None),
        "a single-statement batch is not numbered (ADR-0199's numbering)"
    );
}

#[test]
fn test_a_piped_stdin_zero_effect_write_reports_the_same_note() {
    let dir = tempdir();
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["query", "--jsonl", "--database", "./db"])
            .in_dir(dir.path())
            .with_stdin(b"MATCH (m:NoSuchLabel) DELETE m\n"),
    );
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(data_rows(&stdout).is_empty(), "no data rows");
    assert_eq!(
        stderr,
        statement_note(None),
        "piped stdin is the same batch half"
    );
}

#[test]
fn test_an_empty_or_read_only_transaction_says_nothing() {
    let dir = tempdir();
    let (status, stdout, stderr) = run_in(dir.path(), ".begin; .commit");
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(data_rows(&stdout).is_empty(), "no data rows");
    assert_eq!(stderr, "", "an empty .begin/.commit is silent");

    // A read inside the transaction does not make the commit news either.
    let (status, stdout, stderr) = run_in(
        dir.path(),
        ".begin; MATCH (n:NoSuchLabel) RETURN count(n); .commit",
    );
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert_eq!(
        data_rows(&stdout),
        ["{\"count(n)\":0}"],
        "the read's own row is unchanged"
    );
    assert_eq!(stderr, "", "committing a read-only transaction is silent");
}

#[test]
fn test_a_zero_row_read_and_an_effective_write_say_nothing() {
    let dir = tempdir();
    let (status, stdout, stderr) = run_in(
        dir.path(),
        "MATCH (m:NoSuchLabel) RETURN m; CREATE (:Effective); CREATE (n:Made) RETURN n",
    );
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert_eq!(
        data_rows(&stdout),
        ["{\"n\":{\"id\":1,\"kind\":\"node\",\"labels\":[\"Made\"],\"properties\":{}}}"],
        "the effective writes' stdout is unchanged"
    );
    assert_eq!(
        stderr, "",
        "a read and an effective write are not zero-effect writes"
    );
    assert_eq!(
        node_count(&dir.path().join("db")),
        2,
        "both effective writes landed"
    );
}

#[test]
fn test_a_mixed_transaction_reports_the_statement_only_without_a_summary() {
    let dir = tempdir();
    let (status, stdout, stderr) = run_in(
        dir.path(),
        ".begin; CREATE (:Kept); MATCH (m:Ghost) DELETE m; .commit",
    );
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(data_rows(&stdout).is_empty(), "no data rows");
    assert_eq!(
        stderr,
        statement_note(Some((3, 4))),
        "the statement line names the no-op; the merged counters are not zero, so no summary"
    );
    assert_eq!(
        node_count(&dir.path().join("db")),
        1,
        "the effective write in the same transaction persisted"
    );
}

/// A `PROFILE` run executes its statement, so its write is applied and must
/// silence the transaction summary — even though the plan table keeps that
/// statement's own note out of scope (ADR-0402 Decision 2). The no-op beside
/// it still reports, and the store holds what the silence claims.
#[test]
fn test_a_profile_write_silences_the_summary_without_its_own_note() {
    let dir = tempdir();
    let (status, stdout, stderr) = run_in(
        dir.path(),
        ".begin; PROFILE CREATE (:Kept); MATCH (m:Ghost) DELETE m; .commit",
    );
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(
        stdout.contains("\"plan\""),
        "the PROFILE statement prints its plan table, which is its own report: {stdout}"
    );
    assert_eq!(
        stderr,
        statement_note(Some((3, 4))),
        "the no-op names its position; the PROFILE write applied, so no summary"
    );
    assert_eq!(
        node_count(&dir.path().join("db")),
        1,
        "the PROFILE'd create is what silenced the summary, and the commit kept it"
    );
}

/// ADR-0402 Decision 4: `.begin` opens a fresh ledger, so an autocommit
/// zero-effect write before it keeps its own statement note but has no way
/// into the later transaction's summary.
#[test]
fn test_an_autocommit_zero_effect_write_never_reaches_a_later_summary() {
    let dir = tempdir();
    let (status, stdout, stderr) = run_in(dir.path(), "MATCH (m:Ghost) DELETE m; .begin; .commit");
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(data_rows(&stdout).is_empty(), "no data rows");
    assert_eq!(
        stderr,
        statement_note(Some((1, 3))),
        "the autocommit no-op reports, and the empty transaction's commit says nothing about it"
    );
    assert!(
        !stderr.contains(COMMIT_NOTE.trim_end()),
        "the ledger was reset by .begin, so no summary follows: {stderr}"
    );
}

/// A `PROFILE` zero-effect write inside a transaction is the mirror case of
/// the applied one above: its plan table replaces its own statement note
/// (ADR-0402 Decision 2), while the attempted write still folded into the
/// ledger, so the `.commit` summary reports.
#[test]
fn test_a_profile_zero_effect_write_reports_the_summary_without_its_own_note() {
    let dir = tempdir();
    let (status, stdout, stderr) = run_in(
        dir.path(),
        ".begin; PROFILE MATCH (m:Ghost) DELETE m; .commit",
    );
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(
        stdout.contains("\"plan\""),
        "the PROFILE statement prints its plan table, which is its own report: {stdout}"
    );
    assert_eq!(
        stderr, COMMIT_NOTE,
        "the plan table keeps the statement note out; the attempted no-op still reports at commit"
    );
    assert_eq!(
        node_count(&dir.path().join("db")),
        0,
        "the reported no-op is real: nothing was created"
    );
}

#[test]
fn test_a_rollback_prints_no_commit_summary() {
    let dir = tempdir();
    let (status, stdout, stderr) = run_in(
        dir.path(),
        ".begin; CREATE (:Dropped); MATCH (m:Ghost) DELETE m; .rollback",
    );
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(data_rows(&stdout).is_empty(), "no data rows");
    assert_eq!(
        stderr,
        statement_note(Some((3, 4))),
        "the discard was asked for: the per-statement note stays, no summary"
    );
    assert_eq!(
        node_count(&dir.path().join("db")),
        0,
        "the rollback discarded the transaction"
    );
}

#[test]
fn test_explain_and_profile_are_silent() {
    // EXPLAIN does not execute, and PROFILE's plan artifact replaces this
    // statement's note (ADR-0402 Decision 2): neither adds a note. PROFILE
    // still keeps its effect for the transaction summary — pinned separately.
    let dir = tempdir();
    for statement in [
        "EXPLAIN MATCH (m:Ghost) DELETE m",
        "PROFILE MATCH (m:Ghost) DELETE m",
    ] {
        let (status, stdout, stderr) = run_in(dir.path(), statement);
        assert_eq!(status.code(), Some(0), "{statement}: {stderr}");
        assert!(stdout.contains("\"plan\""), "{statement}: {stdout}");
        assert_eq!(stderr, "", "{statement} reports no zero-effect note");
    }
}

#[test]
fn test_output_modes_keep_their_stdout_bytes() {
    // The note is a stderr report in every mode; stdout stays the payload it
    // was (ADR-0402 Decision 5).
    let dir = tempdir();
    let (status, stdout, stderr) = run_cli(
        Opts::new(&[
            "--mode",
            "json",
            "--database",
            "./db",
            "-c",
            "MATCH (n:Ghost) SET n.x = 1",
        ])
        .in_dir(dir.path()),
    );
    assert_eq!(status.code(), Some(0), "{stderr}");
    // LABS-1120: the empty result rides under `data` beside the timing
    // rider — still no rows.
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        parsed["data"],
        serde_json::json!([]),
        "json's empty result is unchanged"
    );
    assert_eq!(stderr, statement_note(None));

    let (status, stdout, stderr) = run_cli(
        Opts::new(&[
            "--mode",
            "envelope",
            "--database",
            "./db",
            "-c",
            "MATCH (n:Ghost) SET n.x = 1",
        ])
        .in_dir(dir.path()),
    );
    assert_eq!(status.code(), Some(0), "{stderr}");
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        parsed["rows"],
        serde_json::json!([]),
        "the envelope is unchanged"
    );
    assert!(
        parsed["timings"]["elapsed_ms"].is_number(),
        "the timing block: {stdout}"
    );
    assert_eq!(stderr, statement_note(None));

    let (status, stdout, stderr) = run_in(dir.path(), "MATCH (n:Ghost) SET n.x = 1");
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(
        data_rows(&stdout).is_empty(),
        "JSONL prints nothing for a zero-row result"
    );
    assert_eq!(stderr, statement_note(None));
}

/// The ADR-0402 Decision 1 alternative rejected at a prompt: an exploration
/// session that writes nothing says nothing. Requires a real PTY; skip on
/// non-Unix or a bundle without the interactive shell.
#[cfg(all(unix, feature = "repl"))]
#[test]
fn test_the_interactive_shell_prints_no_zero_effect_notes() {
    let binary = support::binary_path_str();
    support::assert_pty_driver_ok("pty_zero_effect_repl.py", &[&binary]);
}

/// A spec-shape ontology document (the gate URI, one node entry), the fixture
/// the `db.ontology.set` cases below store.
const ONTOLOGY_DOC: &str = concat!(
    "{\"schema\":\"https://neo4j.com/docs/ontology-graph-spec/1.1.0/schema.json\",",
    "\"id\":\"twin\",\"version\":\"1\",\"name\":\"Twin\",",
    "\"nodes\":{\"Person\":{\"label\":\"Person\",\"properties\":{",
    "\"id\":{\"type\":\"STRING\",\"key\":true}}}},",
    "\"relationships\":{}}"
);

/// ADR-0402 Decision 2: a catalog change persists without touching one graph
/// mutation counter, so the counters cannot be the criterion. `ALTER DATABASE
/// SET DEFAULT LANGUAGE` is the default-build case, on the one admitted
/// CYPHER 25 spelling (every other value's rejection is the errors suite's
/// pin, in `e2e_errors.rs`): it must not be reported as "changed nothing",
/// and the change it reports must be real.
#[test]
fn test_alter_database_is_not_reported_as_changed_nothing() {
    let dir = tempdir();

    let (status, stdout, stderr) = run_in(
        dir.path(),
        "ALTER DATABASE neo4j SET DEFAULT LANGUAGE CYPHER 25",
    );
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(
        data_rows(&stdout).is_empty(),
        "the admin command returns no rows"
    );
    assert_eq!(
        stderr, "",
        "a persisted catalog change is not a zero-effect write"
    );

    // The change the silent run made is real: the default language is 25.
    let (status, stdout, stderr) = run_in(
        dir.path(),
        "SHOW DATABASE neo4j YIELD name, defaultLanguage",
    );
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert_eq!(
        data_rows(&stdout),
        ["{\"defaultLanguage\":\"CYPHER 25\",\"name\":\"neo4j\"}"],
        "ALTER DATABASE persisted the language"
    );

    // The same statement inside a transaction is a write the commit
    // persisted: no statement note, no commit summary.
    let (status, stdout, stderr) = run_in(
        dir.path(),
        ".begin; ALTER DATABASE neo4j SET DEFAULT LANGUAGE CYPHER 25; .commit",
    );
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(data_rows(&stdout).is_empty(), "no data rows");
    assert_eq!(
        stderr, "",
        "a catalog change is not a zero-effect transaction"
    );
}

/// The preview-gated catalog writes (`db.ontology.set`/`drop` and
/// `CREATE GRAPH TYPE`) leave no graph mutation counter either, and the
/// engine's effect record reports them as applied.
#[test]
fn test_preview_gated_catalog_writes_are_not_reported() {
    let dir = tempdir();
    let ontology = format!("CALL db.ontology.set('{ONTOLOGY_DOC}')");
    let graph_type = "CREATE GRAPH TYPE gt AS { (n:Person => {name :: STRING}) }";
    for statement in [ontology.as_str(), graph_type] {
        let (status, stdout, stderr) = run_cli(
            Opts::new(&["--jsonl", "--database", "./db", "-c", statement])
                .in_dir(dir.path())
                .with_envs(&[("RELATE_PREVIEW", "ontology,graph-type-implied-labels")]),
        );
        assert_eq!(status.code(), Some(0), "{statement}: {stderr}");
        assert!(
            data_rows(&stdout).is_empty(),
            "{statement} produces no rows"
        );
        assert_eq!(
            stderr, "",
            "{statement} persisted a catalog change, so it is not zero-effect"
        );
    }

    // The stored document the silent `set` wrote is really there.
    let get = "CALL db.ontology.get('json')";
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["--jsonl", "--database", "./db", "-c", get])
            .in_dir(dir.path())
            .with_envs(&[("RELATE_PREVIEW", "ontology")]),
    );
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(
        stdout.contains("Twin"),
        "the ontology set persisted: {stdout}"
    );

    // Dropping it is a second catalog change that must stay silent too.
    let (status, stdout, stderr) = run_cli(
        Opts::new(&[
            "--jsonl",
            "--database",
            "./db",
            "-c",
            "CALL db.ontology.drop()",
        ])
        .in_dir(dir.path())
        .with_envs(&[("RELATE_PREVIEW", "ontology")]),
    );
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(data_rows(&stdout).is_empty(), "no data rows");
    assert_eq!(stderr, "", "the drop persisted a catalog change");
}

/// ADR-0402 Decision 2: the plan-derived query type is conservative, so a
/// read-only `CALL { … }` body and `LOAD CSV` are write-routed while mutating
/// nothing. The engine's effect record says so: neither reports, and a
/// transaction holding only them prints no commit summary.
#[test]
fn test_read_only_statements_classified_as_writes_are_silent() {
    let dir = tempdir();
    for statement in [
        "CALL { MATCH (n) RETURN n } RETURN n",
        "CALL { MATCH (n) WITH n LIMIT 1 RETURN n } RETURN n",
    ] {
        let (status, stdout, stderr) = run_in(dir.path(), statement);
        assert_eq!(status.code(), Some(0), "{statement}: {stderr}");
        assert!(data_rows(&stdout).is_empty(), "{statement} returns no rows");
        assert_eq!(
            stderr, "",
            "{statement} is a read the planner classifies as a write"
        );
    }

    // LOAD CSV is on the planner's write list; the bare read form must be
    // silent too.
    let csv = tempdir();
    std::fs::write(csv.path().join("names.csv"), "Ada\n").unwrap();
    let csv_root = csv.path().to_str().unwrap();
    let (status, stdout, stderr) = run_cli(
        Opts::new(&[
            "--jsonl",
            "--database",
            "./db",
            "--load-csv-dir",
            csv_root,
            "-c",
            "LOAD CSV FROM 'names.csv' AS row RETURN row[0] AS name",
        ])
        .in_dir(dir.path()),
    );
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert_eq!(data_rows(&stdout), ["{\"name\":\"Ada\"}"]);
    assert_eq!(
        stderr, "",
        "LOAD CSV without a write clause applied nothing and attempted nothing"
    );

    // A transaction holding only such statements is not news at commit.
    let (status, stdout, stderr) = run_in(
        dir.path(),
        ".begin; CALL { MATCH (n) RETURN n } RETURN n; .commit",
    );
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(data_rows(&stdout).is_empty(), "no data rows");
    assert_eq!(
        stderr, "",
        "a read-only transaction is silent, however its statements were classified"
    );
}

/// The documented rerun nuance: an idempotent `SET` rewrites the value, so the
/// engine applied a change and stays silent; an idempotent `MERGE` that matches
/// and writes nothing applied nothing, so it reports.
#[test]
fn test_idempotent_merge_reports_and_idempotent_set_does_not() {
    let dir = tempdir();
    let (status, _, stderr) = run_in(dir.path(), "CREATE (:Person {id: 1})");
    assert_eq!(status.code(), Some(0), "{stderr}");

    // SET re-writes the property: one applied property write each time.
    for _ in 0..2 {
        let (status, stdout, stderr) = run_in(dir.path(), "MATCH (n:Person) SET n.id = 1");
        assert_eq!(status.code(), Some(0), "{stderr}");
        assert!(data_rows(&stdout).is_empty(), "no data rows");
        assert_eq!(
            stderr, "",
            "an idempotent SET re-applies the property write"
        );
    }

    // MERGE matches and writes nothing.
    let (status, _, stderr) = run_in(dir.path(), "MERGE (n:Person {id: 1})");
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert_eq!(
        stderr,
        statement_note(None),
        "a MERGE that matched and wrote nothing changed nothing"
    );
}
