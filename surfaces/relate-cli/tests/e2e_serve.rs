//! End-to-end `relate serve` (ADR-0232): one store, both network
//! surfaces, one auth story. Drives the built binary: raw HTTP against the
//! Query API v2 endpoint and JSON-RPC `tools/call` against the MCP
//! streamable-HTTP transport, asserting the shared credential behavior
//! (401 without, success with) on both, plus the usage error when no
//! surface flag is given and clean SIGINT shutdown.
//!
//! Compiled only when both surface features are in the build (the `full`
//! bundle); the not-included behavior of leaner bundles is pinned in
//! `e2e_bundles.rs`.

#![cfg(all(feature = "http", feature = "mcp"))]

mod support;

use std::time::{Duration, Instant};

use base64::Engine;
use relate_test_support::loopback::{exchange, post};
use support::serve::{auth_header, free_port, spawn_serve, spawn_serve_with, wait_for_ports, AUTH};
use support::{run_cli, tempdir, CliRunOptions};

#[test]
fn test_serve_with_no_surface_flag_is_a_usage_error() {
    let home = tempdir();
    let (status, _stdout, stderr) =
        run_cli(CliRunOptions::new(&["serve", "--database", ":memory:"]).in_home(home.path()));
    assert_eq!(status.code(), Some(2));
    assert!(stderr.contains("nothing to serve"), "{stderr}");
    assert!(stderr.contains("--http PORT"), "{stderr}");
    assert!(
        stderr.contains("Usage: relate serve --database PATH"),
        "{stderr}"
    );
}

#[test]
fn test_serve_requires_database() {
    let home = tempdir();
    let (status, _stdout, stderr) =
        run_cli(CliRunOptions::new(&["serve", "--http", "7474"]).in_home(home.path()));
    assert_eq!(status.code(), Some(2));
    assert!(stderr.contains("serve requires --database"), "{stderr}");
}

/// Drive one statement through each surface and assert the shared
/// credential behavior: 401 without the credential on both, success with
/// it on both.
#[test]
fn test_both_surfaces_share_one_credential() {
    let dir = tempdir();
    let db = dir.path().join("graph").to_str().unwrap().to_string();
    let http_port = free_port();
    let mcp_port = free_port();
    let child = spawn_serve(&db, http_port, mcp_port, Some(AUTH));
    wait_for_ports(&[http_port, mcp_port]);

    // The discovery document is public (no credential), even from a
    // browser-Origin probe.
    let (status, body) = exchange(
        http_port,
        b"GET / HTTP/1.1
host: localhost
origin: https://browser.neo4j.io

",
    );
    assert_eq!(status, 200, "{body}");
    let doc: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        doc["query"]
            .as_str()
            .unwrap()
            .ends_with("/db/{databaseName}/query/v2"),
        "{doc}"
    );
    assert_eq!(doc["neo4j_edition"], "community");

    // HTTP Query API v2: no credential → 401; the credential → 200 + rows.
    let (status, body) = exchange(
        http_port,
        post(
            "/db/neo4j/query/v2",
            r#"{"statement": "RETURN 1 AS n"}"#,
            None,
        )
        .as_bytes(),
    );
    assert_eq!(status, 401, "{body}");
    let (status, body) = exchange(
        http_port,
        post(
            "/db/neo4j/query/v2",
            r#"{"statement": "RETURN 1 AS n"}"#,
            Some(&auth_header()),
        )
        .as_bytes(),
    );
    assert_eq!(status, 200, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["data"]["values"], serde_json::json!([[1]]));

    // MCP over streamable HTTP: tools/call read_cypher, same auth story.
    let mcp_call = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"read_cypher","arguments":{"statement":"RETURN 2 AS m"}}}"#;
    let (status, body) = exchange(mcp_port, post("/", mcp_call, None).as_bytes());
    assert_eq!(status, 401, "{body}");
    let (status, body) = exchange(
        mcp_port,
        post("/", mcp_call, Some(&auth_header())).as_bytes(),
    );
    assert_eq!(status, 200, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let text = json["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("\"m\": 2") || text.contains("\"m\":2"),
        "{text}"
    );

    child.stop();
}

/// The Browser connectivity flow (mirroring upx/features/query e2e):
/// discovery → a probe without credentials gets the challenge → `CALL
/// db.ping()` with credentials answers the `success` row — repeatedly
/// (reused connections) — in the negotiated v2 media type.
#[test]
fn test_browser_style_discovery_then_ping_flow() {
    let dir = tempdir();
    let db = dir.path().join("graph").to_str().unwrap().to_string();
    let http_port = free_port();
    let mcp_port = free_port();
    let child = spawn_serve(&db, http_port, mcp_port, Some(AUTH));
    wait_for_ports(&[http_port, mcp_port]);

    // Discovery is public and advertises no SSO (Browser skips its OIDC
    // probe).
    let (status, body) = exchange(
        http_port,
        b"GET / HTTP/1.1
host: localhost

",
    );
    assert_eq!(status, 200, "{body}");
    let doc: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(doc["query"]
        .as_str()
        .unwrap()
        .contains("/db/{databaseName}/query/v2"));
    assert_eq!(doc["auth_config"]["oidc_providers"], serde_json::json!([]));

    // The probe without credentials is the 401 challenge…
    let ping = r#"{"statement":"CALL db.ping()","includeCounters":true,"accessMode":"READ"}"#;
    let (status, _) = exchange(http_port, post("/db/neo4j/query/v2", ping, None).as_bytes());
    assert_eq!(status, 401);
    // … and with credentials it answers, repeatedly.
    for _ in 0..2 {
        let (status, body) = exchange(
            http_port,
            post("/db/neo4j/query/v2", ping, Some(&auth_header())).as_bytes(),
        );
        assert_eq!(status, 200, "{body}");
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["data"]["fields"], serde_json::json!(["success"]));
        assert_eq!(json["data"]["values"], serde_json::json!([[true]]));
    }
    child.stop();
}

/// The managed-transaction flow (ADR-0237), Browser's shape: BEGIN on the
/// system namespace with just `accessMode`, a statement on the tx handle,
/// COMMIT — then the handle is gone (404).
#[test]
fn test_tx_begin_run_commit_flow() {
    let dir = tempdir();
    let db = dir.path().join("graph").to_str().unwrap().to_string();
    let http_port = free_port();
    let mcp_port = free_port();
    let child = spawn_serve(&db, http_port, mcp_port, Some(AUTH));
    wait_for_ports(&[http_port, mcp_port]);

    let (status, body) = exchange(
        http_port,
        post(
            "/db/system/query/v2/tx",
            r#"{"accessMode":"READ"}"#,
            Some(&auth_header()),
        )
        .as_bytes(),
    );
    assert_eq!(status, 201, "{body}");
    let begin: serde_json::Value = serde_json::from_str(&body).unwrap();
    let id = begin["transaction"]["id"].as_str().unwrap();
    assert!(begin["transaction"]["expires"].is_string());

    let run_path = format!("/db/system/query/v2/tx/{id}");
    let (status, body) = exchange(
        http_port,
        post(
            &run_path,
            r#"{"statement":"CALL db.ping()"}"#,
            Some(&auth_header()),
        )
        .as_bytes(),
    );
    assert_eq!(status, 200, "{body}");
    let run: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(run["data"]["values"], serde_json::json!([[true]]));

    let (status, body) = exchange(
        http_port,
        post(&format!("{run_path}/commit"), "", Some(&auth_header())).as_bytes(),
    );
    assert_eq!(status, 200, "{body}");
    let (status, _) = exchange(
        http_port,
        post(&run_path, "{}", Some(&auth_header())).as_bytes(),
    );
    assert_eq!(status, 404);
    child.stop();
}

/// The managed-transaction cap (ADR-0435 §2/§4) end to end:
/// `--max-open-transactions 1` reaches the serve-side registry — the first
/// BEGIN registers, a second BEGIN while the first is open is refused with
/// `429`/`42N91`,
/// and once the first commits the slot comes back. This is the wiring proof
/// the registry unit tests cannot give: the flag, `serve`'s resolution, and
/// the registry that enforces it are one path.
#[test]
fn test_max_open_transactions_flag_bounds_the_registry() {
    let dir = tempdir();
    let db = dir.path().join("graph").to_str().unwrap().to_string();
    let http_port = free_port();
    let mcp_port = free_port();
    let child = spawn_serve_with(
        &db,
        http_port,
        mcp_port,
        Some(AUTH),
        &["--max-open-transactions", "1"],
    );
    wait_for_ports(&[http_port, mcp_port]);

    let begin_path = "/db/neo4j/query/v2/tx";
    let begin = |body: &str| {
        exchange(
            http_port,
            post(begin_path, body, Some(&auth_header())).as_bytes(),
        )
    };

    // The first BEGIN takes the one slot.
    let (status, body) = begin(r#"{"accessMode":"READ"}"#);
    assert_eq!(status, 201, "{body}");
    let begun: serde_json::Value = serde_json::from_str(&body).unwrap();
    let id = begun["transaction"]["id"].as_str().unwrap().to_string();

    // A second BEGIN while the first is open is over the cap: 429 with the
    // typed code.
    let (status, body) = begin("{}");
    assert_eq!(status, 429, "{body}");
    let refused: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(refused["errors"][0]["code"], serde_json::json!("42N91"));

    // Closing the open transaction returns the slot, so the same flag
    // admits the next BEGIN.
    let (status, body) = exchange(
        http_port,
        post(
            &format!("/db/neo4j/query/v2/tx/{id}/commit"),
            "",
            Some(&auth_header()),
        )
        .as_bytes(),
    );
    assert_eq!(status, 200, "{body}");
    let (status, body) = begin("{}");
    assert_eq!(status, 201, "the committed slot is reusable: {body}");
    child.stop();
}

/// The generated-token path: no flag, no env — the server prints the token
/// story once on stderr and persists `<dir>.auth-token`; that token unlocks
/// both surfaces.
#[test]
fn test_generated_token_unlocks_both_surfaces() {
    let dir = tempdir();
    let db = dir.path().join("graph").to_str().unwrap().to_string();
    let http_port = free_port();
    let mcp_port = free_port();
    let child = spawn_serve(&db, http_port, mcp_port, None);
    wait_for_ports(&[http_port, mcp_port]);

    // The persisted token file is the credential for both surfaces; its
    // path is relate_auth's one fact, never re-derived here.
    let token_path = relate_auth::token_path(&db)
        .expect("the temp store path is a local URI")
        .expect("a persistent store has a token path");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !token_path.exists() {
        assert!(Instant::now() < deadline, "token file never appeared");
        std::thread::sleep(Duration::from_millis(50));
    }
    let token = std::fs::read_to_string(&token_path).unwrap();
    let (user, _) = token.trim().split_once(':').unwrap();
    assert_eq!(user, "relate");
    let blob = base64::engine::general_purpose::STANDARD.encode(token.trim());
    let header = format!("Basic {blob}");

    let (status, body) = exchange(
        http_port,
        post(
            "/db/neo4j/query/v2",
            r#"{"statement": "RETURN 1 AS n"}"#,
            Some(&header),
        )
        .as_bytes(),
    );
    assert_eq!(status, 200, "{body}");
    let mcp_call = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#;
    let (status, body) = exchange(mcp_port, post("/", mcp_call, Some(&header)).as_bytes());
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("read_cypher"), "{body}");

    child.stop();
}

/// SIGINT shuts the server down cleanly: exit 0 after the shutdown notice
/// and the final checkpoint (the store reopens with its writes intact).
#[cfg(unix)]
#[test]
fn test_sigint_shuts_both_surfaces_down_cleanly() {
    let dir = tempdir();
    let db = dir.path().join("graph").to_str().unwrap().to_string();
    let http_port = free_port();
    let mcp_port = free_port();
    let mut child = spawn_serve(&db, http_port, mcp_port, Some(AUTH));
    wait_for_ports(&[http_port, mcp_port]);

    // A write through the MCP surface, so the checkpoint has something to do.
    let mcp_call = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"write_cypher","arguments":{"statement":"CREATE (:Marker {v: 1})"}}}"#;
    let (status, body) = exchange(
        mcp_port,
        post("/", mcp_call, Some(&auth_header())).as_bytes(),
    );
    assert_eq!(status, 200, "{body}");

    let (status, stderr) = child.sigint_and_drain_stderr();
    assert!(status.success(), "serve exit on SIGINT: {status}");
    assert!(stderr.contains("serve: shutting down"), "{stderr}");
    assert!(
        stderr.contains("auth: basic (from RELATE_AUTH)"),
        "{stderr}"
    );

    // The store survived: a fresh query run sees the write.
    let home = tempdir();
    let (status, _stdout, stderr) = run_cli(
        CliRunOptions::new(&["RETURN 1 AS n", "--database", db.as_str()]).in_home(home.path()),
    );
    assert!(status.success(), "reopen failed: {stderr}");
}

/// `--bolt PORT`: a real Bolt session over TCP against `serve` — the shared
/// credential authenticates the HELLO, RUN+PULL returns the row, and the
/// HTTP discovery document advertises the endpoint.
#[cfg(feature = "bolt")]
#[test]
fn test_bolt_surface_serves_a_session_and_is_advertised() {
    use support::bolt::{
        assert_run_pull_returns_one_row, connect_and_hello, read_msg, success_map,
    };

    let dir = tempdir();
    let db = dir.path().join("graph").to_str().unwrap().to_string();
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
#[cfg(feature = "bolt")]
#[test]
fn test_bolt_port_zero_advertises_the_bound_port() {
    use support::bolt::{
        assert_run_pull_returns_one_row, connect_and_hello, failure_map, field, read_msg,
        success_map,
    };

    let dir = tempdir();
    let db = dir.path().join("graph").to_str().unwrap().to_string();
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

/// `--bolt-identity` (ADR-0293): the HELLO SUCCESS `server` metadata
/// carries the configured agent string verbatim — the opt-in shape for
/// tooling (cypher-shell) that hard-checks the identity.
#[cfg(feature = "bolt")]
#[test]
fn test_bolt_identity_flag_is_the_hello_server_metadata() {
    use relate_bolt::packstream::Pack;
    use support::bolt::{connect_and_hello, read_msg, success_map};

    let dir = tempdir();
    let db = dir.path().join("graph").to_str().unwrap().to_string();
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
    // Metadata reads go through the canonical Pack::get accessor.
    let metadata = Pack::Map(success_map(read_msg(&mut client)));
    assert_eq!(
        metadata.get("server").and_then(Pack::as_str),
        Some("Neo4j/5.26.0")
    );
    // `connection_id` rides beside it (ADR-0293) — the Java driver
    // requires the key.
    let connection_id = metadata.get("connection_id").and_then(Pack::as_str);
    assert!(
        matches!(connection_id, Some(id) if id.starts_with("conn-")),
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
#[cfg(all(unix, feature = "bolt"))]
#[test]
fn test_malformed_identity_fails_before_auth_persists_the_token() {
    let dir = tempdir();
    let db = dir.path().join("graph").to_str().unwrap().to_string();
    let mut child = support::serve::spawn_serve_with_malformed_identity(&db);
    let (status, stderr) = child.wait_and_drain_stderr();
    assert_eq!(status.code(), Some(1), "serve must exit 1: {stderr}");
    assert!(
        stderr.contains("RELATE_IDENTITY is not valid UTF-8"),
        "the malformed identity must be the reported failure: {stderr}"
    );
    let token = relate_auth::token_path(&db)
        .expect("the temp store path is a local URI")
        .expect("a persistent store has a token path");
    assert!(
        !token.exists(),
        "a malformed identity must fail before auth persists {}",
        token.display()
    );
}

/// The setup-ordering invariant, the malformed-identity shape mirrored for
/// the connection cap (ADR-0333): the `RELATE_MAX_CONNECTIONS` read —
/// now inside setup, before auth — fails startup with the same
/// `Error: ...` text and exit 1 and, because auth must not have run,
/// leaves no token file behind. The malformed value is set on the child's
/// environment only (never the test process), so the pin is race-free
/// under the parallel harness.
#[cfg(all(unix, feature = "bolt"))]
#[test]
fn test_malformed_max_connections_fails_before_auth_persists_the_token() {
    let dir = tempdir();
    let db = dir.path().join("graph").to_str().unwrap().to_string();
    let mut child = support::serve::spawn_serve_with_malformed_max_connections(&db);
    let (status, stderr) = child.wait_and_drain_stderr();
    assert_eq!(status.code(), Some(1), "serve must exit 1: {stderr}");
    assert!(
        stderr.contains("Error: invalid RELATE_MAX_CONNECTIONS"),
        "the malformed max-connections must be the reported failure: {stderr}"
    );
    let token = relate_auth::token_path(&db)
        .expect("the temp store path is a local URI")
        .expect("a persistent store has a token path");
    assert!(
        !token.exists(),
        "a malformed max-connections must fail before auth persists {}",
        token.display()
    );
}

/// The setup-ordering invariant for the admission knobs on a stdio-only
/// run: they resolve before the `--mcp stdio` early return, so a malformed
/// deadline or cap variable still fails startup with exit 1 and the loud
/// `Error: invalid <var>` text. The value is set on the child's
/// environment only (never the test process), so the pin is race-free
/// under the parallel harness.
#[cfg(unix)]
#[test]
fn test_stdio_only_serve_still_validates_the_admission_knob_envs() {
    for var in [
        "RELATE_HEADER_DEADLINE_SECS",
        "RELATE_BODY_DEADLINE_SECS",
        "RELATE_MAX_CONNECTIONS",
    ] {
        let mut child = support::serve::spawn_stdio_with_malformed_env(var);
        let (status, stderr) = child.wait_and_drain_stderr();
        assert_eq!(status.code(), Some(1), "{var}: {stderr}");
        assert!(
            stderr.contains(&format!("Error: invalid {var}")),
            "{var}: the malformed value must be the reported failure: {stderr}"
        );
    }
}

/// The setup-ordering invariant for the ADR-0435 managed-transaction
/// limits, the `RELATE_MAX_CONNECTIONS` shape mirrored: a malformed
/// `RELATE_TX_IDLE_TIMEOUT_SECS` fails startup with the same
/// `Error: ...` text and exit 1 and, because the limits resolve before
/// auth, leaves no token file behind.
#[cfg(all(unix, feature = "http"))]
#[test]
fn test_malformed_tx_idle_timeout_fails_before_auth_persists_the_token() {
    let dir = tempdir();
    let db = dir.path().join("graph").to_str().unwrap().to_string();
    let mut child = support::serve::spawn_serve_with_malformed_tx_idle_timeout(&db);
    let (status, stderr) = child.wait_and_drain_stderr();
    assert_eq!(status.code(), Some(1), "serve must exit 1: {stderr}");
    assert!(
        stderr.contains("Error: invalid RELATE_TX_IDLE_TIMEOUT_SECS"),
        "the malformed tx idle timeout must be the reported failure: {stderr}"
    );
    let token = relate_auth::token_path(&db)
        .expect("the temp store path is a local URI")
        .expect("a persistent store has a token path");
    assert!(
        !token.exists(),
        "a malformed tx idle timeout must fail before auth persists {}",
        token.display()
    );
}

/// `--host` (ADR-0248): the default loopback binds stay silent; an
/// explicit non-loopback host serves (reachable via loopback too) and
/// prints the loud plaintext WARNING on stderr.
#[cfg(unix)]
#[test]
fn test_host_flag_binds_and_warns_loudly() {
    // Default: loopback, no warning.
    let dir = tempdir();
    let db = dir.path().join("graph").to_str().unwrap().to_string();
    let http_port = free_port();
    let mcp_port = free_port();
    let mut child = spawn_serve(&db, http_port, mcp_port, Some(AUTH));
    wait_for_ports(&[http_port, mcp_port]);
    let (_, stderr) = child.sigint_and_drain_stderr();
    assert!(!stderr.contains("WARNING"), "{stderr}");

    // Explicit 0.0.0.0: the loud plaintext posture warning, and the
    // surfaces still answer (via loopback here).
    let dir = tempdir();
    let db = dir.path().join("graph").to_str().unwrap().to_string();
    let http_port = free_port();
    let mcp_port = free_port();
    let mut child = spawn_serve_with(&db, http_port, mcp_port, Some(AUTH), &["--host", "0.0.0.0"]);
    wait_for_ports(&[http_port, mcp_port]);
    let (status, body) = exchange(
        http_port,
        post(
            "/db/neo4j/query/v2",
            r#"{"statement": "RETURN 1 AS n"}"#,
            Some(&auth_header()),
        )
        .as_bytes(),
    );
    assert_eq!(status, 200, "{body}");
    let (_, stderr) = child.sigint_and_drain_stderr();
    assert!(stderr.contains("WARNING"), "{stderr}");
    assert!(stderr.contains("PLAINTEXT"), "{stderr}");
    assert!(stderr.contains("0.0.0.0"), "{stderr}");
    assert!(stderr.contains("http://0.0.0.0:"), "{stderr}");
}

/// `--mcp stdio` (LABS-381, ADR-0368): the foreground stdio transport on the
/// process's own stdin/stdout — the product path MCP hosts spawn. stdout carries only
/// JSON-RPC lines (every banner goes to stderr), and closing stdin ends the
/// run cleanly with exit 0.
#[cfg(unix)]
#[test]
fn test_mcp_stdio_serves_json_rpc_until_stdin_closes() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;

    let mut child = support::cli_command()
        .args(["serve", "--mcp", "stdio", "--database", ":memory:"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn relate serve --mcp stdio");
    let mut stdin = child.stdin.take().expect("stdin pipe");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout pipe"));

    let ask = |stdin: &mut std::process::ChildStdin,
               stdout: &mut BufReader<std::process::ChildStdout>,
               request: &str|
     -> serde_json::Value {
        stdin.write_all(request.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        stdout.read_line(&mut line).expect("one response line");
        assert!(!line.is_empty(), "serve closed stdout");
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("response {line:?}: {e}"))
    };

    let init = ask(
        &mut stdin,
        &mut stdout,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"e2e","version":"0"}}}"#,
    );
    assert_eq!(init["result"]["serverInfo"]["name"], "relate-mcp");
    stdin
        .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
        .unwrap();
    stdin.flush().unwrap();

    let list = ask(
        &mut stdin,
        &mut stdout,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
    );
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .expect("tools/list returns an array")
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["write_cypher", "read_cypher", "get_schema"]);

    let read = ask(
        &mut stdin,
        &mut stdout,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"read_cypher","arguments":{"statement":"RETURN 1 AS one"}}}"#,
    );
    let envelope: serde_json::Value =
        serde_json::from_str(read["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(envelope["rows"], serde_json::json!([{"one": 1}]));

    // The host closes stdin: the run ends cleanly (exit 0).
    drop(stdin);
    let status = child.wait().expect("serve exits at stdin EOF");
    assert_eq!(status.code(), Some(0), "stdin EOF is a clean shutdown");
}
