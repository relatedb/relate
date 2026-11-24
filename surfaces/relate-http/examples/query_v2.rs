//! Runnable example for `docs/api/http/examples/query.md`: start the HTTP
//! Query API v2 endpoint on an ephemeral port against an in-memory store and
//! run one write and one parameterized read through a raw HTTP client, plus
//! the ADR-0610 `tx_metadata` exchange — a write whose transaction metadata
//! reaches the commit's record under the `value-provenance` opt-in.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

use relate::Database;
use relate_auth::AuthConfig;
use relate_http::{serve, HttpConfig};

fn post(port: u16, database: &str, body: &str) -> (u16, serde_json::Value) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let request = format!(
        "POST /db/{database}/query/v2 HTTP/1.1\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let status: u16 = raw.split_whitespace().nth(1).unwrap().parse().unwrap();
    let body = raw.split("\r\n\r\n").nth(1).unwrap();
    (status, serde_json::from_str(body).unwrap())
}

fn main() {
    // The ADR-0610 exchange needs the opt-in: RELATE_PREVIEW is read once
    // per open, so the example sets it before its first store — the same
    // opt-in a `RELATE_PREVIEW=value-provenance relate serve …` start
    // carries.
    std::env::set_var("RELATE_PREVIEW", "value-provenance");
    let db = Arc::new(Database::open(":memory:").unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let served = Arc::clone(&db);
    std::thread::spawn(move || {
        serve(served, listener, HttpConfig::new(AuthConfig::None)).expect("serve starts")
    });

    let (status, write) = post(
        port,
        "neo4j",
        r#"{"statement": "CREATE (:City {name: 'Oslo'})"}"#,
    );
    assert_eq!(status, 200);
    assert_eq!(write["counters"]["nodes_created"], 1);

    let (status, read) = post(
        port,
        "neo4j",
        r#"{"statement": "MATCH (c:City {name: $name}) RETURN c.name AS name",
            "parameters": {"name": "Oslo"}}"#,
    );
    assert_eq!(status, 200);
    assert_eq!(read["data"]["fields"], serde_json::json!(["name"]));
    assert_eq!(read["data"]["values"], serde_json::json!([["Oslo"]]));

    // The ADR-0610 `tx_metadata` exchange: the write's audit map rides the
    // request and the commit's transaction record carries it.
    let (status, audited) = post(
        port,
        "neo4j",
        r#"{"statement": "CREATE (:City {name: 'Bergen'})",
            "tx_metadata": {"by": "seed-script", "run": 42}}"#,
    );
    assert_eq!(status, 200);
    assert_eq!(audited["counters"]["nodes_created"], 1);
    let records = db.transaction_records().unwrap();
    // Both writes retained records (their changed keys); the audited
    // write's is the later one, and it carries the request's map.
    assert_eq!(records.len(), 2);
    let metadata = records[1].metadata.as_ref().unwrap();
    assert_eq!(metadata["by"], relate::Value::String("seed-script".into()));
    assert_eq!(metadata["run"], relate::Value::Int(42));
    println!("HTTP Query API v2 example OK on port {port}");
}
