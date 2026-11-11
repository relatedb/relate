//! End-to-end `relate serve --read-only` / `--read-only=live` (ADR-0306,
//! the ADR-0480 spelling): the
//! read-only open served over the network. A read-only serve answers reads
//! of the pre-created store, refuses writes on every surface — the HTTP
//! error envelope carries the engine's read-only error, a Bolt write RUN
//! answers a FAILURE carrying it, and the MCP surface does not list
//! `write-cypher` — holds no writer lease (a separate writer process
//! commits beside it, exit 0, while the served handle keeps the LSN it
//! opened at), and writes nothing on shutdown (no final checkpoint: the
//! manifest is byte-identical after a clean SIGINT exit). A follow serve
//! shows the writer's commits as they land. The loud failure modes: a
//! missing store is never created (exit 1), `--read-only=live` on `:memory:` is a
//! startup error (exit 1), and a pre-verb `--read-only` is a usage error
//! (exit 2) naming the post-verb spelling.
//!
//! Compiled only when both surface features are in the build (the `full`
//! bundle); the not-included behavior of leaner bundles is pinned in
//! `e2e_bundles.rs`.

#![cfg(all(feature = "http", feature = "mcp"))]

mod support;

use relate_test_support::loopback::{exchange, post};
use support::serve::{auth_header, free_port, spawn_serve_with, wait_for_ports, AUTH};
use support::{person_bundle, run_cli, tempdir, CliRunOptions};

/// Run one one-shot CLI statement against `db` (the path-first form) in an
/// isolated home and return (exit status, stdout, stderr). The read-only
/// serve never creates the store, so every test that serves one writes it
/// first with this; the follow test's writer commits beside the server the
/// same way. The caller asserts the exit status where it is the point of
/// the test.
fn one_shot(statement: &str, db: &str) -> (std::process::ExitStatus, String, String) {
    let home = tempdir();
    run_cli(CliRunOptions::new(&[statement, "--database", db]).in_home(home.path()))
}

/// Pre-create a native directory store with one one-shot writer run and
/// assert it committed: the read-only and follow serves below never create
/// a store, so each one needs one on disk first.
fn seed(statement: &str, db: &str) {
    let (status, _stdout, stderr) = one_shot(statement, db);
    assert!(status.success(), "seeding {statement:?} failed: {stderr}");
}

/// The store's node count as the HTTP surface serves it.
fn served_count(http_port: u16) -> i64 {
    let (status, body) = exchange(
        http_port,
        post(
            "/db/neo4j/query/v2",
            r#"{"statement": "MATCH (n) RETURN count(n) AS c"}"#,
            Some(&auth_header()),
        )
        .as_bytes(),
    );
    assert_eq!(status, 200, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    json["data"]["values"][0][0]
        .as_i64()
        .expect("count(n) answers one integer row")
}

/// A read-only serve answers reads of the pre-created store and refuses
/// writes on both served surfaces: the HTTP error envelope carries the
/// engine's read-only error (400), and the MCP surface does not list
/// `write-cypher` — an always-failing tool would be a dead end for hosts
/// that auto-approve `readOnlyHint` tools (ADR-0306 §3).
#[test]
fn test_read_only_serve_refuses_writes_and_hides_write_cypher() {
    let dir = tempdir();
    let db = dir.path().join("graph").to_str().unwrap().to_string();
    seed("CREATE (:Marker {v: 1})", &db);
    let http_port = free_port();
    let mcp_port = free_port();
    let child = spawn_serve_with(&db, http_port, mcp_port, Some(AUTH), &["--read-only"]);
    wait_for_ports(&[http_port, mcp_port]);

    // Reads serve the pre-created data: one node.
    assert_eq!(served_count(http_port), 1);

    // A write through the HTTP surface is refused with the engine's
    // read-only error.
    let (status, body) = exchange(
        http_port,
        post(
            "/db/neo4j/query/v2",
            r#"{"statement": "CREATE (:Refused {v: 9})"}"#,
            Some(&auth_header()),
        )
        .as_bytes(),
    );
    assert_eq!(status, 400, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let message = json["errors"][0]["message"]
        .as_str()
        .expect("the error envelope carries a message");
    assert!(message.contains("read-only"), "{message}");

    // The MCP surface on the same read-only handle lists no write_cypher.
    let tools = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#;
    let (status, body) = exchange(mcp_port, post("/", tools, Some(&auth_header())).as_bytes());
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("read_cypher"), "{body}");
    assert!(!body.contains("write_cypher"), "{body}");

    child.stop();
}

/// A read-only serve holds no writer lease: a separate one-shot writer
/// process commits beside the server and exits 0 — beside a writable
/// serve it would fail locked — while the served handle keeps the LSN it
/// opened at, so the writer's node never appears through it (ADR-0306).
#[test]
fn test_read_only_serve_serves_beside_a_writer() {
    let dir = tempdir();
    let db = dir.path().join("graph").to_str().unwrap().to_string();
    seed("CREATE (:Marker {v: 1})", &db);
    let http_port = free_port();
    let mcp_port = free_port();
    let child = spawn_serve_with(&db, http_port, mcp_port, Some(AUTH), &["--read-only"]);
    wait_for_ports(&[http_port, mcp_port]);
    assert_eq!(served_count(http_port), 1);

    // The writer beside the read-only serve commits and exits 0.
    let (status, _stdout, stderr) = one_shot("CREATE (:Marker {v: 2})", &db);
    assert!(
        status.success(),
        "the writer beside the read-only serve failed: {stderr}"
    );

    // The served handle is frozen at its open: the writer's node is not
    // visible through the read-only serve.
    assert_eq!(served_count(http_port), 1);

    child.stop();
}

/// A read-only serve shuts down cleanly on SIGINT (exit 0, the banner
/// naming the mode verbatim) and writes nothing: the manifest it read is
/// byte-identical after the server's lifetime — no final checkpoint runs
/// on a read-only handle (ADR-0306 §4, §5).
#[cfg(unix)]
#[test]
fn test_read_only_serve_sigint_is_clean_and_writes_nothing() {
    let dir = tempdir();
    let db = dir.path().join("graph").to_str().unwrap().to_string();
    seed("CREATE (:Marker {v: 1})", &db);
    let manifest = dir.path().join("graph").join("manifest.json");
    assert!(manifest.is_file(), "the seeded store carries a manifest");
    let before = std::fs::read(&manifest).unwrap();

    let http_port = free_port();
    let mcp_port = free_port();
    let mut child = spawn_serve_with(&db, http_port, mcp_port, Some(AUTH), &["--read-only"]);
    wait_for_ports(&[http_port, mcp_port]);
    assert_eq!(served_count(http_port), 1);

    let (status, stderr) = child.sigint_and_drain_stderr();
    assert!(status.success(), "serve exit on SIGINT: {status}");
    assert!(stderr.contains("serve: shutting down"), "{stderr}");
    assert!(
        stderr.contains(
            "serve: opened the store read-only — no writer lease \
             and never creates the store; every write fails"
        ),
        "{stderr}"
    );

    let after = std::fs::read(&manifest).unwrap();
    assert_eq!(before, after, "a read-only serve wrote the manifest");
}

/// A write over Bolt against a read-only serve fails at execution with the
/// engine's read-only error: the ROUTE table still lists WRITE — the
/// transport's roles, not the handle's mode — so the FAILURE's message
/// carrying `read-only` is the honest answer (ADR-0306).
#[cfg(feature = "bolt")]
#[test]
fn test_bolt_write_on_a_read_only_serve_fails() {
    use relate_bolt::packstream::Pack;
    use support::bolt::{connect_and_hello, failure_map, read_msg, send_struct, success_map};

    let dir = tempdir();
    let db = dir.path().join("graph").to_str().unwrap().to_string();
    seed("CREATE (:Marker {v: 1})", &db);
    let http_port = free_port();
    let mcp_port = free_port();
    let bolt_port = free_port();
    let bolt_arg = bolt_port.to_string();
    let child = spawn_serve_with(
        &db,
        http_port,
        mcp_port,
        Some(AUTH),
        &["--read-only", "--bolt", &bolt_arg],
    );
    wait_for_ports(&[http_port, mcp_port, bolt_port]);

    let mut client = connect_and_hello(bolt_port, AUTH);
    // The HELLO succeeds: the routing table advertises the transport's
    // roles, not the handle's mode.
    success_map(read_msg(&mut client));

    // RUN a write: the plan is accepted and the refusal — raised while
    // executing, at the auto-commit — is reported at the first PULL
    // (ADR-0319), so the read-only error arrives on the PULL, never as a
    // RECORD.
    send_struct(
        &mut client,
        0x10,
        vec![
            Pack::String("CREATE (:Refused {v: 1})".into()),
            Pack::Map(vec![]),
            Pack::Map(vec![]),
        ],
    );
    success_map(read_msg(&mut client));
    send_struct(&mut client, 0x3F, vec![Pack::Map(vec![])]);
    // Metadata reads go through the canonical Pack::get accessor.
    let failure = Pack::Map(failure_map(read_msg(&mut client)));
    let message = failure
        .get("message")
        .and_then(Pack::as_str)
        .expect("FAILURE metadata carries a message");
    assert!(message.contains("read-only"), "{message}");

    child.stop();
}

/// A follow serve shows the writer's commits: a separate one-shot writer
/// commits `(:T {v: 2})` beside the server, and the follower serves it
/// within 10 s while holding no writer lease (ADR-0306, ADR-0137). The
/// banner names the following mode verbatim.
#[cfg(unix)]
#[test]
fn test_follow_serve_shows_the_writers_commits() {
    use std::time::{Duration, Instant};

    let dir = tempdir();
    let db = dir.path().join("graph").to_str().unwrap().to_string();
    seed("CREATE (:T {v: 1})", &db);
    let http_port = free_port();
    let mcp_port = free_port();
    let mut child = spawn_serve_with(&db, http_port, mcp_port, Some(AUTH), &["--read-only=live"]);
    wait_for_ports(&[http_port, mcp_port]);

    // The writer commits beside the follower and exits 0.
    let (status, _stdout, stderr) = one_shot("CREATE (:T {v: 2})", &db);
    assert!(
        status.success(),
        "the writer beside the follow serve failed: {stderr}"
    );

    // The follower serves the writer's commit as it lands (a 50 ms WAL
    // poll plus reloads across a checkpoint gap).
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (status, body) = exchange(
            http_port,
            post(
                "/db/neo4j/query/v2",
                r#"{"statement": "MATCH (n:T {v: 2}) RETURN n.v AS v"}"#,
                Some(&auth_header()),
            )
            .as_bytes(),
        );
        assert_eq!(status, 200, "{body}");
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        if json["data"]["values"] == serde_json::json!([[2]]) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the follower never showed the writer's commit: {body}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    let (status, stderr) = child.sigint_and_drain_stderr();
    assert!(status.success(), "serve exit on SIGINT: {status}");
    assert!(stderr.contains("serve: shutting down"), "{stderr}");
    assert!(
        stderr.contains(
            "serve: opened the store read-only and following the WAL — \
             the writer's committed frames appear as they land"
        ),
        "{stderr}"
    );
}

/// A read-only serve never creates a missing store: the open is a loud
/// startup failure (exit 1) and the path is left alone — read-only creates
/// nothing (ADR-0306, ADR-0136).
#[test]
fn test_read_only_serve_never_creates_a_missing_store() {
    let dir = tempdir();
    let missing = dir.path().join("missing");
    let db = missing.to_str().unwrap().to_string();
    let http_port = free_port();
    let mcp_port = free_port();
    let mut child = spawn_serve_with(&db, http_port, mcp_port, Some(AUTH), &["--read-only"]);
    let (status, stderr) = child.wait_and_drain_stderr();
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("Error: Failed to open Relate"), "{stderr}");
    assert!(
        !missing.exists(),
        "the read-only serve created the store it refused to open"
    );
}

/// `--read-only=live` on `:memory:` is a startup error (exit 1): the follower
/// needs a native directory's WAL to tail (ADR-0306, ADR-0137).
#[test]
fn test_follow_on_memory_is_a_startup_error() {
    let http_port = free_port();
    let mcp_port = free_port();
    let mut child = spawn_serve_with(
        ":memory:",
        http_port,
        mcp_port,
        Some(AUTH),
        &["--read-only=live"],
    );
    let (status, stderr) = child.wait_and_drain_stderr();
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains(
            "live freshness (WAL follow) is only supported for native directory databases"
        ),
        "{stderr}"
    );
}

/// A pre-verb `--read-only` before `serve` is a usage error (exit 2) whose
/// message names the post-verb spelling byte-verbatim — never silently
/// ignored, because a silently-writable server is the exact hazard the
/// user meant to fence (ADR-0306 §6).
#[test]
fn test_preverb_read_only_before_serve_is_a_usage_error() {
    let home = tempdir();
    let db = home.path().join("graph");
    let (status, _stdout, stderr) = run_cli(
        CliRunOptions::new(&[
            "--read-only",
            "serve",
            "--database",
            db.to_str().unwrap(),
            "--http",
            "7474",
        ])
        .in_home(home.path()),
    );
    assert_eq!(status.code(), Some(2));
    assert!(
        stderr.contains(
            "Error: serve takes --read-only after the verb, not before it \
             (relate serve --read-only …)"
        ),
        "{stderr}"
    );
}

/// LABS-903 (ADR-0500): `--load` with the `:memory:` target is the memory
/// turn-into — serve loads the source into the in-memory store and
/// releases it: nothing holds the bundle open or locked, so the source
/// file can be deleted underneath the running server and the served store
/// does not notice, and writes are refused like a read-only serve's.
#[test]
fn test_load_serves_the_store_from_memory_after_the_source_is_deleted() {
    let dir = tempdir();
    // A .rl bundle holding one :Person — the shared `person_bundle`
    // fixture the `--load` suites use.
    let bundle = person_bundle(dir.path()).to_str().unwrap().to_string();

    let http_port = free_port();
    let mcp_port = free_port();
    let mut child = spawn_serve_with(
        ":memory:",
        http_port,
        mcp_port,
        Some(AUTH),
        &["--load", &bundle],
    );
    wait_for_ports(&[http_port, mcp_port]);

    // The loaded data serves.
    assert_eq!(served_count(http_port), 1);

    // The source is released: delete it underneath the running server and
    // the served store does not notice — the turn-into holds no lock and
    // no open handle (a plain --read-only serve of the same bundle would
    // hold ADR-0074's exclusive bundle lock, and this deletion would
    // succeed only after the server exits).
    std::fs::remove_file(&bundle).unwrap();
    assert_eq!(served_count(http_port), 1);

    // A write is refused — the turn-into is read-only by construction.
    let (status, body) = exchange(
        http_port,
        post(
            "/db/neo4j/query/v2",
            r#"{"statement": "CREATE (:Refused {v: 9})"}"#,
            Some(&auth_header()),
        )
        .as_bytes(),
    );
    assert_eq!(status, 400, "{body}");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let message = json["errors"][0]["message"]
        .as_str()
        .expect("the error envelope carries a message");
    assert!(message.contains("read-only"), "{message}");

    // The banner names the turn-into, and SIGINT is a clean exit.
    let (status, stderr) = child.sigint_and_drain_stderr();
    assert!(status.success(), "serve exit on SIGINT: {status}");
    assert!(
        stderr.contains(
            "serve: loaded the store into memory — read-only, the source \
             released after the load; every write fails"
        ),
        "{stderr}"
    );
}
