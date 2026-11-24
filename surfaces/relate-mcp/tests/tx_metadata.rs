//! ADR-0610's `tx_metadata` tool argument end to end, behind the
//! `value-provenance` opt-in: the product server's stdio session seeds a
//! `write_cypher` call's map into the store's commit record, the shared
//! `batch_cypher` body does the same inside the extension mount's
//! explicit-transaction framing, and the advertised schemas name the
//! argument — read back through the engine's retained-record API
//! ([`Database::transaction_records`]).
//!
//! This file is its own test binary: `RELATE_PREVIEW` is process-global
//! and read once per `Database::open` (ADR-0371), so the opt-in is set
//! before the first open and every test here runs behind the gate. The
//! closed side — the authored preview error a call carrying the argument
//! answers without the opt-in — lives in the unit suite
//! (`src/tools/tests.rs`).

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

use relate::{Database, Value};
use relate_mcp::tools::{Limits, Tools, BATCH_CYPHER, WRITE_CYPHER};
use relate_mcp::{serve_stdio, McpServer, McpServerConfig};
use serde_json::{json, Value as Json};

/// Opt this process into `value-provenance` before the first open — the
/// Once-shared form, so sibling threads opening stores race no in-flight
/// `set_var`.
fn gated() -> Database {
    relate_test_support::preview::set_preview_once("value-provenance");
    Database::open(":memory:").unwrap()
}

/// One end of a duplex byte stream: two connected TCP streams, so both
/// halves are independent owned handles (the same portable pair the
/// stdio transport tests use).
fn duplex() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    (client, server)
}

/// A live stdio session over the product server, sharing its store with
/// the test so the retained records stay reachable while it serves.
struct StdioSession {
    writer: TcpStream,
    reader: BufReader<TcpStream>,
}

impl StdioSession {
    /// Serve `db` (kept by the test) over a duplex pair.
    fn spawn(db: &Arc<Database>) -> Self {
        let config = McpServerConfig {
            read_only: db.is_read_only(),
            ..McpServerConfig::default()
        };
        let server = McpServer::new_shared(Arc::clone(db), config);
        let (client, server_end) = duplex();
        std::thread::spawn(move || {
            let reader = BufReader::new(server_end.try_clone().unwrap());
            // Ok on EOF; an error means the host closed mid-response,
            // which the session's own asserts have already judged.
            let _ = serve_stdio(&server, reader, server_end);
        });
        Self {
            writer: client.try_clone().unwrap(),
            reader: BufReader::new(client),
        }
    }

    /// Send one request, read its one response line.
    fn call(&mut self, request: Json) -> Json {
        let mut line = request.to_string();
        line.push('\n');
        self.writer.write_all(line.as_bytes()).unwrap();
        self.writer.flush().unwrap();
        let mut response = String::new();
        self.reader.read_line(&mut response).unwrap();
        assert!(!response.is_empty(), "server closed the stream");
        serde_json::from_str(&response).unwrap()
    }

    /// The handshake every session starts with.
    fn initialize(&mut self) {
        let response = self.call(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "tx-metadata-test", "version": "0"},
            },
        }));
        assert_eq!(response["result"]["protocolVersion"], json!("2024-11-05"));
        self.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    }

    /// A notification is a send without a response read.
    fn send(&mut self, value: Json) {
        let mut line = value.to_string();
        line.push('\n');
        self.writer.write_all(line.as_bytes()).unwrap();
        self.writer.flush().unwrap();
    }

    fn call_tool(&mut self, id: u64, name: &str, arguments: Json) -> Json {
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
        // The host's EOF: the serve loop returns and its thread ends.
        let _ = self.writer.shutdown(std::net::Shutdown::Both);
    }
}

/// The `by: app, run: 7` map the seeds below share, spelled once.
fn by_app() -> std::collections::BTreeMap<String, Value> {
    std::collections::BTreeMap::from([
        ("by".to_string(), Value::String("app".into())),
        ("run".to_string(), Value::Int(7)),
    ])
}

/// The store's retained transaction records' metadata maps, in commit
/// order.
fn metadata_records(db: &Database) -> Vec<Option<std::collections::BTreeMap<String, Value>>> {
    db.transaction_records()
        .unwrap()
        .into_iter()
        .map(|record| record.metadata)
        .collect()
}

/// The product mount: a `write_cypher` call carrying `tx_metadata`
/// commits with the map in its transaction record, and the advertised
/// schemas name the argument on the writing tool alone.
#[test]
fn write_cypher_tx_metadata_reaches_the_commit() {
    let db = Arc::new(gated());
    let mut session = StdioSession::spawn(&db);
    session.initialize();

    // The advertised schema carries the argument (the wire kit's one
    // table, both mounts' catalog), on the writing tool only.
    let list = session.call(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {},
    }));
    let tools = list["result"]["tools"].as_array().unwrap();
    let schema_of = |name: &str| {
        tools
            .iter()
            .find(|tool| tool["name"] == json!(name))
            .unwrap_or_else(|| panic!("tools/list carries {name}"))["inputSchema"]
            .clone()
    };
    assert_eq!(
        schema_of(WRITE_CYPHER)["properties"]["tx_metadata"]["type"],
        json!("object")
    );
    assert!(schema_of("read_cypher")["properties"]
        .get("tx_metadata")
        .is_none());

    // The call: the write commits with the seeded map.
    let write = session.call_tool(
        3,
        WRITE_CYPHER,
        json!({"statement": "CREATE (n:Person {name: 'Ada'})",
               "tx_metadata": {"by": "app", "run": 7}}),
    );
    assert_eq!(write["result"]["isError"], json!(false));
    assert_eq!(metadata_records(&db), vec![Some(by_app())]);
}

/// The shared batch body inside the extension mount's framing: the
/// extension mount runs every writing call inside one explicit
/// transaction (its commit policy), so the seed lands between the begin
/// and the commit and the commit serializes it — pinned here over a
/// connection the test itself frames the same way.
#[test]
fn batch_cypher_tx_metadata_reaches_the_framed_commit() {
    let db = gated();
    let mut conn = db.connect().unwrap();
    // The framing the extension mount's per-batch policy applies: begin,
    // run the tool call, commit.
    conn.begin().unwrap();
    let outcome = Tools::new(&mut conn, Limits::shipped())
        .call(
            BATCH_CYPHER,
            json!({"statements": [{"statement": "CREATE (n:Batch {slot: 1})"},
                                  {"statement": "CREATE (n:Batch {slot: 2})"}],
                   "tx_metadata": {"by": "app", "run": 7}})
            .as_object()
            .expect("an object"),
        )
        .unwrap();
    let relate_mcp::tools::ToolOutcome::Batch(results) = outcome else {
        panic!("a batch call answers the per-statement results");
    };
    assert_eq!(results.len(), 2);
    conn.commit().unwrap();
    // One commit for the whole batch, carrying the call-level map.
    assert_eq!(metadata_records(&db), vec![Some(by_app())]);
}

/// A call the mount refuses leaves no seed behind: the write gate's
/// refusal runs before the seed, so a refused `write_cypher` (a pure read
/// handed to the write tool) contributes no metadata to the next commit.
#[test]
fn a_refused_call_seeds_nothing() {
    let db = gated();
    let mut conn = db.connect().unwrap();
    assert!(Tools::new(&mut conn, Limits::shipped())
        .call(
            WRITE_CYPHER,
            json!({"statement": "MATCH (n) RETURN n", "tx_metadata": {"by": "app"}})
                .as_object()
                .expect("an object"),
        )
        .is_err());
    conn.query("CREATE (n:After {id: 1})").unwrap();
    // The follow-up write retains a record (its changed keys) whose
    // metadata is empty: the refused call contributed nothing to it.
    let records = metadata_records(&db);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0], None);
}
