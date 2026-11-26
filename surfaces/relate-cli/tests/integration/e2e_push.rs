//! End-to-end tests for `relate push` (ADR-0401): the spawned binary
//! against the shared loopback Query API fixtures, seeded local stores,
//! and the usage-error convention.

#![cfg(feature = "push")]

use crate::support;

use relate_test_support::loopback::{spawn_capture_server, spawn_scripted_responses};
use std::path::{Path, PathBuf};
use support::{run_cli, tempdir, CliRunOptions as Opts};

const EMPTY_BODY: &str = r#"{"data":{"fields":[],"values":[]},"errors":[]}"#;

fn write_env_file(home: &Path, port: u16) -> PathBuf {
    let env_path = home.join("n.env");
    std::fs::write(
        &env_path,
        format!("NEO4J_URI=http://127.0.0.1:{port}\nNEO4J_USERNAME=neo4j\nNEO4J_PASSWORD=pw\n"),
    )
    .unwrap();
    env_path
}

/// Seed the movies-shaped store the scenarios push: Person keyed by name,
/// Movie keyed by title, two ACTED_IN rels.
fn seed_movies(home: &Path, db: &Path) {
    let statements = [
        "CREATE CONSTRAINT m_title IF NOT EXISTS FOR (m:Movie) REQUIRE m.title IS UNIQUE",
        "CREATE CONSTRAINT p_name IF NOT EXISTS FOR (p:Person) REQUIRE p.name IS UNIQUE",
        "CREATE (:Person {name: 'Tom'})",
        "CREATE (:Person {name: 'Meg'})",
        "CREATE (:Movie {title: 'Sleepless'})",
        "CREATE (:Movie {title: 'Youve'})",
        "MATCH (p:Person {name: 'Tom'}), (m:Movie {title: 'Sleepless'}) CREATE (p)-[:ACTED_IN]->(m)",
        "MATCH (p:Person {name: 'Meg'}), (m:Movie {title: 'Youve'}) CREATE (p)-[:ACTED_IN]->(m)",
    ];
    for statement in statements {
        let (status, _, stderr) =
            run_cli(Opts::new(&["--database", db.to_str().unwrap(), statement]).in_home(home));
        assert!(status.success(), "seed {statement} failed: {stderr}");
    }
}

fn push_args<'a>(env: &'a Path, db: &'a Path, extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec![
        "push",
        "--to",
        env.to_str().unwrap(),
        "--database",
        db.to_str().unwrap(),
    ];
    args.extend(extra.iter().copied());
    args
}

#[test]
fn test_help_lists_push() {
    let home = tempdir();
    let (status, stdout, stderr) = run_cli(Opts::new(&["push", "--help"]).in_home(home.path()));
    assert!(status.success(), "push --help failed: {stderr}");
    assert!(
        stdout.starts_with("Usage: relate push --to CREDS"),
        "expected the push synopsis, got: {stdout}"
    );
    for flag in [
        "--labels",
        "--types",
        "--batch-size",
        "--nodes-update",
        "--rels-update",
        "--updated-at",
        "--rel-mode",
        "--merge-key",
        "--rel-key",
        "--unkeyed-labels",
        "--dry-run",
        "--json",
        "--allow-plaintext",
    ] {
        assert!(stdout.contains(flag), "{flag} missing from push --help");
    }
}

#[test]
fn test_push_requires_to_and_database() {
    let home = tempdir();
    let (status, _, stderr) =
        run_cli(Opts::new(&["push", "--database", "./graph"]).in_home(home.path()));
    assert_eq!(status.code(), Some(2));
    assert!(stderr.contains("--to"), "expected the --to error: {stderr}");
    let (status, _, stderr) = run_cli(Opts::new(&["push", "--to", "n.env"]).in_home(home.path()));
    assert_eq!(status.code(), Some(2));
    assert!(
        stderr.contains("--database"),
        "expected --database: {stderr}"
    );
}

#[test]
fn test_push_rejects_memory_and_multiple_selectors() {
    let home = tempdir();
    let (status, _, stderr) = run_cli(
        Opts::new(&["push", "--to", "n.env", "--database", ":memory:"]).in_home(home.path()),
    );
    assert_eq!(status.code(), Some(2));
    assert!(
        stderr.contains(":memory:"),
        "expected :memory: refusal: {stderr}"
    );
    let (status, _, stderr) = run_cli(
        Opts::new(&[
            "push",
            "--to",
            "n.env",
            "--database",
            "./g",
            "--labels",
            "Movie",
            "--types",
            "T",
        ])
        .in_home(home.path()),
    );
    assert_eq!(status.code(), Some(2));
    assert!(
        stderr.contains("one of --query"),
        "expected the selector error: {stderr}"
    );
}

#[test]
fn test_push_rejects_unknown_flag() {
    let home = tempdir();
    let (status, _, stderr) = run_cli(
        Opts::new(&["push", "--bogus", "--to", "n.env", "--database", "./g"]).in_home(home.path()),
    );
    assert_eq!(status.code(), Some(2));
    assert!(
        stderr.contains("unexpected argument to push: --bogus"),
        "expected the unknown-flag error: {stderr}"
    );
}

/// ADR-0118 §3's policy, mirrored from pull: push always works in the
/// `neo4j` namespace, so `--namespace` is rejected in both positions.
#[test]
fn test_push_rejects_namespace() {
    let home = tempdir();
    // Post-verb: the flag is simply unknown to the push grammar.
    let (status, _, stderr) = run_cli(
        Opts::new(&[
            "push",
            "--namespace",
            "system_ontologies",
            "--to",
            "n.env",
            "--database",
            "./g",
        ])
        .in_home(home.path()),
    );
    assert_eq!(status.code(), Some(2));
    assert!(
        stderr.contains("unexpected argument to push: --namespace"),
        "expected --namespace reported as unknown to push: {stderr}"
    );
    // Pre-verb: the main flag loop consumed it; push rejects it with its
    // own message, exactly as pull does (ADR-0118 §3).
    let (status, _, stderr) = run_cli(
        Opts::new(&[
            "--namespace",
            "system",
            "push",
            "--to",
            "n.env",
            "--database",
            "./g",
        ])
        .in_home(home.path()),
    );
    assert_eq!(status.code(), Some(2));
    assert!(
        stderr.contains(
            "Error: --namespace is not valid with push (push always reads the neo4j namespace)"
        ),
        "expected the pre-verb --namespace rejection: {stderr}"
    );
}

/// A dry run prints the plan and never touches the remote (ADR-0401 §6):
/// the capture server records zero requests.
#[test]
fn test_push_dry_run_touches_nothing() {
    let home = tempdir();
    let db = home.path().join("graph");
    seed_movies(home.path(), &db);
    let (server, port) = spawn_capture_server("200 OK", EMPTY_BODY);
    let env = write_env_file(home.path(), port);
    let (status, stdout, stderr) =
        run_cli(Opts::new(&push_args(&env, &db, &["--dry-run"])).in_home(home.path()));
    assert!(status.success(), "dry run failed: {stderr}");
    assert!(
        stdout.contains("Movie") && stdout.contains("Person"),
        "expected the plan entities, got: {stdout}"
    );
    assert!(
        stdout.contains("dry run: 3 entities, 6 rows planned; nothing was sent"),
        "expected the dry-run summary, got: {stdout}"
    );
    assert!(
        stdout.contains("MERGE (n:`Movie` {`title`: row.key.`title`})"),
        "expected the statement shape in the plan, got: {stdout}"
    );
    assert_eq!(
        server.request_count(),
        0,
        "a dry run must not send anything"
    );
}

/// The first push writes the scoped graph; the report table names the
/// entities and the summary the statements (ADR-0401 §6).
#[test]
fn test_push_creates_the_scoped_graph() {
    let home = tempdir();
    let db = home.path().join("graph");
    seed_movies(home.path(), &db);
    let (server, port) = spawn_capture_server("200 OK", EMPTY_BODY);
    let env = write_env_file(home.path(), port);
    let (status, stdout, stderr) =
        run_cli(Opts::new(&push_args(&env, &db, &[])).in_home(home.path()));
    assert!(status.success(), "push failed: {stderr}");
    assert!(
        stdout.contains("(Person)-[:ACTED_IN]->(Movie)"),
        "expected the rel group row, got: {stdout}"
    );
    assert!(
        stdout.contains("pushed 6 rows in 3 statements"),
        "expected the summary, got: {stdout}"
    );
    assert_eq!(
        server.request_count(),
        6,
        "one pre-read and one write per entity"
    );
    // Every write carries the typed parameter media type.
    for body in server.bodies() {
        assert!(body.contains("statement"), "{body:?}");
    }
    server.stop();
}

/// `--json` prints the machine form: one JSON object with per-entity
/// counters (ADR-0401 §6).
#[test]
fn test_push_json_report() {
    let home = tempdir();
    let db = home.path().join("graph");
    seed_movies(home.path(), &db);
    let (server, port) = spawn_capture_server("200 OK", EMPTY_BODY);
    let env = write_env_file(home.path(), port);
    let (status, stdout, stderr) =
        run_cli(Opts::new(&push_args(&env, &db, &["--json"])).in_home(home.path()));
    assert!(status.success(), "push --json failed: {stderr}");
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).expect("one JSON object");
    let entities = json["entities"].as_array().expect("entities array");
    assert_eq!(entities.len(), 3);
    let person = entities
        .iter()
        .find(|e| e["entity"] == "Person")
        .expect("Person entity");
    assert_eq!(person["created"], 2);
    assert_eq!(person["mergeKey"][0], "name");
    assert_eq!(json["rowsSent"], 6);
    assert_eq!(json["dryRun"], false);
    server.stop();
}

/// `--updated-at`: a strictly newer remote row is skipped and counted
/// `remote-newer` (ADR-0401 §4).
#[test]
fn test_push_updated_at_skips_remote_newer() {
    let home = tempdir();
    let db = home.path().join("graph");
    seed_movies(home.path(), &db);
    let (status, _, stderr) = run_cli(
        Opts::new(&[
            "--database",
            db.to_str().unwrap(),
            "MATCH (p:Person) SET p.updatedAt = date('2024-01-02')",
        ])
        .in_home(home.path()),
    );
    assert!(status.success(), "seed updatedAt failed: {stderr}");
    // The remote's rows are newer for both persons.
    let body = r#"{"data":{"fields":["k0","c0"],"values":[[{"$type":"String","_value":"Tom"},{"$type":"Date","_value":"2024-01-03"}],[{"$type":"String","_value":"Meg"},{"$type":"Date","_value":"2024-01-03"}]]},"errors":[]}"#;
    let (server, port) = spawn_capture_server("200 OK", body);
    let env = write_env_file(home.path(), port);
    let (status, stdout, stderr) = run_cli(
        Opts::new(&push_args(
            &env,
            &db,
            &["--labels", "Person", "--updated-at", "updatedAt", "--json"],
        ))
        .in_home(home.path()),
    );
    assert!(status.success(), "push failed: {stderr}");
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let person = &json["entities"][0];
    assert_eq!(person["skipped"]["remoteNewer"], 2);
    assert_eq!(person["updated"], 0);
    assert_eq!(json["rowsSent"], 0, "nothing to write");
    server.stop();
}

/// A relabeling of the same policy vocabulary: `--nodes-update skip`
/// counts existing entities as policy-skips instead of writing them.
#[test]
fn test_push_skip_policy_skips_existing() {
    let home = tempdir();
    let db = home.path().join("graph");
    seed_movies(home.path(), &db);
    let body = r#"{"data":{"fields":["k0"],"values":[[{"$type":"String","_value":"Tom"}],[{"$type":"String","_value":"Meg"}]]},"errors":[]}"#;
    let (server, port) = spawn_capture_server("200 OK", body);
    let env = write_env_file(home.path(), port);
    let (status, stdout, stderr) = run_cli(
        Opts::new(&push_args(
            &env,
            &db,
            &["--labels", "Person", "--nodes-update", "skip", "--json"],
        ))
        .in_home(home.path()),
    );
    assert!(status.success(), "push failed: {stderr}");
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(json["entities"][0]["skipped"]["policy"], 2);
    assert_eq!(json["rowsSent"], 0);
    server.stop();
}

/// A pre-read failure fails closed with the remote's own error (ADR-0401
/// §4): exit 1, nothing written.
#[test]
fn test_push_pre_read_failure_fails_closed() {
    let home = tempdir();
    let db = home.path().join("graph");
    seed_movies(home.path(), &db);
    let (handle, port) = spawn_scripted_responses(
        "500 Internal Server Error",
        vec![r#"{"errors":[{"code":"Neo.DatabaseError","message":"boom"}]}"#],
    );
    let env = write_env_file(home.path(), port);
    let (status, _, stderr) = run_cli(Opts::new(&push_args(&env, &db, &[])).in_home(home.path()));
    assert_eq!(status.code(), Some(1), "the push must fail: {stderr}");
    assert!(
        stderr.contains("pre-read of Movie failed"),
        "expected the pre-read failure, got: {stderr}"
    );
    assert!(
        stderr.contains("boom"),
        "the remote's error is the reason: {stderr}"
    );
    handle.join().expect("scripted server thread");
}
