//! End-to-end tests for `relate pull` (ADR-0080).

#![cfg(feature = "pull")]

use crate::support;

use relate_test_support::loopback::{spawn_scripted_responses, spawn_single_response};
use std::path::{Path, PathBuf};
use support::{manifest_codec, run_cli, seed_database, tempdir, CliRunOptions as Opts};

const NODE_BODY: &str = r#"{"data":{"fields":["n"],"values":[[{"$type":"Node","_value":{"_element_id":"4:aaa:0","_labels":["Person"],"_properties":{"name":{"$type":"String","_value":"Ada"}}}}]]}}"#;

const REL_BODY: &str = r#"{"data":{"fields":["a","r","b"],"values":[[{"$type":"Node","_value":{"_element_id":"4:aaa:0","_labels":["Person"],"_properties":{"name":{"$type":"String","_value":"Ada"}}}},{"$type":"Relationship","_value":{"_element_id":"5:aaa:1","_start_node_element_id":"4:aaa:0","_end_node_element_id":"4:aaa:1","_type":"KNOWS","_properties":{"since":{"$type":"Integer","_value":"1"}}}},{"$type":"Node","_value":{"_element_id":"4:aaa:1","_labels":["Person"],"_properties":{"name":{"$type":"String","_value":"Bob"}}}}]]}}"#;

fn write_env_file(home: &Path, port: u16) -> PathBuf {
    let env_path = home.join("n.env");
    std::fs::write(
        &env_path,
        format!("NEO4J_URI=http://127.0.0.1:{port}\nNEO4J_USERNAME=neo4j\nNEO4J_PASSWORD=pw\n"),
    )
    .unwrap();
    env_path
}

/// Pull twice. The first pull must succeed; the second is the caller's to
/// judge (ADR-0158 refuses a non-empty destination).
fn pull_twice(
    home: &Path,
    env_path: &Path,
    db: &Path,
) -> (
    (std::process::ExitStatus, String, String),
    (std::process::ExitStatus, String, String),
) {
    let args = [
        "pull",
        "--from",
        env_path.to_str().unwrap(),
        "--query",
        "MATCH (n) RETURN n",
        "--database",
        db.to_str().unwrap(),
    ];
    let first = run_cli(Opts::new(&args).in_home(home));
    assert!(
        first.0.success(),
        "first pull failed: {} {}",
        first.2,
        first.1
    );
    let second = run_cli(Opts::new(&args).in_home(home));
    (first, second)
}

#[test]
fn test_help_lists_pull() {
    let home = tempdir();
    let (status, stdout, stderr) = run_cli(Opts::new(&["pull", "--help"]).in_home(home.path()));
    assert!(status.success(), "pull --help failed: {stderr}");
    assert!(
        stdout.contains("relate pull"),
        "expected pull in pull --help, got: {stdout}"
    );
}

#[test]
fn test_pull_requires_query() {
    let home = tempdir();
    let (status, _stdout, stderr) =
        run_cli(Opts::new(&["pull", "--database", "./graph"]).in_home(home.path()));
    assert_eq!(status.code(), Some(2));
    assert!(
        stderr.contains("--query"),
        "expected --query usage error, got: {stderr}"
    );
}

#[test]
fn test_pull_requires_database() {
    let home = tempdir();
    let (status, _stdout, stderr) =
        run_cli(Opts::new(&["pull", "--query", "MATCH (n) RETURN n"]).in_home(home.path()));
    assert_eq!(status.code(), Some(2));
    assert!(
        stderr.contains("--database"),
        "expected DATABASE usage error, got: {stderr}"
    );
}

#[test]
fn test_pull_rejects_memory() {
    let home = tempdir();
    let (status, _stdout, stderr) = run_cli(
        Opts::new(&[
            "pull",
            "--query",
            "MATCH (n) RETURN n",
            "--database",
            ":memory:",
        ])
        .in_home(home.path()),
    );
    assert_eq!(status.code(), Some(2));
    assert!(
        stderr.contains(":memory:"),
        "expected :memory: usage error, got: {stderr}"
    );
}

#[test]
fn test_pull_rejects_unknown_flag() {
    let home = tempdir();
    let (status, _stdout, stderr) = run_cli(
        Opts::new(&[
            "pull",
            "--bogus",
            "--query",
            "MATCH (n) RETURN n",
            "--database",
            "./g",
        ])
        .in_home(home.path()),
    );
    assert_eq!(status.code(), Some(2));
    assert!(
        stderr.contains("unexpected"),
        "expected unexpected-argument error, got: {stderr}"
    );
}

#[test]
fn test_pull_loopback_creates_graph() {
    // After the data body the source answers the pull's schema fetch
    // (ADR-0201): one uniqueness constraint and its backing RANGE index
    // (filtered, arrives with the constraint), plus one standalone index.
    const CONSTRAINTS_BODY: &str = r#"{"data":{"fields":["name","type","entityType","labelsOrTypes","properties","propertyType"],"values":[["person_name","NODE_PROPERTY_UNIQUENESS","NODE",["Person"],["name"],null]]},"errors":[]}"#;
    const INDEXES_BODY: &str = r#"{"data":{"fields":["name","type","entityType","labelsOrTypes","properties","owningConstraint","state"],"values":[["person_name","RANGE","NODE",["Person"],["name"],"person_name","ONLINE"],["person_name_idx","RANGE","NODE",["Person"],["name"],null,"ONLINE"]]},"errors":[]}"#;
    let (handle, port) =
        spawn_scripted_responses("200 OK", vec![NODE_BODY, CONSTRAINTS_BODY, INDEXES_BODY]);
    let home = tempdir();
    let env_path = write_env_file(home.path(), port);
    let db = home.path().join("mirror");
    let (status, stdout, stderr) = run_cli(
        Opts::new(&[
            "pull",
            "--from",
            env_path.to_str().unwrap(),
            "--query",
            "MATCH (n) RETURN n",
            "--database",
            db.to_str().unwrap(),
        ])
        .in_home(home.path()),
    );
    assert!(
        status.success(),
        "pull failed: status={:?} stderr={stderr} stdout={stdout}",
        status.code()
    );
    assert!(
        stdout.contains("pulled 1 new / 0 reused nodes, 0 new / 0 reused rels"),
        "expected pull summary, got: {stdout}"
    );
    assert!(
        stdout.contains("+ 1 constraints, 1 indexes; 0 schema skipped"),
        "expected schema counts in the summary (ADR-0201), got: {stdout}"
    );
    handle.join().ok();
    // The pulled store carries the source schema (ADR-0201).
    let (sstatus, sout, serr) = run_cli(
        Opts::new(&["--database", db.to_str().unwrap(), "SHOW CONSTRAINTS"]).in_home(home.path()),
    );
    assert!(sstatus.success(), "SHOW CONSTRAINTS failed: {serr}");
    assert!(
        sout.contains("person_name") && sout.contains("NODE_PROPERTY_UNIQUENESS"),
        "expected the pulled constraint, got: {sout}"
    );
    // A new store gets the build's default codec: JSON without `parquet`.
    let expected = if cfg!(feature = "parquet") {
        "parquet"
    } else {
        "json"
    };
    assert_eq!(manifest_codec(&db), expected);
    let (qstatus, qout, qerr) = run_cli(
        Opts::new(&[
            "--database",
            db.to_str().unwrap(),
            "MATCH (n:Person) RETURN n.name",
        ])
        .in_home(home.path()),
    );
    assert!(qstatus.success(), "query after pull failed: {qerr}");
    assert!(qout.contains("Ada"), "expected Ada, got: {qout}");
}

#[test]
fn test_pull_error_body_control_bytes_are_sanitized() {
    // A remote error body is attacker-controlled text. A body carrying a real
    // ESC (0x1B) must not reach the CLI's stderr raw: both the loader's
    // `truncate_for_error` and the `pull` print site strip unsafe characters.
    // The sanitized message keeps `denied[31m`, proving the ESC between
    // `denied` and `[31m` was stripped rather than never sent.
    const EVIL_BODY: &str = "{\"error\":\"denied\u{1b}[31m by server\"}";
    let (handle, port) = spawn_single_response("401 Unauthorized", EVIL_BODY);
    let home = tempdir();
    let env_path = write_env_file(home.path(), port);
    let db = home.path().join("mirror");
    let (status, _stdout, stderr) = run_cli(
        Opts::new(&[
            "pull",
            "--from",
            env_path.to_str().unwrap(),
            "--query",
            "MATCH (n) RETURN n",
            "--database",
            db.to_str().unwrap(),
        ])
        .in_home(home.path()),
    );
    assert!(
        !status.success(),
        "a 401 from the remote must fail the pull, stderr: {stderr:?}"
    );
    assert!(
        !stderr.as_bytes().contains(&0x1b),
        "raw ESC survived into pull stderr: {stderr:?}"
    );
    assert!(
        stderr.contains("denied[31m by server"),
        "expected the sanitized remote body in stderr, got: {stderr:?}"
    );
    handle.join().ok();
}

#[test]
fn test_pull_broken_uri_password_not_in_stderr() {
    // A NEO4J_URI carrying userinfo (`user:secretpw@host:port`) is rejected
    // outright at the query_api_url boundary (ADR-0122): pull credentials
    // come from NEO4J_USERNAME/NEO4J_PASSWORD. The userinfo error is fixed
    // text: a password sitting in the URI must never reach stderr/stdout.
    let home = tempdir();
    let env_path = home.path().join("broken.env");
    std::fs::write(
        &env_path,
        "NEO4J_URI=user:secretpw@127.0.0.1:7687\nNEO4J_USERNAME=neo4j\nNEO4J_PASSWORD=pw\n",
    )
    .unwrap();
    let db = home.path().join("mirror");
    let (status, stdout, stderr) = run_cli(
        Opts::new(&[
            "pull",
            "--from",
            env_path.to_str().unwrap(),
            "--query",
            "MATCH (n) RETURN n",
            "--database",
            db.to_str().unwrap(),
        ])
        .in_home(home.path()),
    );
    assert_eq!(
        status.code(),
        Some(1),
        "a malformed NEO4J_URI is a runtime error: stderr={stderr:?} stdout={stdout:?}"
    );
    assert!(
        stderr.contains("userinfo"),
        "expected a userinfo rejection, got: {stderr:?}"
    );
    assert!(
        !stderr.contains("secretpw"),
        "URI password leaked into pull stderr: {stderr:?}"
    );
    assert!(
        !stdout.contains("secretpw"),
        "URI password leaked into pull stdout: {stdout:?}"
    );
}

#[test]
fn test_pull_repeat_is_refused() {
    let (handle, port) = spawn_scripted_responses("200 OK", vec![NODE_BODY, NODE_BODY]);
    let home = tempdir();
    let env_path = write_env_file(home.path(), port);
    let db = home.path().join("mirror");
    let (_, (status2, stdout2, stderr2)) = pull_twice(home.path(), &env_path, &db);
    let out2 = format!("{stdout2} {stderr2}");
    assert!(
        !status2.success(),
        "second pull must be refused (ADR-0158): {out2}"
    );
    assert!(out2.contains("not empty"), "expected the refusal: {out2}");
    handle.join().ok();
}

#[test]
fn test_pull_repeat_is_refused_json_store() {
    let (handle, port) = spawn_scripted_responses("200 OK", vec![REL_BODY, REL_BODY]);
    let home = tempdir();
    let env_path = write_env_file(home.path(), port);
    let db = home.path().join("mirror");
    // LABS-104 repro: the one-shot's clean-exit checkpoint pins the codec.
    seed_database(home.path(), &db, &["--snapshot-format", "json"], "RETURN 1");
    assert_json_codec(&db);
    let ((_, stdout1, _), (status2, stdout2, stderr2)) = pull_twice(home.path(), &env_path, &db);
    assert!(
        stdout1.contains("pulled 2 new / 0 reused nodes, 1 new / 0 reused rels"),
        "expected new-entity summary, got: {stdout1}"
    );
    let out2 = format!("{stdout2} {stderr2}");
    assert!(
        !status2.success(),
        "second pull must be refused (ADR-0158): {out2}"
    );
    assert!(out2.contains("not empty"), "expected the refusal: {out2}");
    handle.join().ok();
    assert_json_codec(&db);
    assert!(!has_parquet_file(&db), "unexpected Parquet files in {db:?}");
}

fn has_parquet_file(dir: &Path) -> bool {
    std::fs::read_dir(dir).unwrap().any(|entry| {
        let path = entry.unwrap().path();
        if path.is_dir() {
            has_parquet_file(&path)
        } else {
            path.extension().is_some_and(|ext| ext == "parquet")
        }
    })
}

fn assert_json_codec(db: &Path) {
    assert!(db.join("graph.json").is_file(), "expected a JSON store");
    assert_eq!(manifest_codec(db), "json");
}

#[test]
fn test_pull_rejects_namespace_flag() {
    // ADR-0118 §3: pull always writes neo4j, so --namespace is unknown to it.
    let home = tempdir();
    let (status, _stdout, stderr) = run_cli(
        Opts::new(&[
            "pull",
            "--namespace",
            "system_ontologies",
            "--query",
            "MATCH (n) RETURN n",
            "--database",
            "./graph",
        ])
        .in_home(home.path()),
    );
    assert_eq!(status.code(), Some(2));
    assert!(
        stderr.contains("unexpected argument to pull: --namespace"),
        "expected --namespace reported as unknown to pull, got: {stderr}"
    );
}

#[test]
fn test_pull_rejects_preverb_namespace_flag() {
    // A valid --namespace before the `pull` verb is consumed by the main
    // flag loop and would be silently ignored (pull always writes neo4j);
    // it must fail with exit 2, exactly as the post-verb placement does.
    let home = tempdir();
    let (status, _stdout, stderr) = run_cli(
        Opts::new(&[
            "--namespace",
            "system",
            "pull",
            "--query",
            "MATCH (n) RETURN n",
            "--database",
            "./graph",
        ])
        .in_home(home.path()),
    );
    assert_eq!(status.code(), Some(2));
    assert!(
        stderr.contains(
            "Error: --namespace is not valid with pull (pull always writes the neo4j namespace)"
        ),
        "expected the pre-verb --namespace rejection, got: {stderr}"
    );
}
