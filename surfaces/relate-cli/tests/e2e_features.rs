//! Relate durability and transaction e2e (directory + WAL).

use std::process::ExitStatus;

#[allow(dead_code)]
mod support;

use support::{bundle_manifest_codec, manifest_codec, OPEN_TRANSACTION_WARNING};

/// The shared harness with `&str` stdin, which every test here passes.
fn run_cli(args: &[&str], stdin: Option<&str>) -> (ExitStatus, String, String) {
    let opts = support::CliRunOptions::new(args);
    support::run_cli(match stdin {
        Some(input) => opts.with_stdin(input.as_bytes()),
        None => opts,
    })
}

#[test]
fn test_directory_wal_roundtrip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("graph");
    let db_path = db.to_str().unwrap();

    let (status, _stdout, stderr) = run_cli(
        &["--database", db_path, "CREATE (:Person {name: 'Alice'});"],
        None,
    );
    assert!(status.success(), "create failed: {stderr}");

    let (status, stdout, stderr) = run_cli(
        &[
            "-csv",
            "--database",
            db_path,
            "MATCH (p:Person) RETURN p.name AS n;",
        ],
        None,
    );
    assert!(status.success(), "reopen failed: {stderr}");
    assert!(
        stdout.contains("Alice"),
        "expected Alice after WAL reopen, got: {stdout}"
    );
}

#[test]
fn test_explicit_transaction_commit_persists() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("txn");
    let db_path = db.to_str().unwrap();
    let script = ".begin\nCREATE (:T {n: 1});\n.commit\n";
    let (status, _stdout, stderr) = run_cli(&["query", "--database", db_path], Some(script));
    assert!(status.success(), "txn commit failed: {stderr}");

    let (status, stdout, stderr) = run_cli(
        &[
            "-csv",
            "--database",
            db_path,
            "MATCH (n:T) RETURN count(*) AS c;",
        ],
        None,
    );
    assert!(status.success(), "reopen failed: {stderr}");
    assert!(
        stdout.lines().any(|l| l.trim() == "1"),
        "committed write must persist, got: {stdout}"
    );
}

#[test]
fn test_explicit_transaction_rollback_does_not_persist() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("txn_rb");
    let db_path = db.to_str().unwrap();
    let (status, _stdout, stderr) =
        run_cli(&["--database", db_path, "CREATE (:Keep {n: 1});"], None);
    assert!(status.success(), "setup failed: {stderr}");

    let script = ".begin\nCREATE (:Gone {n: 2});\n.rollback\n";
    let (status, _stdout, stderr) = run_cli(&["query", "--database", db_path], Some(script));
    assert!(status.success(), "txn rollback failed: {stderr}");

    let (status, stdout, stderr) = run_cli(
        &[
            "-csv",
            "--database",
            db_path,
            "MATCH (n) RETURN labels(n)[0] AS l ORDER BY l;",
        ],
        None,
    );
    assert!(status.success(), "reopen failed: {stderr}");
    assert!(stdout.contains("Keep"), "kept node missing: {stdout}");
    assert!(
        !stdout.contains("Gone"),
        "rolled-back node leaked: {stdout}"
    );
}

#[test]
fn test_json_snapshot_codec_roundtrip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("json_snap");
    let db_path = db.to_str().unwrap();
    let (status, _stdout, stderr) = run_cli(
        &[
            "--snapshot-format",
            "json",
            "--database",
            db_path,
            "CREATE (:Codec {ok: true});",
        ],
        None,
    );
    assert!(status.success(), "json snapshot create failed: {stderr}");
    let (status, stdout, stderr) = run_cli(
        &[
            "--snapshot-format",
            "json",
            "-csv",
            "--database",
            db_path,
            "MATCH (n:Codec) RETURN n.ok AS ok;",
        ],
        None,
    );
    assert!(status.success(), "json snapshot reopen failed: {stderr}");
    assert!(stdout.contains("true"), "expected true, got: {stdout}");
}

/// ADR-0553: the Parquet codec's checkpoint format is the wide-column
/// part encoding — a fresh store's exit checkpoint publishes a v5
/// manifest with no flag, and a plain reopen keeps it wide.
#[cfg(feature = "parquet")]
#[test]
fn test_parquet_snapshot_is_wide_by_default() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("wide_snap");
    let db_path = db.to_str().unwrap();
    let (status, _stdout, stderr) = run_cli(
        &["--database", db_path, "CREATE (:Codec {ok: true});"],
        None,
    );
    assert!(status.success(), "snapshot create failed: {stderr}");
    let manifest = support::manifest_json(&db);
    assert_eq!(
        manifest["format_version"], 5,
        "the Parquet codec publishes the wide v5 manifest with no flag"
    );
    // Reopened plainly: the wide manifest keeps the store wide.
    let (status, stdout, stderr) = run_cli(
        &[
            "-csv",
            "--database",
            db_path,
            "MATCH (n:Codec) RETURN n.ok AS ok;",
        ],
        None,
    );
    assert!(status.success(), "plain reopen failed: {stderr}");
    assert!(stdout.contains("true"), "expected true, got: {stdout}");
}

fn count_label(db_path: &str, label: &str) -> String {
    let query = format!("MATCH (n:{label}) RETURN count(n) AS c;");
    let (status, stdout, stderr) = run_cli(&["-csv", "--database", db_path, &query], None);
    assert!(status.success(), "reopen failed: {stderr}");
    stdout.lines().nth(1).unwrap_or_default().trim().to_string()
}

fn assert_parquet_reopen_rejected(db_path: &str) {
    let (status, _stdout, stderr) = run_cli(
        &[
            "--snapshot-format",
            "parquet",
            "--database",
            db_path,
            "RETURN 1",
        ],
        None,
    );
    assert_eq!(status.code(), Some(1), "stderr: {stderr}");
    let expected = if cfg!(feature = "parquet") {
        "snapshot codec mismatch: directory is json, requested parquet"
    } else {
        r#"parquet snapshot codec is not compiled into this build (enable the "parquet" feature)"#
    };
    assert!(stderr.contains(expected), "stderr: {stderr}");
}

#[test]
fn test_one_shot_exit_pins_requested_snapshot_codec() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("pinned");
    let db_path = db.to_str().unwrap();
    let (status, _stdout, stderr) = run_cli(
        &[
            "--snapshot-format",
            "json",
            "--database",
            db_path,
            "RETURN 1",
        ],
        None,
    );
    assert!(status.success(), "one-shot failed: {stderr}");
    assert_eq!(manifest_codec(&db), "json");
    let (status, _stdout, stderr) = run_cli(&["--database", db_path, "RETURN 1"], None);
    assert!(status.success(), "reopen without flag failed: {stderr}");
    assert_parquet_reopen_rejected(db_path);
}

#[test]
fn test_one_shot_exit_pins_snapshot_codec_in_bundle() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("x.rl");
    let db_path = db.to_str().unwrap();
    let (status, _stdout, stderr) = run_cli(
        &[
            "--snapshot-format",
            "json",
            "--database",
            db_path,
            "RETURN 1",
        ],
        None,
    );
    assert!(status.success(), "one-shot failed: {stderr}");
    assert_eq!(bundle_manifest_codec(&db), "json");
    // Without the Parquet codec the rejection cannot tell a pinned codec
    // from an unpinned one; the manifest can.
    assert_parquet_reopen_rejected(db_path);
    assert_eq!(bundle_manifest_codec(&db), "json");
}

#[test]
fn test_piped_stdin_exit_checkpoints() {
    for script in ["CREATE (:A);", "CREATE (:A);\n.quit\n"] {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("piped");
        let db_path = db.to_str().unwrap();
        let (status, _stdout, stderr) = run_cli(
            &["query", "--snapshot-format", "json", "--database", db_path],
            Some(script),
        );
        assert!(status.success(), "{script:?} failed: {stderr}");
        assert_eq!(manifest_codec(&db), "json", "script {script:?}");
        assert_eq!(count_label(db_path, "A"), "1", "script {script:?}");
    }
}

#[test]
fn test_quit_discards_open_transaction_then_checkpoints() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("quit_txn");
    let db_path = db.to_str().unwrap();
    let script = ".begin\nCREATE (:Gone);\n.quit\n";
    let (status, _stdout, stderr) = run_cli(&["query", "--database", db_path], Some(script));
    // ADR-0164: piped stdin that reaches exit with an open transaction
    // warns on stderr and exits 1; the discard and the final checkpoint
    // are unchanged.
    assert_eq!(status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains(OPEN_TRANSACTION_WARNING),
        "stderr: {stderr}"
    );
    assert!(
        db.join("manifest.json").is_file(),
        "final checkpoint missing"
    );
    assert_eq!(count_label(db_path, "Gone"), "0");
}

#[test]
fn test_failed_one_shot_still_checkpoints() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("failed");
    let db_path = db.to_str().unwrap();
    let (status, _stdout, stderr) = run_cli(&["--database", db_path, "RETURN ("], None);
    assert_eq!(status.code(), Some(6), "stderr: {stderr}");
    assert!(
        db.join("manifest.json").is_file(),
        "final checkpoint missing"
    );
}
