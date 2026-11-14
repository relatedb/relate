//! End-to-end: a commit record over the per-commit WAL payload cap
//! (ADR-0086, typed by ADR-0494) is refused with the capacity family's
//! caller-facing error on every surface — never the generic
//! `UnknownError: internal error` — and the message names the cap and the
//! remedy. The same refusal is driven through the Bolt explicit
//! transaction's COMMIT, the HTTP managed transaction's commit endpoint, and
//! the CLI's `query` path.

#![cfg(all(feature = "http", feature = "mcp", feature = "bolt"))]

mod support;

use relate_bolt::packstream::Pack;
use relate_test_support::loopback::{exchange, post};
use support::bolt::{connect_and_hello, failure_map, read_msg, send_struct, success_map};
use support::serve::{auth_header, free_port, spawn_serve_with, wait_for_ports, AUTH};
use support::{run_cli, tempdir, CliRunOptions};

/// A write whose commit record crosses the 64 MiB cap: one 64 MiB string
/// built in-engine (26 doublings of one byte), so the statement needs no
/// large wire message and stays under the 256 MiB expression cap.
const OVER_CAP_WRITE: &str = "CREATE (:Big {p: reduce(s = 'x', i IN range(1, 26) | s + s)})";

const CAPACITY_CODE: &str = "Neo.ClientError.General.TransactionOutOfMemoryError";

/// The text every surface carries: the cap in its stated unit and the
/// batching remedy.
fn assert_names_the_cap_and_the_remedy(message: &str) {
    assert!(message.contains("64 MiB"), "{message}");
    assert!(message.contains("smaller transactions"), "{message}");
    assert!(message.contains("IN TRANSACTIONS"), "{message}");
}

#[test]
fn test_bolt_explicit_commit_over_the_cap_is_a_capacity_failure() {
    let dir = tempdir();
    let db = dir.path().join("graph").to_str().unwrap().to_string();
    let (http_port, mcp_port, bolt_port) = (free_port(), free_port(), free_port());
    let bolt_arg = bolt_port.to_string();
    let child = spawn_serve_with(&db, http_port, mcp_port, Some(AUTH), &["--bolt", &bolt_arg]);
    wait_for_ports(&[http_port, mcp_port, bolt_port]);

    let mut client = connect_and_hello(bolt_port, AUTH);
    success_map(read_msg(&mut client));
    send_struct(&mut client, 0x11, vec![Pack::Map(vec![])]); // BEGIN
    success_map(read_msg(&mut client));
    send_struct(
        &mut client,
        0x10, // RUN
        vec![
            Pack::String(OVER_CAP_WRITE.into()),
            Pack::Map(vec![]),
            Pack::Map(vec![]),
        ],
    );
    success_map(read_msg(&mut client));
    send_struct(&mut client, 0x3F, vec![Pack::Map(vec![])]); // PULL
    success_map(read_msg(&mut client));
    // The refusal arrives at COMMIT, after the whole transaction streamed.
    send_struct(&mut client, 0x12, vec![]); // COMMIT
    let failure = Pack::Map(failure_map(read_msg(&mut client)));
    let code = failure.get("code").and_then(Pack::as_str).unwrap();
    assert_eq!(code, CAPACITY_CODE);
    assert_ne!(code, "Neo.DatabaseError.General.UnknownError");
    assert_names_the_cap_and_the_remedy(failure.get("message").and_then(Pack::as_str).unwrap());

    child.stop();
}

#[test]
fn test_http_transaction_commit_over_the_cap_is_a_capacity_error() {
    let dir = tempdir();
    let db = dir.path().join("graph").to_str().unwrap().to_string();
    let (http_port, mcp_port) = (free_port(), free_port());
    let child = spawn_serve_with(&db, http_port, mcp_port, Some(AUTH), &[]);
    wait_for_ports(&[http_port, mcp_port]);

    let (status, body) = exchange(
        http_port,
        post("/db/neo4j/query/v2/tx", "{}", Some(&auth_header())).as_bytes(),
    );
    assert_eq!(status, 201, "{body}");
    let begin: serde_json::Value = serde_json::from_str(&body).unwrap();
    let path = format!(
        "/db/neo4j/query/v2/tx/{}",
        begin["transaction"]["id"].as_str().unwrap()
    );
    let run = serde_json::json!({ "statement": OVER_CAP_WRITE }).to_string();
    let (status, body) = exchange(
        http_port,
        post(&path, &run, Some(&auth_header())).as_bytes(),
    );
    assert_eq!(status, 200, "{body}");
    let (status, body) = exchange(
        http_port,
        post(&format!("{path}/commit"), "", Some(&auth_header())).as_bytes(),
    );
    // The HTTP surface carries the capacity class as status 507 with the
    // class-less general code `42000` (docs/api/http/spec.md), not the
    // genericized 500.
    assert_eq!(status, 507, "{body}");
    let error: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(error["errors"][0]["code"], "42000", "{body}");
    assert_names_the_cap_and_the_remedy(error["errors"][0]["message"].as_str().unwrap());

    child.stop();
}

#[test]
fn test_cli_query_over_the_cap_is_a_capacity_error() {
    let home = tempdir();
    let db = home.path().join("graph");
    let db = db.to_str().unwrap();
    let (status, _stdout, stderr) = run_cli(
        CliRunOptions::new(&["query", "--database", db, OVER_CAP_WRITE]).in_home(home.path()),
    );
    assert!(!status.success(), "{stderr}");
    assert!(!stderr.contains("internal error"), "{stderr}");
    assert_names_the_cap_and_the_remedy(&stderr);
}
