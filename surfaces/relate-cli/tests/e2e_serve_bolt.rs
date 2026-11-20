//! End-to-end `relate serve` — the Bolt surface (LABS-298): one store,
//! the Bolt listener beside the HTTP/MCP pair every serve spawn binds,
//! one auth story. Pins the HTTP discovery document's `bolt_direct`
//! advertisement against the real listener (the **bound** port under
//! `--bolt 0`), the Neo4j-format startup lines (REL8-1762), the HELLO
//! identity flag (ADR-0293), and the bolt-gated setup-ordering pins (a
//! malformed identity or cap variable fails startup before auth persists
//! the token file). Split from `e2e_serve.rs` (the http/mcp auth-story
//! hub) when the bolt-gated cluster outgrew that file's charter.
//!
//! Compiled only when every surface feature is in the build (the `full`
//! bundle); the not-included behavior of leaner bundles is pinned in
//! `e2e_bundles.rs`.

#![cfg(all(feature = "http", feature = "mcp", feature = "bolt"))]

mod support;

use relate_test_support::loopback::exchange;
use support::serve::{free_port, graph_db, spawn_serve_with, wait_for_ports, AUTH};
use support::tempdir;

/// `--bolt PORT`: a real Bolt session over TCP against `serve` — the shared
/// credential authenticates the HELLO, RUN+PULL returns the row, and the
/// HTTP discovery document advertises the endpoint.
#[test]
fn test_bolt_surface_serves_a_session_and_is_advertised() {
    use support::bolt::{
        assert_run_pull_returns_one_row, connect_and_hello, read_msg, success_map,
    };

    let dir = tempdir();
    let db = graph_db(&dir);
    let http_port = free_port();
    let mcp_port = free_port();
    let bolt_port = free_port();
    let bolt_arg = bolt_port.to_string();
    let child = spawn_serve_with(&db, http_port, mcp_port, Some(AUTH), &["--bolt", &bolt_arg]);
    wait_for_ports(&[http_port, mcp_port, bolt_port]);

    // Discovery advertises the bolt endpoint.
    let (status, body) = exchange(http_port, b"GET / HTTP/1.1\r\nhost: localhost\r\n\r\n");
    assert_eq!(status, 200, "{body}");
    let doc: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        doc["bolt_direct"],
        serde_json::json!(format!("bolt://127.0.0.1:{bolt_port}"))
    );

    // A Bolt session: magic + versions, HELLO with the shared credential,
    // then RUN+PULL answers one row.
    let mut client = connect_and_hello(bolt_port, AUTH);
    success_map(read_msg(&mut client));
    assert_run_pull_returns_one_row(&mut client);

    child.stop();
}

/// `--bolt 0` (LABS-308): the discovery document's `bolt_direct` carries
/// the **bound** port — the OS-assigned ephemeral one, never the
/// requested `0` — and the advertised endpoint carries a real Bolt
/// session (HELLO → RUN → PULL → RECORD), so the advertisement and the
/// listener cannot drift: the advertised port is one a driver connects
/// to.
#[test]
fn test_bolt_port_zero_advertises_the_bound_port() {
    use support::bolt::{
        assert_run_pull_returns_one_row, connect_and_hello, failure_map, field, read_msg,
        success_map,
    };

    let dir = tempdir();
    let db = graph_db(&dir);
    let http_port = free_port();
    let mcp_port = free_port();
    let child = spawn_serve_with(&db, http_port, mcp_port, Some(AUTH), &["--bolt", "0"]);
    wait_for_ports(&[http_port, mcp_port]);

    // Discovery advertises the bound port — never the requested 0.
    let (status, body) = exchange(http_port, b"GET / HTTP/1.1\r\nhost: localhost\r\n\r\n");
    assert_eq!(status, 200, "{body}");
    let doc: serde_json::Value = serde_json::from_str(&body).unwrap();
    let advertised = doc["bolt_direct"]
        .as_str()
        .expect("the bolt endpoint is advertised");
    let port = advertised
        .strip_prefix("bolt://127.0.0.1:")
        .unwrap_or_else(|| panic!("unexpected bolt_direct shape: {advertised}"))
        .parse::<u16>()
        .expect("the advertised port parses");
    assert_ne!(
        port, 0,
        "the advertised port is the bound one: {advertised}"
    );

    // The advertised endpoint is the real listener: a full Bolt session —
    // HELLO with the shared credential, RUN, PULL — answers there, one row.
    let mut client = connect_and_hello(port, AUTH);
    success_map(read_msg(&mut client));
    assert_run_pull_returns_one_row(&mut client);

    // A wrong credential against the same advertised port is the
    // Unauthorized FAILURE (the ADR-0139 delay applies), never a session.
    let mut client = connect_and_hello(port, "neo4j:not-the-password");
    let failure = failure_map(read_msg(&mut client));
    let message = field(&failure, "message");
    assert!(message.contains("authentication failed"), "{message}");

    child.stop();
}

/// The listener startup lines are Neo4j's `xxx enabled on host:port.`
/// shape (REL8-1762): the Docker image runs `relate serve`, and
/// log-waiting tooling built for the Neo4j image (testcontainers and
/// friends) reads exactly these lines from `docker logs` — bare, no
/// `serve:` prefix, no auth suffix, trailing period, the bound port — so
/// the image works as a drop-in replacement. The MCP surface keeps the
/// `serve:` notice shape (Neo4j has no counterpart to match), and the
/// auth story still rides the banner's own lines.
#[cfg(unix)]
#[test]
fn test_startup_lines_match_the_neo4j_docker_format() {
    let dir = tempdir();
    let db = graph_db(&dir);
    let http_port = free_port();
    let mcp_port = free_port();
    let bolt_port = free_port();
    let bolt_arg = bolt_port.to_string();
    let mut child = spawn_serve_with(&db, http_port, mcp_port, Some(AUTH), &["--bolt", &bolt_arg]);
    wait_for_ports(&[http_port, mcp_port, bolt_port]);

    // SIGINT straight after the binds — the earliest a client can observe
    // the server — needs no pacing: the watcher is armed before setup
    // binds anything (REL8-1884), so a live port implies a live handler.
    let (status, stderr) = child.sigint_and_drain_stderr();
    assert!(status.success(), "serve exit on SIGINT: {status}");
    // The two lines, exactly: bare label, the default bind host, the
    // bound port, trailing period — full lines, so a port that prefixes
    // another number cannot match the wrong line.
    assert!(
        stderr.contains(&format!("HTTP enabled on 127.0.0.1:{http_port}.\n")),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!("Bolt enabled on 127.0.0.1:{bolt_port}.\n")),
        "{stderr}"
    );
    // The MCP surface keeps the serve-notice shape, and the auth story
    // still prints on the banner's own line.
    assert!(
        stderr.contains("serve: MCP (streamable HTTP) listening on"),
        "{stderr}"
    );
    assert!(
        stderr.contains("auth: basic (from RELATE_AUTH)"),
        "{stderr}"
    );
}

/// `--bolt-identity` (ADR-0293): the HELLO SUCCESS `server` metadata
/// carries the configured agent string verbatim — the opt-in shape for
/// tooling (cypher-shell) that hard-checks the identity.
#[test]
fn test_bolt_identity_flag_is_the_hello_server_metadata() {
    use support::bolt::{connect_and_hello, field, read_msg, success_map};

    let dir = tempdir();
    let db = graph_db(&dir);
    let http_port = free_port();
    let mcp_port = free_port();
    let bolt_port = free_port();
    let bolt_arg = bolt_port.to_string();
    let child = spawn_serve_with(
        &db,
        http_port,
        mcp_port,
        Some(AUTH),
        &["--bolt", &bolt_arg, "--bolt-identity", "Neo4j/5.26.0"],
    );
    wait_for_ports(&[bolt_port]);

    let mut client = connect_and_hello(bolt_port, AUTH);
    // Metadata reads go through the canonical `field` extractor.
    let metadata = success_map(read_msg(&mut client));
    assert_eq!(field(&metadata, "server"), "Neo4j/5.26.0");
    // `connection_id` rides beside it (ADR-0293) — the Java driver
    // requires the key.
    assert!(
        field(&metadata, "connection_id").starts_with("conn-"),
        "connection_id: {metadata:?}"
    );

    child.stop();
}

/// The setup-ordering invariant (F-3-1/F-3-2): the Bolt identity read runs
/// before auth resolves, so `relate_auth::resolve` — whose
/// generated-token path persists the token file — is genuinely the last
/// fallible setup step. A malformed `RELATE_IDENTITY` (non-UTF-8)
/// fails startup with exit 1 and, because auth must not have run, leaves
/// no token file behind. The malformed value is set on the child's
/// environment only (never the test process), so the pin is race-free
/// under the parallel harness.
#[cfg(unix)]
#[test]
fn test_malformed_identity_fails_before_auth_persists_the_token() {
    let dir = tempdir();
    support::serve::malformed_env_fails_before_auth(
        &support::serve::graph_db(&dir),
        support::serve::spawn_serve_with_malformed_identity,
        "RELATE_IDENTITY is not valid UTF-8",
        "identity",
    );
}

/// The setup-ordering invariant, the malformed-identity shape mirrored for
/// the connection cap (ADR-0333): the `RELATE_MAX_CONNECTIONS` read —
/// now inside setup, before auth — fails startup with the same
/// `Error: ...` text and exit 1 and, because auth must not have run,
/// leaves no token file behind. The malformed value is set on the child's
/// environment only (never the test process), so the pin is race-free
/// under the parallel harness.
#[cfg(unix)]
#[test]
fn test_malformed_max_connections_fails_before_auth_persists_the_token() {
    let dir = tempdir();
    support::serve::malformed_env_fails_before_auth(
        &support::serve::graph_db(&dir),
        support::serve::spawn_serve_with_malformed_max_connections,
        "Error: invalid RELATE_MAX_CONNECTIONS",
        "max-connections",
    );
}
