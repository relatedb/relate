//! End-to-end `relate serve --durability-mode` (ADR-0470): every
//! durability mode (ADR-0077) is selectable through the product binary —
//! the flag and the `RELATE_DURABILITY_MODE` variable both start a
//! serving process that answers a write and a read over the HTTP surface,
//! a bad flag value is a usage error (exit 2), and a bad variable value
//! is a loud startup failure (exit 2) naming the variable. The modes'
//! engine behavior (the barrier/checkpoint postures) is the engine
//! crate's own suites; this suite pins the serve wiring.
//!
//! Compiled only when both surface features are in the build (the `full`
//! bundle); the not-included behavior of leaner bundles is pinned in
//! `e2e_bundles.rs`.

#![cfg(all(feature = "http", feature = "mcp"))]

use crate::support;

use relate_test_support::durability::DURABILITY_MODES;
use relate_test_support::loopback::{exchange, post};
use support::serve::{
    auth_header, free_port, graph_db, spawn_serve_with_env, wait_for_ports, AUTH,
};
use support::{run_cli, tempdir, CliRunOptions};

/// One `serve` run's mode selection, driven through one spawn + one
/// write + one read: the process comes up under the selection, commits a
/// write, and serves the read back — the product-binary half of the
/// mode's contract (the engine half is the engine crate's suites).
fn serve_answers_under(db: &str, extra: &[&str], envs: &[(&str, &str)], what: &str) {
    let http_port = free_port();
    let mcp_port = free_port();
    let child = spawn_serve_with_env(db, http_port, mcp_port, Some(AUTH), extra, envs);
    wait_for_ports(&[http_port, mcp_port]);

    let (status, body) = exchange(
        http_port,
        post(
            "/db/neo4j/query/v2",
            r#"{"statement": "CREATE (:Served {kind: 1})"}"#,
            Some(&auth_header()),
        )
        .as_bytes(),
    );
    assert_eq!(status, 200, "{what}: the write must commit:\n{body}");
    let (status, body) = exchange(
        http_port,
        post(
            "/db/neo4j/query/v2",
            r#"{"statement": "MATCH (n:Served) RETURN count(n) AS c"}"#,
            Some(&auth_header()),
        )
        .as_bytes(),
    );
    assert_eq!(status, 200, "{what}: the read must serve:\n{body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        json["data"]["values"][0][0].as_i64(),
        Some(1),
        "{what}: the committed write must be visible:\n{body}"
    );
    child.stop();
}

/// Runs `serve` once per durability mode, each against its own fresh
/// store path — the one per-mode loop both selection channels share.
/// One fresh store per mode: the previous iteration's committed write
/// must not leak into this one's count.
fn for_each_mode_on_a_fresh_store(mut serve: impl FnMut(&str, &str)) {
    let dir = tempdir();
    for &(mode, _) in DURABILITY_MODES {
        let db = dir.path().join(format!("graph-{mode}"));
        serve(db.to_str().unwrap(), mode);
    }
}

/// Every mode is selectable through the serve flag (ADR-0470): the
/// process starts, commits, and serves under `full`, `everysecond`, and
/// `snapshot` alike.
#[test]
fn test_each_durability_mode_is_selectable_through_the_serve_flag() {
    for_each_mode_on_a_fresh_store(|db, mode| {
        serve_answers_under(
            db,
            &["--durability-mode", mode],
            &[],
            &format!("--durability-mode {mode}"),
        );
    });
}

/// Every mode is selectable through `RELATE_DURABILITY_MODE` alone
/// (ADR-0470) — the channel a harness or supervisor sets.
#[test]
fn test_each_durability_mode_is_selectable_through_the_env() {
    for_each_mode_on_a_fresh_store(|db, mode| {
        serve_answers_under(
            db,
            &[],
            &[("RELATE_DURABILITY_MODE", mode)],
            &format!("RELATE_DURABILITY_MODE={mode}"),
        );
    });
}

/// A bad flag value is a usage error (exit 2) naming the flag and the
/// mode vocabulary — the query path's message shape, never a silent
/// default.
#[test]
fn test_bad_serve_durability_mode_flag_is_a_usage_error() {
    let home = tempdir();
    let (status, _stdout, stderr) = run_cli(
        CliRunOptions::new(&[
            "serve",
            "--database",
            ":memory:",
            "--http",
            "0",
            "--durability-mode",
            "later",
        ])
        .in_home(home.path()),
    );
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("--durability-mode"), "{stderr}");
    assert!(
        stderr.contains("expected full, everysecond, or snapshot"),
        "{stderr}"
    );
}

/// A bad `RELATE_DURABILITY_MODE` under a serve run is a loud
/// startup failure (exit 2, the storage knobs' usage-error code) naming
/// the variable — never a silently ignored value.
#[test]
fn test_bad_durability_mode_env_fails_serve_startup() {
    let dir = tempdir();
    let db = graph_db(&dir);
    let http_port = free_port();
    let mcp_port = free_port();
    let mut child = spawn_serve_with_env(
        &db,
        http_port,
        mcp_port,
        Some(AUTH),
        &[],
        &[("RELATE_DURABILITY_MODE", "everysecnd")],
    );
    let (status, stderr) = child.wait_and_drain_stderr();
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("invalid RELATE_DURABILITY_MODE"),
        "{stderr}"
    );
    assert!(
        stderr.contains("expected full, everysecond, or snapshot"),
        "{stderr}"
    );
}
