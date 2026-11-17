//! End-to-end MCP session over the stdio transport: the crate's own
//! `serve_stdio` driving a real duplex stream, the way an MCP host drives
//! the server's stdin/stdout.
//!
//! The standalone `relate-mcp` binary is retired (LABS-381, ADR-0368):
//! `relate serve --mcp stdio` is the product path that hosts spawn, and this
//! crate's
//! protocol coverage runs the same transport loop in-process over a TCP
//! duplex pair (a real byte stream, so framing and flush behavior are
//! exercised exactly as over a pipe). The spawned process boundary itself is
//! covered by the CLI's own e2e (`surfaces/relate-cli/tests`), which
//! spawns `relate serve --mcp stdio` as a child.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use relate::{Database, OpenMode, OpenOptions};
use relate_mcp::{serve_stdio, McpServer, McpServerConfig};
use relate_test_support::uri;
use serde_json::{json, Value};

/// The store the session serves and the config its flags fold to — the
/// small piece of the retired binary's argument fold this crate's tests
/// still need (the product fold now lives in `relate serve`).
struct SpawnSpec<'a> {
    database: &'a str,
    namespace: &'a str,
    mode: OpenMode,
}

impl Default for SpawnSpec<'_> {
    fn default() -> Self {
        SpawnSpec {
            database: ":memory:",
            namespace: "",
            mode: OpenMode::Writable,
        }
    }
}

/// One end of a duplex byte stream: two connected TCP streams, so both
/// halves are independent owned handles (portable, unlike a Unix socket
/// pair, and real streams unlike an in-memory cursor).
fn duplex() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind the duplex pair");
    let client = TcpStream::connect(listener.local_addr().unwrap()).expect("connect the pair");
    let (server, _) = listener.accept().expect("accept the pair");
    (client, server)
}

struct StdioSession {
    writer: TcpStream,
    reader: BufReader<TcpStream>,
    /// The serve loop's thread; it ends at EOF (the session's drop closes
    /// the writer).
    serve: Option<JoinHandle<()>>,
}

impl StdioSession {
    /// Serve the default `:memory:` store.
    fn spawn() -> Self {
        Self::spawn_store(SpawnSpec::default())
    }

    /// Serve an explicit store with an explicit namespace and open mode:
    /// the beside-a-writer sessions (ADR-0306) serve a native directory the
    /// test process itself holds writable. The stdio transport skips auth
    /// entirely (ADR-0232 §1), so no credential applies.
    fn spawn_store(spec: SpawnSpec<'_>) -> Self {
        let db = Database::open_with(spec.database, OpenOptions::with_mode(spec.mode))
            .unwrap_or_else(|e| panic!("open {}: {e}", spec.database));
        let config = McpServerConfig {
            namespace: spec.namespace.to_string(),
            read_only: db.is_read_only(),
            ..McpServerConfig::default()
        };
        let server = McpServer::new(db, config);
        let (client, server_end) = duplex();
        let serve = std::thread::spawn(move || {
            let reader = BufReader::new(server_end.try_clone().expect("clone the server stream"));
            // Ok on EOF; an error means the host closed mid-response, which
            // the session's own asserts have already judged.
            let _ = serve_stdio(&server, reader, server_end);
        });
        StdioSession {
            writer: client.try_clone().expect("clone the client stream"),
            reader: BufReader::new(client),
            serve: Some(serve),
        }
    }

    /// Send one request, read its one response line.
    fn call(&mut self, request: Value) -> Value {
        self.send(request);
        let mut response = String::new();
        self.reader.read_line(&mut response).unwrap();
        assert!(!response.is_empty(), "server closed the stream");
        serde_json::from_str(&response).unwrap()
    }

    /// The one line writer both halves share: a notification is a send
    /// without a response read.
    fn send(&mut self, value: Value) {
        let mut line = value.to_string();
        line.push('\n');
        self.writer.write_all(line.as_bytes()).unwrap();
        self.writer.flush().unwrap();
    }

    fn notify(&mut self, notification: Value) {
        self.send(notification);
    }

    fn initialize(&mut self) {
        let response = self.call(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "integration-test", "version": "0"},
            },
        }));
        assert_eq!(response["result"]["protocolVersion"], json!("2024-11-05"));
        assert_eq!(
            response["result"]["serverInfo"]["name"],
            json!("relate-mcp")
        );
        self.notify(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    }

    fn call_tool(&mut self, id: u64, name: &str, arguments: Value) -> Value {
        self.call(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        }))
    }
}

impl Drop for StdioSession {
    fn drop(&mut self) {
        // Shutting the stream down is the host's EOF: the serve loop
        // returns and its thread ends (both TcpStream clones share the
        // connection, so this reaches the server's reader). The join must
        // happen while the writer is closed — the server blocks on stdin
        // until then.
        let _ = self.writer.shutdown(std::net::Shutdown::Both);
        if let Some(serve) = self.serve.take() {
            let _ = serve.join();
        }
    }
}

fn tool_text(response: &Value) -> &str {
    response["result"]["content"][0]["text"]
        .as_str()
        .expect("tool result carries text content")
}

/// The tools/list names, in listed order.
fn tool_names(session: &mut StdioSession) -> Vec<String> {
    let list = session.call(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {},
    }));
    list["result"]["tools"]
        .as_array()
        .expect("tools/list returns an array")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name").to_string())
        .collect()
}

/// The count of `:T` nodes as the server sees it, via read_cypher. The
/// unaliased `count(n)` column name is engine-chosen, so the first
/// column's first row is read positionally.
fn t_count(session: &mut StdioSession) -> i64 {
    let read = session.call_tool(
        3,
        "read_cypher",
        json!({"statement": "MATCH (n:T) RETURN count(n)"}),
    );
    assert_eq!(read["result"]["isError"], json!(false));
    let envelope: Value = serde_json::from_str(tool_text(&read)).unwrap();
    let column = envelope["columns"][0].as_str().expect("count column");
    envelope["rows"][0][column].as_i64().expect("integer count")
}

/// The canonical wire envelope carries the engine-reported elapsed
/// (ADR-0221) and its ADR-0491 phase split on every tool result: the
/// total plus the three phase keys, each a non-negative millisecond
/// number. One assertion for both cypher tools' envelopes — the write
/// counters and the read rows.
fn assert_phase_split(envelope: &Value, label: &str) {
    assert!(
        envelope["elapsed_ms"].as_f64().expect("elapsed_ms") >= 0.0,
        "the ADR-0221 elapsed rides the {label} envelope: {envelope}"
    );
    for key in ["parse_ms", "plan_ms", "execute_ms"] {
        assert!(
            envelope[key].as_f64().is_some_and(|ms| ms >= 0.0),
            "the ADR-0491 phase split rides the {label} envelope: {envelope}"
        );
    }
}

#[test]
fn stdio_full_session_covers_all_three_tools() {
    let mut session = StdioSession::spawn();
    session.initialize();

    // tools/list advertises the reference server's three tools. The
    // names stay inline rather than going through `tool_names`: the one
    // response also feeds the annotations assert below, and the helper's
    // names-only return would need a second tools/list call to reach it.
    let list = session.call(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {},
    }));
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["write_cypher", "read_cypher", "get_schema"]);
    // The annotations are the wire kit's table's: the read tool is the
    // read-only one, the write tool is not.
    assert_eq!(
        list["result"]["tools"][1]["annotations"]["readOnlyHint"],
        json!(true)
    );
    assert_eq!(
        list["result"]["tools"][0]["annotations"]["readOnlyHint"],
        json!(false)
    );

    // write_cypher runs a write and returns the mutation counters.
    let write = session.call_tool(
        3,
        "write_cypher",
        json!({"statement": "CREATE (a:Person {name: $name})-[:KNOWS {since: 1843}]->(b:Person {name: 'Bob'})",
               "parameters": {"name": "Ada"}}),
    );
    assert_eq!(write["result"]["isError"], json!(false));
    let counters: Value = serde_json::from_str(tool_text(&write)).unwrap();
    assert_eq!(counters["stats"]["nodes_created"], json!(2));
    assert_eq!(counters["stats"]["relationships_created"], json!(1));
    assert_phase_split(&counters, "write");

    // read_cypher returns the wire envelope: row objects under `rows`,
    // engine-stamped column types (ADR-0224) beside the column names.
    let read = session.call_tool(
        4,
        "read_cypher",
        json!({"statement": "MATCH (p:Person) RETURN p.name AS name ORDER BY name"}),
    );
    assert_eq!(read["result"]["isError"], json!(false));
    let envelope: Value = serde_json::from_str(tool_text(&read)).unwrap();
    assert_eq!(envelope["columns"], json!(["name"]));
    assert_eq!(envelope["columnTypes"], json!(["STRING"]));
    assert_eq!(envelope["rows"], json!([{"name": "Ada"}, {"name": "Bob"}]));
    assert_phase_split(&envelope, "read");

    // get_schema reports the label, its properties, and the relationship.
    let schema = session.call_tool(5, "get_schema", json!({}));
    assert_eq!(schema["result"]["isError"], json!(false));
    let schema: Value = serde_json::from_str(tool_text(&schema)).unwrap();
    assert_eq!(schema["Person"]["type"], json!("node"));
    assert_eq!(schema["Person"]["count"], json!(2));
    assert_eq!(
        schema["Person"]["properties"]["name"]["type"],
        json!("String")
    );
    assert_eq!(
        schema["Person"]["relationships"]["KNOWS"]["labels"],
        json!(["Person"])
    );
}

/// ADR-0363 decision 10: both cypher tools carry the classification
/// `EXPLAIN`'s notifications — the one `call_cypher` already runs — in the
/// shared wire envelope's native shape. A statement that raises none still
/// carries the always-present key as an empty array (decision 9).
#[test]
fn stdio_cypher_tools_carry_the_classification_explains_notifications() {
    let mut session = StdioSession::spawn();
    session.initialize();

    // One :Person and one :City: the two-pattern store the cartesian
    // statement's :Person / :City patterns name.
    let created = session.call_tool(
        2,
        "write_cypher",
        json!({"statement": "CREATE (:Person), (:City)"}),
    );
    assert_eq!(created["result"]["isError"], json!(false));

    let read = session.call_tool(
        3,
        "read_cypher",
        json!({"statement": "MATCH (a:Person), (b:City) RETURN a, b"}),
    );
    assert_eq!(read["result"]["isError"], json!(false));
    let envelope: Value = serde_json::from_str(tool_text(&read)).unwrap();
    let notifications = envelope["notifications"]
        .as_array()
        .expect("the wire envelope always carries the notifications array");
    assert_eq!(notifications.len(), 1, "{envelope}");
    let notification = &notifications[0];
    assert_eq!(
        notification["code"],
        json!("Neo.ClientNotification.Statement.CartesianProduct")
    );
    assert_eq!(notification["gql_status"], json!("03N90"));
    assert_eq!(notification["severity"], json!("INFORMATION"));
    assert_eq!(notification["category"], json!("PERFORMANCE"));
    assert_eq!(notification["title"], json!("Cartesian product"));
    assert!(
        notification["remedy"]
            .as_str()
            .unwrap()
            .contains("join anchor"),
        "the native form carries the remedy: {notification}"
    );
    assert!(
        notification["operator_id"].is_number(),
        "the cartesian kind is operator-scoped: {notification}"
    );

    // write_cypher goes through the same envelope: the classification
    // EXPLAIN's list rides a writing statement too.
    let write = session.call_tool(
        4,
        "write_cypher",
        json!({"statement": "MATCH (a:Person), (b:City) SET a.paired = true"}),
    );
    assert_eq!(write["result"]["isError"], json!(false));
    let envelope: Value = serde_json::from_str(tool_text(&write)).unwrap();
    assert_eq!(
        envelope["notifications"][0]["code"],
        json!("Neo.ClientNotification.Statement.CartesianProduct")
    );

    // A statement without notifications carries the key, empty. The witness
    // must be one that cannot raise any statement-scoped kind: a bare
    // `RETURN` names no label, relationship type, property key or
    // variable-length pattern, so no detector can fire (unlike
    // `MATCH (p:Person) RETURN p.name`, whose key may be unwritten).
    let read = session.call_tool(5, "read_cypher", json!({"statement": "RETURN 1 AS n"}));
    assert_eq!(read["result"]["isError"], json!(false));
    let envelope: Value = serde_json::from_str(tool_text(&read)).unwrap();
    assert_eq!(envelope["notifications"], json!([]), "{envelope}");
}

#[test]
fn stdio_read_cypher_rejects_a_write_statement() {
    let mut session = StdioSession::spawn();
    session.initialize();

    let response = session.call_tool(
        2,
        "read_cypher",
        json!({"statement": "CREATE (n:Person {name: 'Ada'})"}),
    );
    assert_eq!(response["result"]["isError"], json!(true));
    assert_eq!(
        tool_text(&response),
        "Only MATCH queries are allowed for read-query"
    );
}

/// LABS-888: the read gate admits an OPTIONAL MATCH read end to end. The
/// repro — `MATCH (a:Room) OPTIONAL MATCH (a)-[:NORTH]->(b) RETURN a.name,
/// b.name` — classifies `r` (the planner's access fold reads OPTIONAL MATCH
/// as the read its plain MATCH counterpart is), so `read_cypher` serves it
/// rather than refusing with the `Only MATCH queries` text. The fixture
/// holds both arms — one :Room with a :NORTH neighbour and one without — so
/// the unmatched rows' `b.name` comes back null (two of them: the isolated
/// :Room, and the neighbour :Room, which is a `:Room` too and has no
/// outgoing :NORTH). The statement stays the bare repro (no ORDER BY): the
/// rows are asserted order-insensitively, sorted by `a.name`.
#[test]
fn stdio_read_cypher_serves_an_optional_match_read() {
    let mut session = StdioSession::spawn();
    session.initialize();

    let created = session.call_tool(
        2,
        "write_cypher",
        json!({"statement": "CREATE (:Room {name: 'A'})-[:NORTH]->(:Room {name: 'B'}), (:Room {name: 'C'})"}),
    );
    assert_eq!(created["result"]["isError"], json!(false));

    let read = session.call_tool(
        3,
        "read_cypher",
        json!({"statement": "MATCH (a:Room) OPTIONAL MATCH (a)-[:NORTH]->(b) RETURN a.name, b.name"}),
    );
    assert_eq!(read["result"]["isError"], json!(false));
    let envelope: Value = serde_json::from_str(tool_text(&read)).unwrap();
    assert_eq!(envelope["columns"], json!(["a.name", "b.name"]));
    let rows = envelope["rows"]
        .as_array()
        .unwrap_or_else(|| panic!("the optional-match read returns rows: {envelope}"));
    let mut arms: Vec<(&str, Option<&str>)> = rows
        .iter()
        .map(|row| {
            (
                row["a.name"].as_str().unwrap_or_else(|| panic!("{row}")),
                row["b.name"].as_str(),
            )
        })
        .collect();
    arms.sort();
    assert_eq!(
        arms,
        [("A", Some("B")), ("B", None), ("C", None)],
        "{envelope}"
    );
}

#[test]
fn stdio_protocol_errors_use_the_json_rpc_error_envelope() {
    let mut session = StdioSession::spawn();
    session.initialize();

    let response = session.call(json!({
        "jsonrpc": "2.0", "id": 10, "method": "resources/list", "params": {},
    }));
    assert_eq!(response["error"]["code"], json!(-32601));
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("resources/list"));

    let mut line = b"{ not json\n".to_vec();
    session.writer.write_all(&line).unwrap();
    session.writer.flush().unwrap();
    line.clear();
    let mut response = String::new();
    session.reader.read_line(&mut response).unwrap();
    let response: Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["id"], Value::Null);
    assert_eq!(response["error"]["code"], json!(-32700));
}

/// The strict decode is the wire kit's, so a body that omits the `jsonrpc`
/// member is refused with the same pinned text and code the extension mount
/// answers with — never decoded as a request.
/// A malformed `tools/call` payload is refused in the shared tool-layer
/// vocabulary, the same words the extension mount answers.
#[test]
fn stdio_a_malformed_tools_call_is_refused_in_the_shared_words() {
    let mut session = StdioSession::spawn();
    session.initialize();
    let response = session.call(json!({
        "jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {},
    }));
    assert_eq!(response["error"]["code"], json!(-32602));
    assert_eq!(
        response["error"]["message"],
        json!("invalid params: missing string \"name\"")
    );
    let response = session.call(json!({
        "jsonrpc": "2.0", "id": 8, "method": "tools/call", "params": [1],
    }));
    assert_eq!(
        response["error"]["message"],
        json!("invalid params: \"params\" must be an object")
    );
}

#[test]
fn stdio_a_body_without_the_jsonrpc_member_is_refused_with_the_pinned_text() {
    let mut session = StdioSession::spawn_store(SpawnSpec::default());

    session
        .writer
        .write_all(b"{\"id\":7,\"method\":\"ping\"}\n")
        .unwrap();
    session.writer.flush().unwrap();
    let mut response = String::new();
    session.reader.read_line(&mut response).unwrap();
    let response: Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["id"], Value::Null);
    assert_eq!(response["error"]["code"], json!(-32700));
    assert_eq!(
        response["error"]["message"],
        json!("malformed JSON-RPC message: missing field `jsonrpc` at line 1 column 24")
    );
}

#[test]
fn stdio_namespace_prefixes_tool_names() {
    let mut session = StdioSession::spawn_store(SpawnSpec {
        namespace: "db1",
        ..SpawnSpec::default()
    });
    session.initialize();

    let list = session.call(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {},
    }));
    assert_eq!(
        list["result"]["tools"][0]["name"],
        json!("db1-write_cypher")
    );

    let read = session.call_tool(
        3,
        "db1-read_cypher",
        json!({"statement": "RETURN 1 AS one"}),
    );
    let envelope: Value = serde_json::from_str(tool_text(&read)).unwrap();
    assert_eq!(envelope["rows"], json!([{"one": 1}]));
}

#[test]
fn stdio_read_only_disables_write_cypher() {
    let mut session = StdioSession::spawn_store(SpawnSpec {
        mode: OpenMode::ReadOnly,
        ..SpawnSpec::default()
    });
    session.initialize();

    assert_eq!(tool_names(&mut session), ["read_cypher", "get_schema"]);

    let response = session.call_tool(3, "write_cypher", json!({"statement": "CREATE (n)"}));
    assert_eq!(response["result"]["isError"], json!(true));
    assert_eq!(tool_text(&response), "unknown tool 'write_cypher'");
}

/// Seed a native directory store the test process holds writable, with one
/// committed `(:T)` node: the beside-a-writer sessions (ADR-0306) serve this
/// store while the writer keeps committing. Bind the tuple as
/// `let (dir, writer, store) = …` — locals drop in reverse declaration
/// order, so the directory outlives the writer's final checkpoint and the
/// session (declared after) joins its serve thread before either runs.
fn writable_store_with_one_t() -> (tempfile::TempDir, relate::Database, String) {
    let dir = tempfile::tempdir().unwrap();
    let store = uri(&dir.path().join("db")).to_string();
    let writer = relate::Database::open(&store).unwrap();
    writer.connect().unwrap().execute("CREATE (:T)").unwrap();
    (dir, writer, store)
}

#[test]
fn stdio_read_only_serves_beside_a_writer() {
    // ADR-0306 §8: `--read-only` opens ReadOnly — no writer lease, so the
    // test process's own writable handle keeps committing beside the
    // server.
    let (_dir, writer, store) = writable_store_with_one_t();
    let mut session = StdioSession::spawn_store(SpawnSpec {
        database: &store,
        mode: OpenMode::ReadOnly,
        ..SpawnSpec::default()
    });
    session.initialize();

    // The read-only open hides write_cypher (an always-failing tool would
    // be a dead end for hosts that auto-approve readOnlyHint tools).
    assert_eq!(tool_names(&mut session), ["read_cypher", "get_schema"]);

    // The server loaded the writer's committed node.
    assert_eq!(t_count(&mut session), 1);

    // No writer lease was taken: the test's own writable handle still
    // commits while the server serves.
    writer.connect().unwrap().execute("CREATE (:T)").unwrap();

    // ReadOnly loads once (ADR-0136): the later commit stays invisible to
    // the frozen server.
    assert_eq!(t_count(&mut session), 1);
}

#[test]
fn stdio_follow_serves_the_writers_commits() {
    // ADR-0306 §8: a Follow open (ADR-0137 — read-only plus the writer's
    // commits, applied by the follower's 50 ms WAL poll; the CLI spelling
    // is `--read-only=live` since ADR-0480).
    let (_dir, writer, store) = writable_store_with_one_t();
    let mut session = StdioSession::spawn_store(SpawnSpec {
        database: &store,
        mode: OpenMode::Follow,
        ..SpawnSpec::default()
    });
    session.initialize();

    // Follow implies read-only: write_cypher stays hidden.
    assert_eq!(tool_names(&mut session), ["read_cypher", "get_schema"]);
    assert_eq!(t_count(&mut session), 1);

    writer.connect().unwrap().execute("CREATE (:T)").unwrap();

    // The follower's poll applies the commit: the count reaches 2 well
    // within the 10 s window.
    let deadline = Instant::now() + Duration::from_secs(10);
    while t_count(&mut session) != 2 {
        assert!(
            Instant::now() < deadline,
            "the follower never reached a count of 2"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

// The open-mode startup failures a stdio serve can hit — a read-only open
// of a missing store creating nothing (ADR-0136), and Follow refusing
// `:memory:` (ADR-0137) — are the engine's own open semantics, pinned by
// the engine's test surface (`read_only_refuses_a_missing_path_without_creating_it`
// in relate's tests/read_only.rs and `follow_refuses_memory_and_bundle`
// in its src/tests/follow.rs, which also covers the .rl bundle). This
// module drove them through the pure `Database::open_with` engine API as
// duplicates (LABS-392 round 1, F-1-10's follow-up); `relate serve`'s
// own e2e pins that the serve command surfaces the same open failures.
