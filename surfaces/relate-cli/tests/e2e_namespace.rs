//! End-to-end tests for the `--namespace` session flag (ADR-0118).

#[allow(dead_code)]
mod support;

use support::{run_cli, CliRunOptions as Opts};

/// The `ontology` preview opt-in (ADR-0593): every run that selects or
/// lists `system_ontologies` carries it.
const GATE_ON: &[(&str, &str)] = &[("RELATE_PREVIEW", "ontology")];
/// The pinned ADR-0229 42N17 text.
const NOT_QUERYABLE: &str = "This database is not queryable. Use the db.ontology.* procedures on the user database 'neo4j' instead.";

#[test]
fn test_namespace_system_ontologies_rejects_queries_with_42n17() {
    // ADR-0229 §1/§2: selecting the namespace works, but every
    // non-administration statement routed to it — reads included — fails
    // with the pinned 42N17 text (a rejected statement: exit 6, the
    // `semantic error:` line on stderr).
    let (status, _stdout, stderr) = run_cli(
        Opts::new(&[
            "--namespace",
            "system_ontologies",
            "--mode",
            "table",
            "-c",
            "MATCH (n) RETURN count(n)",
        ])
        .with_envs(GATE_ON),
    );
    assert_eq!(status.code(), Some(6), "stderr: {stderr}");
    assert!(
        stderr
            .trim_end()
            .ends_with(&format!("semantic error: SemanticError: {NOT_QUERYABLE}")),
        "expected the 42N17 not-queryable text on stderr, got: {stderr}"
    );
}

/// The opted-in `SHOW DATABASES` run from a `--namespace` session: the
/// three fixed namespaces, whatever session namespace the run selects —
/// the one mechanics the two clause-pinning tests below share.
fn show_databases_lists_three_rows_from(namespace: &str) {
    let (status, stdout, stderr) = run_cli(
        Opts::new(&[
            "--namespace",
            namespace,
            "--mode",
            "table",
            "-c",
            "SHOW DATABASES",
        ])
        .with_envs(GATE_ON),
    );
    assert!(status.success(), "SHOW DATABASES failed: {stderr}");
    for name in ["neo4j", "system", "system_ontologies"] {
        assert!(
            stdout.contains(name),
            "expected a row for {name}, got: {stdout}"
        );
    }
}

#[test]
fn test_namespace_system_ontologies_still_runs_administration_commands() {
    // ADR-0116 §2's carve-out survives ADR-0229 (under the opt-in,
    // ADR-0593): SHOW DATABASES still runs while routed to
    // `system_ontologies`, and the run selects the namespace.
    show_databases_lists_three_rows_from("system_ontologies");
}

#[test]
fn test_namespace_system_show_databases_lists_three_rows() {
    show_databases_lists_three_rows_from("system");
}

#[test]
fn test_namespace_defaults_to_neo4j() {
    // No flag: the session namespace is neo4j, so writes land in the user
    // graph and SHOW DATABASES is not the default view.
    let (status, stdout, stderr) =
        run_cli(Opts::new(&["-c", "CREATE (n:Probe) RETURN count(n) AS c"]));
    assert!(status.success(), "default-namespace write failed: {stderr}");
    assert!(
        stdout.contains(r#"{"c":1}"#),
        "expected the write to succeed in neo4j, got: {stdout}"
    );
}

#[test]
fn test_namespace_applies_to_positional_query_and_piped_stdin() {
    // The flag applies in every invocation shape (positional query, piped
    // stdin), and the ADR-0229 gate rejects the read in each of them:
    // `system_ontologies` is not queryable, reads included.
    let (status, _stdout, stderr) = run_cli(
        Opts::new(&[
            "--namespace",
            "system_ontologies",
            "MATCH (n) RETURN count(n) AS c",
        ])
        .with_envs(GATE_ON),
    );
    assert_eq!(status.code(), Some(6), "stderr: {stderr}");
    assert!(
        stderr.trim_end().ends_with(NOT_QUERYABLE),
        "expected the 42N17 not-queryable text on stderr, got: {stderr}"
    );

    let home = tempfile::tempdir().expect("tempdir");
    let (status, _stdout, stderr) = run_cli(
        Opts::new(&["query", "--namespace", "system_ontologies"])
            .with_envs(GATE_ON)
            .in_home(home.path())
            .with_stdin(b"MATCH (n) RETURN count(n) AS c;"),
    );
    assert_eq!(status.code(), Some(6), "stderr: {stderr}");
    assert!(
        stderr.trim_end().ends_with(NOT_QUERYABLE),
        "expected the 42N17 not-queryable text on stderr, got: {stderr}"
    );
}

#[test]
fn test_namespace_names_are_case_insensitive() {
    let (status, stdout, stderr) =
        run_cli(Opts::new(&["--namespace", "SYSTEM", "-c", "SHOW DATABASES"]).with_envs(GATE_ON));
    assert!(status.success(), "uppercase SYSTEM failed: {stderr}");
    assert!(
        stdout.contains("system_ontologies"),
        "expected SHOW DATABASES output, got: {stdout}"
    );
}

#[test]
fn test_namespace_unknown_value_errors_with_usage_before_open() {
    let (status, _stdout, stderr) = run_cli(Opts::new(&["--namespace", "nosuch"]));
    assert_eq!(status.code(), Some(2), "invalid --namespace should exit 2");
    assert!(
        stderr.contains(
            "Error: invalid --namespace: unknown namespace 'nosuch' (expected neo4j, system or system_ontologies)"
        ),
        "expected the invalid-value error on stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("Usage: relate"),
        "expected the usage text after the error, got: {stderr}"
    );
}

#[test]
fn test_namespace_flag_before_command_word_is_validated() {
    // `pull` is not the first positional here, but the invalid flag value
    // must still fail in the main parse loop, before anything else runs.
    let (status, _stdout, stderr) = run_cli(Opts::new(&[
        "--namespace",
        "nosuch",
        "pull",
        "--query",
        "x",
        "./db",
    ]));
    assert_eq!(status.code(), Some(2), "invalid --namespace should exit 2");
    assert!(
        stderr.contains("Error: invalid --namespace: unknown namespace 'nosuch'"),
        "expected the invalid-value error on stderr, got: {stderr}"
    );
}

#[test]
fn test_namespace_missing_value_errors() {
    let (status, _stdout, stderr) = run_cli(Opts::new(&["--namespace"]));
    assert_eq!(
        status.code(),
        Some(2),
        "missing --namespace value should exit 2"
    );
    assert!(
        stderr.contains("requires a value"),
        "expected a 'requires a value' message on stderr, got: {stderr}"
    );
}

#[test]
fn test_help_lists_namespace_flag() {
    let (status, stdout, stderr) = run_cli(Opts::new(&["--help"]));
    assert!(status.success(), "--help failed: {stderr}");
    assert!(
        stdout.contains("--namespace NAME")
            && stdout.contains("neo4j|system|system_ontologies")
            && stdout.contains("default neo4j"),
        "expected --namespace with values and default in --help, got: {stdout}"
    );
}

#[test]
fn test_show_databases_without_the_opt_in_lists_the_two_ce_rows() {
    // ADR-0593: without the `ontology` opt-in the listing is CE's exact
    // two rows — `system_ontologies` is invisible.
    let (status, stdout, stderr) = run_cli(Opts::new(&["-c", "SHOW DATABASES"]));
    assert!(status.success(), "SHOW DATABASES failed: {stderr}");
    assert!(
        stdout.contains("neo4j") && stdout.contains("system"),
        "got: {stdout}"
    );
    assert!(
        !stdout.contains("system_ontologies"),
        "the hidden namespace must not be listed, got: {stdout}"
    );
}

#[test]
fn test_namespace_system_ontologies_without_the_opt_in_fails_as_unknown() {
    // ADR-0593: without the `ontology` opt-in the session connect refuses
    // the hidden namespace with the ordinary unknown-database rejection —
    // an engine error (exit 6), not the flag-parse usage error.
    let (status, _stdout, stderr) = run_cli(Opts::new(&[
        "--namespace",
        "system_ontologies",
        "-c",
        "SHOW DATABASES",
    ]));
    assert_eq!(status.code(), Some(6), "stderr: {stderr}");
    assert!(
        stderr
            .trim_end()
            .ends_with("Graph not found: system_ontologies"),
        "expected the unknown-database rejection on stderr, got: {stderr}"
    );
}
