//! The remote `--load` source (LABS-872, ADR-0496; the `--load` grammar is
//! LABS-903, ADR-0500): an `http(s)` URL naming a remote `.rl` bundle is
//! fetched (stub HTTP server here) and staged at a temp path — always
//! scratch. With the `:memory:` target the copy is released right after
//! the load (pinned in `e2e_load.rs`); with a `--database` path
//! target the copy is cloned into the caller's store and removed after the
//! clone.
//!
//! Compiled only with the `remote-database` feature: these fetch-behavior
//! tests need it. The policy refusals (`--safe`, `--read-only=live`, the
//! URL shapes) run in **every** build — the fetch gate is downstream of
//! them — so `e2e_bundles.rs` pins them in the every-bundle lane, including
//! the fail-closed fetch without the feature.
#![cfg(feature = "remote-database")]

use std::process::ExitStatus;

use relate_test_support::loopback::spawn_single_response_bytes_capturing;

use crate::support;

use support::{person_bundle, run_cli, staged_path, CliRunOptions as Opts};

/// Run the CLI through the canonical harness: isolated `$HOME` and a refused
/// Mixpanel endpoint, so a telemetry-enabled build cannot touch the real home
/// dir or POST to the live endpoint.
fn run(args: &[&str], home: &std::path::Path) -> (ExitStatus, String, String) {
    run_cli(Opts::new(args).in_home(home))
}

/// A real `.rl` bundle holding one `:Person` node, as bytes — the shared
/// `person_bundle` fixture, read to serve on the stub. The same single-file
/// format a caller would publish.
fn bundle_bytes(home: &std::path::Path) -> Vec<u8> {
    std::fs::read(person_bundle(home)).expect("read the cloned bundle")
}

/// Serve `bytes` once on loopback and return the `http://127.0.0.1:PORT`
/// base. The server thread is never joined — the fixture's accept deadline
/// reaps a stub the CLI never contacts, so a regression that skips the fetch
/// cannot hang the test.
fn serve_bundle(bytes: Vec<u8>) -> String {
    let (_fixture, port) = spawn_single_response_bytes_capturing("200 OK", bytes);
    format!("http://127.0.0.1:{port}")
}

#[test]
fn a_remote_load_into_a_path_target_materializes_the_store_there() {
    let home = tempfile::tempdir().unwrap();
    let base = serve_bundle(bundle_bytes(home.path()));
    let target = home.path().join("materialized.rl");

    // The load creates the store at the caller's chosen path and the run
    // opens it writable — its writes land in the materialized store.
    let (status, stdout, stderr) = run(
        &[
            "-json",
            "--database",
            target.to_str().unwrap(),
            "--load",
            &format!("{base}/store.rl"),
            "-c",
            "CREATE (n:Fresh {id: 7}); MATCH (n) RETURN count(n) AS total",
        ],
        home.path(),
    );
    assert!(status.success(), "stderr: {stderr}");
    assert!(stdout.contains("2"), "the write landed: {stdout}");
    assert!(
        stderr.contains(&format!("--load {base}/store.rl: downloaded to",))
            && stderr.contains(&format!(
                "loaded into {} (the copy is removed after the clone)",
                target.display()
            )),
        "the load is announced, naming the scratch it removed: {stderr}"
    );
    assert!(target.is_file(), "the materialized store exists");

    // The fetched scratch — the one the announcement names — is gone after
    // the clone: nothing is kept, the materialized store is the only copy.
    let staged = staged_path(&stderr);
    assert!(
        !staged.exists(),
        "the fetched scratch must be removed after the clone: {}",
        staged.display()
    );

    // The store is a real one: it opens and answers on its own.
    let (status, stdout, stderr) = run(
        &[
            "-json",
            "--database",
            target.to_str().unwrap(),
            "-c",
            "MATCH (n) RETURN count(n) AS total",
        ],
        home.path(),
    );
    assert!(status.success(), "stderr: {stderr}");
    assert!(stdout.contains("2"), "the writes survived: {stdout}");
}

#[test]
fn an_http_error_status_is_a_runtime_error() {
    let home = tempfile::tempdir().unwrap();
    let (_fixture, port) = spawn_single_response_bytes_capturing("404 Not Found", b"no".to_vec());
    let (status, _stdout, stderr) = run(
        &[
            "--load",
            &format!("http://127.0.0.1:{port}/missing.rl"),
            "-c",
            "RETURN 1",
        ],
        home.path(),
    );
    assert_eq!(status.code(), Some(1), "stderr: {stderr}");
    assert!(stderr.contains("HTTP 404"), "stderr: {stderr}");
}

#[test]
fn help_lists_the_remote_load_capability() {
    let home = tempfile::tempdir().unwrap();
    let (status, stdout, _stderr) = run(&["--help"], home.path());
    assert!(status.success());
    assert!(
        stdout.contains("--load SOURCE") && stdout.contains("rewritten to its raw form"),
        "--help must document the remote --load source form (AGENTS.md: help \
         text is the source of truth):\n{stdout}"
    );
    // And the target axis is stated where --database is listed.
    assert!(
        stdout.contains("never a URL"),
        "--help must keep --database the target axis:\n{stdout}"
    );
}
