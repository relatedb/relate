//! The ontology preview opt-in end to end through the CLI binary
//! (ADR-0378): with `RELATE_PREVIEW` absent every `db.ontology.*` call
//! fails with ADR-0371's authored preview error and `SHOW PROCEDURES` hides
//! the four rows; with `RELATE_PREVIEW=ontology` the document
//! round-trips and the rows are listed. A stored document survives
//! reopening under the gate — the gate governs callability, not stored
//! state — and an unknown feature name fails the open loudly, naming the
//! registry.
//!
//! Every CLI invocation goes through the shared [`support::run_cli`]
//! harness, whose [`support::cli_command`] scrubs the ambient
//! `RELATE_PREVIEW` before the gate-on value below is applied, so a
//! developer shell that exports the opt-in cannot turn a gate-off assertion
//! into a gate-on one. The one PTY probe goes through
//! [`support::assert_pty_driver_ok`] instead; [`support::pty_command`] applies
//! the same scrub to the Python driver whose environment the PTY child
//! inherits.

#[allow(dead_code)]
mod support;

use std::process::ExitStatus;

use serde_json::Value;
use support::{run_cli, CliRunOptions};

/// ADR-0371's authored disabled-feature text for the `ontology` row; the
/// gate-off assertions pin it byte-for-byte.
const DISABLED: &str = "semantic error: SemanticError: preview feature 'ontology' is not \
     enabled: set RELATE_PREVIEW=ontology";

/// The four procedures ADR-0378 gates, in the registry's listing order.
const ONTOLOGY_PROCEDURES: [&str; 4] = [
    "db.ontology.set",
    "db.ontology.get",
    "db.ontology.drop",
    "db.ontology.apply",
];

/// The gate-on environment: the opt-in the document family requires.
const GATE_ON: &[(&str, &str)] = &[("RELATE_PREVIEW", "ontology")];

/// A fresh tempdir and the store path inside it, in the argv form the CLI
/// expects. The returned guard must outlive every run against the store.
fn fresh_store() -> (tempfile::TempDir, String) {
    let dir = support::tempdir();
    let store = dir.path().join("store");
    let store = store.to_str().expect("utf-8 temp path").to_string();
    (dir, store)
}

/// One gate-on `db.ontology.set` of the fixture document into `db`, in the
/// suite's JSONL output mode. Returns the document value a later `get` must
/// serve.
fn store_ontology(db: &str) -> Value {
    let document = ontology_document();
    let expected: Value = serde_json::from_str(&document).expect("the fixture is JSON");
    let param = ontology_param(&document);

    let (status, _stdout, stderr) = run_cli(
        CliRunOptions::new(&[
            "query",
            "CALL db.ontology.set($ontology)",
            "--param",
            param.as_str(),
            "--mode",
            "jsonl",
            "-d",
            db,
        ])
        .with_envs(GATE_ON),
    );
    assert!(status.success(), "db.ontology.set failed: {stderr}");
    expected
}

/// The valid main-shape Ontology Graph Spec 1.0.0 document the tests store
/// (ADR-0364): a key property and a required property on one node type.
fn ontology_document() -> String {
    let path = format!(
        "{}/tests/fixtures/ontology.json",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// `--param ontology=<the document as a JSON string>`; `set` takes the
/// document as a STRING parameter, not as a map.
fn ontology_param(document: &str) -> String {
    format!(
        "ontology={}",
        serde_json::to_string(document).expect("a document serializes as a JSON string")
    )
}

fn rows(stdout: &str) -> Vec<Value> {
    // LABS-1120: the stream's timing terminal line is metadata, not a row
    // (the one typed discriminator, `support::timing_terminal_line`).
    stdout
        .lines()
        .filter(|line| !support::timing_terminal_line(line))
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line).unwrap_or_else(|e| panic!("row is JSON: {e}: {line}"))
        })
        .collect()
}

/// `db.ontology.get(...)`'s single row, with the `ontology` column parsed as
/// the document JSON it carries. The column is a string even when the stored
/// bytes are canonical JSON, so parse before comparing: the CLI may return
/// the stored bytes and the test must not treat formatting as content.
fn returned_ontology(stdout: &str) -> Value {
    let rows = rows(stdout);
    assert_eq!(rows.len(), 1, "get returns exactly one row: {stdout}");
    let document = rows[0]["ontology"]
        .as_str()
        .unwrap_or_else(|| panic!("get's ontology column is a string: {stdout}"));
    serde_json::from_str(document)
        .unwrap_or_else(|e| panic!("the returned ontology is JSON: {e}: {document}"))
}

fn procedure_names(stdout: &str) -> Vec<String> {
    rows(stdout)
        .into_iter()
        .map(|row| {
            row["name"]
                .as_str()
                .expect("every SHOW PROCEDURES row carries a name")
                .to_string()
        })
        .collect()
}

fn assert_disabled(status: ExitStatus, stderr: &str) {
    assert_eq!(status.code(), Some(6), "stderr: {stderr}");
    assert!(
        stderr.contains(DISABLED),
        "expected the authored preview error on stderr, got: {stderr}"
    );
}

#[test]
fn gate_off_rejects_calls_and_hides_the_ontology_rows() {
    let (_dir, db) = fresh_store();

    let (status, _stdout, stderr) = run_cli(CliRunOptions::new(&[
        "query",
        "CALL db.ontology.get()",
        "--mode",
        "jsonl",
        "-d",
        db.as_str(),
    ]));
    assert_disabled(status, &stderr);

    let (status, stdout, stderr) = run_cli(CliRunOptions::new(&[
        "query",
        "SHOW PROCEDURES YIELD name",
        "--mode",
        "jsonl",
        "-d",
        db.as_str(),
    ]));
    assert!(status.success(), "SHOW PROCEDURES failed: {stderr}");
    assert!(
        !stdout.contains("db.ontology."),
        "the disabled gate lists no db.ontology.* row: {stdout}"
    );
    assert!(
        !procedure_names(&stdout).is_empty(),
        "the catalog still lists the other procedures: {stdout}"
    );
}

/// The reviewer-visible symptom of an inherited opt-in is a PTY child's
/// `SHOW PROCEDURES` listing the gated family. This runs the real PTY path
/// under the suite's own environment: with `RELATE_PREVIEW` exported the
/// child must still see the gate off, because [`support::pty_command`]
/// scrubs the Python driver before `pty_driver.fork_exec` copies its
/// `os.environ`. The gate assertion (no `db.ontology.*` row, catalog still
/// rendered) lives in `tests/support/pty_env_probe.py`; the driver's `OK` is
/// [`support::assert_pty_driver_ok`]'s assertion.
#[cfg(unix)]
#[test]
fn pty_child_cannot_see_an_inherited_preview_opt_in() {
    let binary = support::binary_path_str();
    support::assert_pty_driver_ok("pty_env_probe.py", &[&binary]);
}

#[test]
fn gate_on_stores_reads_and_lists_the_ontology_family() {
    let (_dir, db) = fresh_store();
    let expected = store_ontology(&db);

    let (status, stdout, stderr) = run_cli(
        CliRunOptions::new(&[
            "query",
            "CALL db.ontology.get('json') YIELD ontology RETURN ontology",
            "--mode",
            "jsonl",
            "-d",
            db.as_str(),
        ])
        .with_envs(GATE_ON),
    );
    assert!(status.success(), "db.ontology.get failed: {stderr}");
    assert_eq!(returned_ontology(&stdout), expected);

    let (status, stdout, stderr) = run_cli(
        CliRunOptions::new(&[
            "query",
            "SHOW PROCEDURES YIELD name",
            "--mode",
            "jsonl",
            "-d",
            db.as_str(),
        ])
        .with_envs(GATE_ON),
    );
    assert!(status.success(), "SHOW PROCEDURES failed: {stderr}");
    let names = procedure_names(&stdout);
    for name in ONTOLOGY_PROCEDURES {
        assert!(
            names.iter().any(|listed| listed == name),
            "expected {name} in SHOW PROCEDURES, got: {stdout}"
        );
    }
}

#[test]
fn reopening_the_same_store_respects_the_gate() {
    let (_dir, db) = fresh_store();
    let expected = store_ontology(&db);

    // Reopened with the opt-in absent: the stored state opens and survives,
    // but the procedure surface is gated exactly as on a fresh store.
    let (status, _stdout, stderr) = run_cli(CliRunOptions::new(&[
        "query",
        "CALL db.ontology.get()",
        "--mode",
        "jsonl",
        "-d",
        db.as_str(),
    ]));
    assert_disabled(status, &stderr);

    // Reopened with the opt-in set: the same document is served again.
    let (status, stdout, stderr) = run_cli(
        CliRunOptions::new(&[
            "query",
            "CALL db.ontology.get('json') YIELD ontology RETURN ontology",
            "--mode",
            "jsonl",
            "-d",
            db.as_str(),
        ])
        .with_envs(GATE_ON),
    );
    assert!(
        status.success(),
        "db.ontology.get after reopen failed: {stderr}"
    );
    assert_eq!(returned_ontology(&stdout), expected);
}

#[test]
fn unknown_preview_feature_fails_open_naming_the_known_features() {
    // The unknown name is rejected in `Database::open`'s preflight, before
    // any store is leased, so the default in-memory store is enough here.
    let (status, _stdout, stderr) = run_cli(
        CliRunOptions::new(&[
            "query",
            "RETURN 1 AS n",
            "--mode",
            "jsonl",
            "-d",
            ":memory:",
        ])
        .with_envs(&[("RELATE_PREVIEW", "bogus")]),
    );
    assert_eq!(status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains("unknown preview feature 'bogus'"),
        "expected the unknown-name error on stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("known features:") && stderr.contains("ontology"),
        "expected the known-features list to name `ontology`, got: {stderr}"
    );
}
