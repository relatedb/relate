//! `--fetch-params NAME=SOURCE` (ADR-0267): fetch a URL (stub HTTP server here) or
//! read a file, parse it as a JSON, and bind it as a query parameter.
//!
//! Compiled only with the `fetch-params` feature: these fetch-behavior tests
//! need it. Every build parses the flag (ADR-0270); the `minimal` binary
//! recognizes it and fails closed — pinned in `e2e_bundles.rs`.
#![cfg(feature = "fetch-params")]

use std::process::ExitStatus;

use relate_test_support::loopback::spawn_single_response;

use crate::support;

/// Run the CLI through the canonical harness: isolated `$HOME` and a refused
/// Mixpanel endpoint, so a telemetry-enabled build cannot touch the real home
/// dir or POST to the live endpoint.
fn run(args: &[&str], home: &std::path::Path) -> (ExitStatus, String, String) {
    support::run_cli(support::CliRunOptions::new(args).in_home(home))
}

/// Minimal one-shot HTTP stub: the canonical loopback fixture serves `body`
/// with `status` to the first request. The server thread is never joined —
/// if the CLI never contacts the stub (a regression), an accept-blocked
/// thread must not hang the test, and the fixture's accept deadline reaps
/// it — the CLI output is what is asserted.
fn stub_server(status: &'static str, body: &'static str) -> String {
    let (_handle, port) = spawn_single_response(status, body);
    format!("http://127.0.0.1:{port}")
}

#[test]
fn file_source_binds_json_as_parameter() {
    let home = tempfile::tempdir().unwrap();
    let doc = home.path().join("doc.json");
    std::fs::write(&doc, r#"{"items": [1, 2, 3]}"#).unwrap();
    let doc = doc.to_str().unwrap();
    let (status, stdout, stderr) = run(
        &[
            "-json",
            "--fetch-params",
            &format!("value={doc}"),
            "-c",
            "RETURN size($value.items) AS n",
        ],
        home.path(),
    );
    assert!(status.success(), "stderr: {stderr}");
    assert!(stdout.contains("\"n\":3"));
}

#[test]
fn url_source_binds_stub_response_as_parameter() {
    let home = tempfile::tempdir().unwrap();
    let url = stub_server("200 OK", r#"{"ok": true, "count": 42}"#);
    let (status, stdout, stderr) = run(
        &[
            "-json",
            "--fetch-params",
            &format!("value={url}/x?a=b"),
            "-c",
            "RETURN $value.count AS count",
        ],
        home.path(),
    );
    assert!(status.success(), "stderr: {stderr}");
    assert!(stdout.contains("42"));
}

#[test]
fn repeatable_fetches_bind_distinct_parameters() {
    let home = tempfile::tempdir().unwrap();
    let a = home.path().join("a.json");
    let b = home.path().join("b.json");
    std::fs::write(&a, r#"{"x": 1}"#).unwrap();
    std::fs::write(&b, r#"{"y": 2}"#).unwrap();
    let (a, b) = (a.to_str().unwrap(), b.to_str().unwrap());
    let (status, stdout, stderr) = run(
        &[
            "-json",
            "--fetch-params",
            &format!("one={a}"),
            "--fetch-params",
            &format!("two={b}"),
            "-c",
            "RETURN $one.x + $two.y AS sum",
        ],
        home.path(),
    );
    assert!(status.success(), "stderr: {stderr}");
    assert!(stdout.contains("\"sum\":3"));
}

#[test]
fn malformed_flag_is_a_usage_error() {
    let home = tempfile::tempdir().unwrap();
    for bad in ["novalue", "=x", "v=ftp://host/x", "v=", "v=file://"] {
        let (status, _, _) = run(&["--fetch-params", bad, "-c", "RETURN 1"], home.path());
        assert_eq!(status.code(), Some(2), "flag {bad:?} should exit 2");
    }
}

#[test]
fn fetch_and_parse_failures_are_runtime_errors() {
    let home = tempfile::tempdir().unwrap();

    // missing file
    let (status, _, stderr) = run(
        &[
            "--fetch-params",
            "v=/definitely/not/here.json",
            "-c",
            "RETURN 1",
        ],
        home.path(),
    );
    assert_eq!(status.code(), Some(1));
    assert!(stderr.contains("--fetch-params v:"));

    // invalid JSON
    let doc = home.path().join("bad.json");
    std::fs::write(&doc, "not json at all").unwrap();
    let (status, _, stderr) = run(
        &[
            "--fetch-params",
            &format!("v={}", doc.to_str().unwrap()),
            "-c",
            "RETURN 1",
        ],
        home.path(),
    );
    assert_eq!(status.code(), Some(1));
    assert!(stderr.contains("invalid JSON"));

    // HTTP error status
    let url = stub_server("500 Internal Server Error", r#"{"error": "boom"}"#);
    let (status, _, stderr) = run(
        &["--fetch-params", &format!("v={url}/err"), "-c", "RETURN 1"],
        home.path(),
    );
    assert_eq!(status.code(), Some(1));
    assert!(stderr.contains("HTTP 500"));
}

#[test]
fn oversized_source_is_a_runtime_error() {
    // MAX_SOURCE_BYTES + 1 bytes on disk: the read must detect, not truncate.
    let home = tempfile::tempdir().unwrap();
    let doc = home.path().join("big.bin");
    let cap = 64 * 1024 * 1024;
    let mut body = vec![b'0'; cap + 1];
    body[0] = b'{';
    body[cap] = b'}';
    std::fs::write(&doc, &body).unwrap();
    let (status, _, stderr) = run(
        &[
            "--fetch-params",
            &format!("v={}", doc.to_str().unwrap()),
            "-c",
            "RETURN 1",
        ],
        home.path(),
    );
    assert_eq!(status.code(), Some(1));
    assert!(stderr.contains("cap"), "stderr: {stderr}");
}

#[test]
fn help_lists_the_fetch_params_flag() {
    let home = tempfile::tempdir().unwrap();
    let (status, stdout, _) = run(&["--help"], home.path());
    assert!(status.success());
    assert!(
        stdout.contains("--fetch-params N=SRC"),
        "--help must list --fetch-params (AGENTS.md: help text is the source of truth)"
    );
}

#[test]
fn help_with_a_fetch_params_flag_does_no_side_effect_io() {
    // --help exits 0 before any --fetch-params source is resolved, so a missing
    // file (or a URL) is never fetched on the help path.
    let home = tempfile::tempdir().unwrap();
    let (status, stdout, _) = run(
        &["--fetch-params", "v=/definitely/not/here.json", "--help"],
        home.path(),
    );
    assert!(status.success());
    assert!(stdout.contains("Usage:"));
}

#[test]
fn last_assignment_for_a_name_wins_across_param_and_fetch() {
    let home = tempfile::tempdir().unwrap();
    let doc = home.path().join("doc.json");
    std::fs::write(&doc, r#"{"x": 1}"#).unwrap();
    let doc = doc.to_str().unwrap();

    // --param after --fetch-params: the param wins and the overridden source is
    // never fetched (the file below does not exist, so a fetch would exit 1).
    let (status, stdout, stderr) = run(
        &[
            "-json",
            "--fetch-params",
            "v=/definitely/not/here.json",
            "--param",
            "v=42",
            "-c",
            "RETURN $v AS v",
        ],
        home.path(),
    );
    assert!(status.success(), "stderr: {stderr}");
    assert!(stdout.contains("42"));

    // --fetch-params after --param: the fetched value wins.
    let (status, stdout, stderr) = run(
        &[
            "-json",
            "--param",
            "v=42",
            "--fetch-params",
            &format!("v={doc}"),
            "-c",
            "RETURN $v.x AS x",
        ],
        home.path(),
    );
    assert!(status.success(), "stderr: {stderr}");
    assert!(stdout.contains("1"));
}

#[test]
fn single_dash_fetch_params_is_not_a_flag() {
    // Per AGENTS.md, single-dash spellings are legacy aliases only; -fetch-params
    // must hit the unknown-flag usage error like any other new flag.
    let home = tempfile::tempdir().unwrap();
    let (status, _, stderr) = run(
        &["-fetch-params", "v=/tmp/x.json", "-c", "RETURN 1"],
        home.path(),
    );
    assert_eq!(status.code(), Some(2));
    assert!(stderr.contains("unknown flag"));
}
