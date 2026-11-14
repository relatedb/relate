//! The child kit's tests (ADR-0442): the handshake over scripted pipes,
//! the caps the kit chunks and refuses against, and the policy-scoped
//! whole-run stream budget (ADR-0598) — a per-batch child streams past
//! any published cap, a whole-import child refuses with the pinned text
//! before the host kills it.

use super::super::protocol::{request_bytes_cap, ServerFacts};
use super::super::tools::{tool_result, READ_CYPHER, READ_ONLY_REJECTION, WRITE_CYPHER};
use super::*;
use crate::{MAX_ROWS_PER_STATEMENT, MAX_STATEMENTS, MAX_STREAM_BYTES, PER_BATCH, WHOLE_IMPORT};
use serde_json::json;
use std::io::Cursor;

/// A scripted child over the tests' cursor-and-Vec pair — the client
/// [`McpChild::start`](super::McpChild::start) returns for a hand-fed
/// script.
type Scripted = McpChild<Cursor<Vec<u8>>, Vec<u8>>;

/// The host's initialize result for the fixture manifest facts.
fn facts_json() -> Value {
    json!({
        "serverInfo": {"name": "relate", "version": "0.13.0"},
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": {"tools": {"listChanged": false}},
        "freshStoreRequired": false,
        "commitPolicy": PER_BATCH,
        "caps": {
            "maxStatementBytes": MAX_STATEMENT_BYTES,
            "maxRowsPerStatement": MAX_ROWS_PER_STATEMENT,
            "maxStatements": MAX_STATEMENTS,
            "maxStreamBytes": MAX_STREAM_BYTES,
            "maxRequestBytes": request_bytes_cap(MAX_STATEMENT_BYTES),
        },
        "capacity": {
            "storeLimitBytes": 1 << 20,
            "storeUsedBytes": 1024,
            "storeRemainingBytes": (1 << 20) - 1024,
        },
    })
}

/// The fixture facts under the commit policy and stream cap a test
/// spells — the two facts the kit's whole-run guard reads (ADR-0598).
fn facts_under(commit: &str, stream_cap: u64) -> Value {
    let mut facts = facts_json();
    facts["commitPolicy"] = json!(commit);
    facts["caps"]["maxStreamBytes"] = json!(stream_cap);
    facts
}

/// The scripted host side for the kit's tests: a response line for the
/// id the child is about to send.
fn response_line(id: u64, result: Value) -> String {
    Response::success(json!(id), result).to_line() + "\n"
}

/// A child whose handshake already succeeded: the host's empty envelope,
/// the initialize response, then `tail` — the scripted responses the
/// test's calls consume. Returns the child and the facts the handshake
/// decoded.
fn started_child(tail: &str) -> (Scripted, ServerFacts) {
    started_child_with(facts_json(), tail)
}

/// [`started_child`] under facts the test spells — the policy-scoped
/// guard tests publish a commit policy and stream cap the default
/// fixture does not carry.
fn started_child_with(facts: Value, tail: &str) -> (Scripted, ServerFacts) {
    let mut input = String::new();
    input.push_str("{}\n");
    input.push_str(&response_line(1, facts));
    input.push_str(tail);
    let (child, facts, envelope): (Scripted, ServerFacts, Value) = McpChild::start(
        Cursor::new(input.into_bytes()),
        Vec::new(),
        "greet",
        "0.1.0",
    )
    .unwrap();
    assert_eq!(envelope, json!({}));
    (child, facts)
}

#[test]
fn the_server_facts_decode_the_initialize_result() {
    let (_child, facts) = started_child("");
    assert_eq!(facts.protocol_version, MCP_PROTOCOL_VERSION);
    assert!(!facts.capabilities.tools.list_changed);
    assert!(!facts.fresh_store_required);
    assert_eq!(facts.commit_policy.spelling(), PER_BATCH);
    assert_eq!(facts.caps.max_rows_per_statement, MAX_ROWS_PER_STATEMENT);
    assert_eq!(facts.caps.max_statements, MAX_STATEMENTS);
    assert_eq!(facts.caps.max_statement_bytes, MAX_STATEMENT_BYTES);
    assert_eq!(
        facts.caps.max_request_bytes,
        request_bytes_cap(MAX_STATEMENT_BYTES)
    );
    // The child sees the store's capacity in the same handshake: the
    // baseline its own write results count down from.
    assert_eq!(facts.capacity.store_limit_bytes, 1 << 20);
    assert_eq!(facts.capacity.store_used_bytes, 1024);
    assert_eq!(facts.capacity.store_remaining_bytes, (1 << 20) - 1024);
}

#[test]
fn the_child_reads_the_envelope_then_initializes_and_calls() {
    let (mut child, _facts) = started_child(&response_line(
        2,
        tool_result("{\"counts\":{\"nodes_created\":3}}"),
    ));
    let result = child
        .call(
            WRITE_CYPHER,
            json!({"statement": "UNWIND $rows AS row CREATE (n:N) RETURN 1"}),
        )
        .unwrap();
    assert_eq!(result, json!({"counts": {"nodes_created": 3}}));
    let written = String::from_utf8(child.out).unwrap();
    let lines: Vec<Value> = written
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    // MCP's handshake: initialize with the revision and the client
    // identity, then the id-less initialized confirmation, then tools.
    assert_eq!(lines[0]["method"], "initialize");
    assert_eq!(lines[0]["jsonrpc"], JSONRPC_VERSION);
    assert_eq!(lines[0]["id"], json!(1));
    assert_eq!(
        lines[0]["params"]["protocolVersion"],
        json!(MCP_PROTOCOL_VERSION)
    );
    assert_eq!(lines[0]["params"]["clientInfo"]["name"], json!("greet"));
    assert_eq!(lines[1]["method"], METHOD_INITIALIZED);
    assert_eq!(lines[1]["jsonrpc"], JSONRPC_VERSION);
    assert!(
        lines[1].get("id").is_none(),
        "a notification carries no id: {}",
        lines[1]
    );
    assert_eq!(lines[2]["method"], "tools/call");
    assert_eq!(lines[2]["params"]["name"], WRITE_CYPHER);
}

#[test]
fn a_tool_error_surfaces_its_text() {
    let failure = json!({
        "content": [{"type": "text", "text": READ_ONLY_REJECTION}],
        "isError": true,
    });
    let (mut child, _facts) = started_child(&response_line(2, failure));
    let err = child
        .call(READ_CYPHER, json!({"statement": "CREATE (n)"}))
        .unwrap_err();
    assert_eq!(
        err,
        McpChildError::Tool {
            text: READ_ONLY_REJECTION.to_string()
        }
    );
}

#[test]
fn a_second_content_item_is_refused_not_dropped() {
    let two_items = json!({
        "content": [
            {"type": "text", "text": "{}"},
            {"type": "text", "text": "{}"},
        ],
        "isError": false,
    });
    let (mut child, _facts) = started_child(&response_line(2, two_items));
    let err = child.call(REPORT_OUTPUT, json!({"text": "x"})).unwrap_err();
    assert_eq!(
        err.to_string(),
        "malformed MCP response: the tools/call result must carry exactly one content item, got 2"
    );
}

#[test]
fn an_id_mismatch_is_a_pinned_refusal() {
    let (mut child, _facts) = started_child(&response_line(99, json!({})));
    let err = child.call(REPORT_OUTPUT, json!({"text": "x"})).unwrap_err();
    assert_eq!(
        err,
        McpChildError::IdMismatch {
            expected: 2,
            got: json!(99)
        }
    );
    assert_eq!(
        err.to_string(),
        "the host answered id 99, but the child sent id 2"
    );
}

#[test]
fn a_missing_envelope_is_a_pinned_refusal() {
    let err = Scripted::start::<Value>(Cursor::new(Vec::new()), Vec::new(), "greet", "0.1.0")
        .err()
        .unwrap();
    assert_eq!(
        err.to_string(),
        "invalid request envelope: the host wrote no request envelope"
    );
}

#[test]
fn a_protocol_mismatch_is_a_pinned_refusal() {
    let mut facts = facts_json();
    facts["protocolVersion"] = json!("1999-01-01");
    let mut input = String::new();
    input.push_str("{}\n");
    input.push_str(&response_line(1, facts));
    let err = Scripted::start::<Value>(
        Cursor::new(input.into_bytes()),
        Vec::new(),
        "greet",
        "0.1.0",
    )
    .err()
    .unwrap();
    assert_eq!(
        err.to_string(),
        format!(
            "the host speaks MCP protocol '1999-01-01'; this child speaks '{MCP_PROTOCOL_VERSION}'"
        )
    );
}

/// The `tools/call` requests a started child wrote, in order.
fn calls(written: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(written)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .filter(|line: &Value| line["method"] == METHOD_TOOLS_CALL)
        .collect()
}

/// A started child whose host answers every call with `result`.
fn answering_child<T>(answers: usize, result: T) -> McpChild<Cursor<Vec<u8>>, Vec<u8>>
where
    T: Fn() -> Value,
{
    answering_child_with(facts_json(), answers, result)
}

/// [`answering_child`] under facts the test spells — the same seam
/// [`started_child_with`] exposes for the policy-scoped guard tests.
fn answering_child_with<T>(
    facts: Value,
    answers: usize,
    result: T,
) -> McpChild<Cursor<Vec<u8>>, Vec<u8>>
where
    T: Fn() -> Value,
{
    let mut tail = String::new();
    for id in 2..(2 + answers as u64) {
        tail.push_str(&response_line(id, result()));
    }
    started_child_with(facts, &tail).0
}

#[test]
fn a_per_batch_child_streams_past_the_published_stream_cap() {
    // The whole-run stream cap guards the framing that retains work
    // across calls (ADR-0598); a per-batch session retains nothing
    // between calls, so every call ships however small the published
    // cap — the per-call caps and the store's live-graph limit are
    // that session's bounds, never a whole-run byte count.
    let facts = facts_under(PER_BATCH, 8);
    let mut child = answering_child_with(facts, 8, || tool_result("{\"counts\":{}}"));
    for _ in 0..8 {
        child
            .call(REPORT_OUTPUT, json!({"text": "chunk"}))
            .expect("a per-batch call ships whatever the whole-run cap says");
    }
    assert_eq!(
        calls(&child.out).len(),
        8,
        "the per-batch session streamed every call"
    );
}

#[test]
fn a_whole_import_child_refuses_past_the_published_stream_cap() {
    // Under whole-import framing the guard is on and reads the cap the
    // handshake published (ADR-0598): the calls stream until the
    // accounting crosses it, the refusal names the framing and the
    // remedy, and the refusing call never ships.
    let facts = facts_under(WHOLE_IMPORT, 512);
    let mut child = answering_child_with(facts, 64, || tool_result("{\"counts\":{}}"));
    let mut refusal = None;
    for _ in 0..64 {
        if let Err(error) = child.call(REPORT_OUTPUT, json!({"text": "chunk"})) {
            refusal = Some(error);
            break;
        }
    }
    let error = refusal.expect("the calls cross the 512-byte whole-import cap");
    assert_eq!(error, McpChildError::StreamTooLarge { limit: 512 });
    assert_eq!(
        error.to_string(),
        "the whole-import MCP request stream exceeds the 512-byte stream cap; \
         window the import or move the package to per-batch framing"
    );
    assert!(
        calls(&child.out).len() < 64,
        "the refusing call ships nothing"
    );
}

#[test]
fn write_rows_chunks_at_the_row_cap() {
    // More rows than one call carries: the overflow opens the next
    // call, every row lands, and no call exceeds the row cap.
    let ids = 0..(MAX_ROWS_PER_STATEMENT as i64 + 5);
    let expected_calls = 2;
    let mut child = answering_child(expected_calls, || tool_result("{\"counts\":{}}"));
    child
        .write_rows(
            "UNWIND $rows AS row CREATE (n:N {id: row.id})",
            ids.map(|id| json!({"id": id})),
        )
        .expect("the chunker writes both calls");
    let calls = calls(&child.out);
    assert_eq!(calls.len(), expected_calls, "{calls:?}");
    let rows = |call: &Value| {
        call["params"]["arguments"]["parameters"]["rows"]
            .as_array()
            .expect("the call carries a rows array")
            .len()
    };
    assert_eq!(rows(&calls[0]), MAX_ROWS_PER_STATEMENT);
    assert_eq!(rows(&calls[1]), 5);
    // Every call names the statement and the tool.
    for call in &calls {
        assert_eq!(call["params"]["name"], WRITE_CYPHER);
        assert_eq!(
            call["params"]["arguments"]["statement"],
            json!("UNWIND $rows AS row CREATE (n:N {id: row.id})")
        );
    }
}

#[test]
fn write_rows_joins_one_chunks_rows_under_the_statement_cap() {
    let mut child = answering_child(1, || tool_result("{\"counts\":{}}"));
    child
        .write_rows("X", [json!({"id": 1}), json!({"id": 2})].into_iter())
        .unwrap();
    let calls = calls(&child.out);
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0]["params"]["arguments"]["parameters"]["rows"],
        json!([{"id": 1}, {"id": 2}]),
        "one call carries the whole array, never a row per call"
    );
}

#[test]
fn a_row_over_the_cap_behind_a_nonempty_chunk_refuses() {
    // The hole the first-row test cannot see: a SMALL row already sits
    // in the chunk when the oversized one arrives, so the byte-cap
    // branch would flush the small chunk and then — without the
    // single-row bound — open a fresh chunk that ships the oversized
    // row over the cap. The refusal must land BEFORE anything ships.
    let mut child = answering_child(0, || tool_result("{\"counts\":{}}"));
    let huge = "x".repeat(MAX_STATEMENT_BYTES as usize);
    let rows = [json!({ "id": 0 }), json!({ "blob": huge })];
    let err = child.write_rows("X", rows.into_iter()).unwrap_err();
    assert_eq!(
        err,
        McpChildError::RowTooLarge {
            limit: MAX_STATEMENT_BYTES
        },
        "an oversized row behind a non-empty chunk is refused before any flush"
    );
    // Nothing shipped: the small chunk never left the child.
    assert!(calls(&child.out).is_empty());
}

#[test]
fn write_rows_writes_nothing_for_an_empty_phase() {
    // A zero-row phase is a no-op: no call, no transaction.
    let mut child = answering_child(0, || tool_result("{\"counts\":{}}"));
    child.write_rows("X", std::iter::empty()).unwrap();
    assert!(calls(&child.out).is_empty());
}

#[test]
fn a_single_row_over_the_statement_cap_refuses() {
    // A row whose serialized bytes alone fill the statement cap: no
    // chunk can carry it, and the refusal names the cap.
    let mut child = answering_child(0, || tool_result("{\"counts\":{}}"));
    let huge = "x".repeat(MAX_STATEMENT_BYTES as usize);
    let err = child
        .write_rows("X", [json!({"blob": huge})].into_iter())
        .unwrap_err();
    assert_eq!(
        err,
        McpChildError::RowTooLarge {
            limit: MAX_STATEMENT_BYTES
        }
    );
    assert_eq!(
        err.to_string(),
        format!("a single row exceeds the {MAX_STATEMENT_BYTES}-byte statement cap")
    );
    assert!(calls(&child.out).is_empty(), "nothing was sent");
}

/// The row the chunker measures is the row the call carries: the
/// byte arithmetic counts the array, both brackets included, so a
/// chunk that fits the cap is the chunk the host admits.
#[test]
fn the_counted_chunk_is_the_written_chunk() {
    let mut child = answering_child(1, || tool_result("{\"counts\":{}}"));
    let rows = [json!({"id": 1}), json!({"id": 2}), json!({"id": 3})];
    child.write_rows("X", rows.clone().into_iter()).unwrap();
    let calls = calls(&child.out);
    let sent = serde_json::to_string(&calls[0]["params"]["arguments"]["parameters"]["rows"])
        .expect("the array serializes");
    assert_eq!(
        sent,
        serde_json::to_string(&Value::Array(rows.to_vec())).unwrap()
    );
}

#[test]
fn update_schema_sends_the_ddl_statement() {
    let mut child = answering_child(1, || tool_result("{\"created\":[\"file_content\"]}"));
    let result = child
        .update_schema(
            "CREATE FULLTEXT INDEX file_content IF NOT EXISTS FOR (f:File) ON EACH [f.name]",
        )
        .unwrap();
    assert_eq!(result, json!({"created": ["file_content"]}));
    let calls = calls(&child.out);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["params"]["name"], UPDATE_SCHEMA);
}
