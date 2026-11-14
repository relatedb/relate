//! The five tools against a real in-memory store, their pinned per-call
//! refusals, and the caps re-anchored per call. Helpers are the tests
//! module's — one kit, not a copy.

use super::*;
// ---------------------------------------------------------------------
// The five tools
// ---------------------------------------------------------------------

#[test]
fn write_cypher_writes_the_rows_and_returns_the_counters() {
    let mut input = opened();
    input.push(call(
        2,
        mcp::WRITE_CYPHER,
        json!({
            "statement": "UNWIND $rows AS row CREATE (n:Greet {id: row.id, name: row.name})",
            "parameters": {"rows": [{"id": 1, "name": "a"}, {"id": 2, "name": "b"}]},
        }),
    ));
    let outcome = run_with(input, "per-batch", caps());
    assert!(outcome.result.is_ok(), "{:?}", outcome.result);
    let result = tool_result(&outcome.responses[1]);
    assert_eq!(result["counts"]["nodes_created"], json!(2));
    assert_eq!(result["counts"]["labels_added"], json!(2));
    assert_eq!(node_count(&outcome.db), 2);
}

/// One session's request lines: the handshake and one writing call.
fn one_write(statement: &str) -> Vec<String> {
    let mut lines: Vec<String> = opened().iter().map(Value::to_string).collect();
    lines.push(call(2, mcp::WRITE_CYPHER, json!({"statement": statement})).to_string());
    lines
}

/// The capacity a writing call reports is the host-owned store's own
/// reading after the write: `storeRemainingBytes` is `limit - used`, so a
/// child subtracting two results gets exactly the bytes its own call cost
/// between them (ADR-0442 §2).
#[test]
fn the_writing_results_carry_the_store_remaining_bytes_across_two_writes() {
    let limit = 1 << 20;
    let open = || {
        Database::open_with(
            ":memory:",
            OpenOptions {
                logical_graph_size_limit: Some(limit),
                ..Default::default()
            },
        )
        .unwrap()
    };
    let remaining = |outcome: &Outcome| {
        tool_result(&outcome.responses[1])["storeRemainingBytes"]
            .as_u64()
            .unwrap_or_else(|| panic!("a writing result carries storeRemainingBytes"))
    };

    let db = open();
    // A store is never at zero: the system graph is charged at open, so the
    // baseline is the store's own reading before the first call.
    let (opened_used, _) = db.memory_usage().unwrap();
    let first = run_raw_against(
        db,
        one_write("UNWIND range(1, 4) AS i CREATE (n:Greet {i: i})"),
        "per-batch",
        caps(),
    );
    assert!(first.result.is_ok(), "{:?}", first.result);
    // The handshake publishes the baseline: the cap and the store as opened.
    assert_eq!(
        first.responses[0]["result"]["capacity"],
        json!({
            "storeLimitBytes": limit,
            "storeUsedBytes": opened_used,
            "storeRemainingBytes": limit - opened_used,
        })
    );
    let remaining_first = remaining(&first);
    let (used_first, reported_limit) = first.db.memory_usage().unwrap();
    assert_eq!(reported_limit, Some(limit));
    assert_eq!(
        remaining_first,
        limit - used_first,
        "the result is the store's own reading after the write"
    );

    // The child's second session: the store it left, and one more write.
    let second = run_raw_against(
        first.db,
        one_write("UNWIND range(5, 6) AS i CREATE (n:Greet {i: i})"),
        "per-batch",
        caps(),
    );
    assert!(second.result.is_ok(), "{:?}", second.result);
    let remaining_second = remaining(&second);
    let (used_second, _) = second.db.memory_usage().unwrap();
    assert_eq!(
        remaining_second,
        limit - used_second,
        "the second result is the store's own reading too"
    );

    // The store grew between the calls, and the delta of the reported
    // remaining bytes is exactly that growth — the child's own usage.
    assert!(
        used_second > used_first,
        "the second write must grow the store"
    );
    assert_eq!(remaining_first - remaining_second, used_second - used_first);
}

#[test]
fn write_cypher_passes_plain_parameters() {
    let mut input = opened();
    input.push(call(
        2,
        mcp::WRITE_CYPHER,
        json!({"statement": "CREATE (n:Point {name: $name})", "parameters": {"name": "origin"}}),
    ));
    let outcome = run_with(input, "per-batch", caps());
    let result = tool_result(&outcome.responses[1]);
    assert_eq!(result["counts"]["nodes_created"], json!(1));
}

#[test]
fn read_cypher_returns_columns_and_rows() {
    let mut input = opened();
    input.push(call(
        2,
        mcp::WRITE_CYPHER,
        json!({
            "statement": "UNWIND $rows AS row CREATE (n:Greet {name: row.name})",
            "parameters": {"rows": [{"name": "b"}, {"name": "a"}]},
        }),
    ));
    input.push(call(
        3,
        mcp::READ_CYPHER,
        json!({"statement": "MATCH (n:Greet) RETURN n.name AS name ORDER BY n.name"}),
    ));
    let outcome = run_with(input, "per-batch", caps());
    let result = tool_result(&outcome.responses[2]);
    assert_eq!(result["columns"], json!(["name"]));
    assert_eq!(result["rows"], json!([{"name": "a"}, {"name": "b"}]));
}

#[test]
fn read_cypher_rejects_a_writing_statement() {
    let mut input = opened();
    input.push(call(
        2,
        mcp::READ_CYPHER,
        json!({"statement": "CREATE (n:Greet)"}),
    ));
    let outcome = run_with(input, "per-batch", caps());
    let (code, message) = refusal(&outcome.responses[1]);
    assert_eq!(code, mcp::INVALID_PARAMS);
    assert_eq!(message, READ_ONLY_REJECTION);
    assert_eq!(node_count(&outcome.db), 0);
}

/// LABS-888: the mount's `read_cypher` — the shared wire table's read tool
/// — admits an OPTIONAL MATCH read. The repro classifies `r` (the access
/// fold reads OPTIONAL MATCH as the read its plain MATCH counterpart is),
/// so the gate passes and both arms come back: the :Room with a :NORTH
/// neighbour reports its neighbour's name, and the rooms without one — the
/// isolated :Room and the neighbour :Room, a `:Room` with no outgoing
/// :NORTH — report null. Like the product server's pin (the stdio suite's
/// `stdio_read_cypher_serves_an_optional_match_read`), the statement stays
/// the bare repro and the rows are asserted order-insensitively.
#[test]
fn read_cypher_serves_an_optional_match_read() {
    let mut input = opened();
    input.push(call(
        2,
        mcp::WRITE_CYPHER,
        json!({"statement": "CREATE (:Room {name: 'A'})-[:NORTH]->(:Room {name: 'B'}), (:Room {name: 'C'})"}),
    ));
    input.push(call(
        3,
        mcp::READ_CYPHER,
        json!({"statement": "MATCH (a:Room) OPTIONAL MATCH (a)-[:NORTH]->(b) RETURN a.name, b.name"}),
    ));
    let outcome = run_with(input, "per-batch", caps());
    assert!(outcome.result.is_ok(), "{:?}", outcome.result);
    let result = tool_result(&outcome.responses[2]);
    assert_eq!(result["columns"], json!(["a.name", "b.name"]));
    let rows = result["rows"]
        .as_array()
        .unwrap_or_else(|| panic!("the optional-match read returns rows: {result}"));
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
        "{result}"
    );
}

#[test]
fn update_schema_reports_the_created_names() {
    let mut input = opened();
    input.push(call(
        2,
        mcp::UPDATE_SCHEMA,
        json!({"statement": "CREATE CONSTRAINT greet_id_unique FOR (n:Greet) REQUIRE n.id IS UNIQUE"}),
    ));
    let outcome = run_with(input, "per-batch", caps());
    assert!(outcome.result.is_ok(), "{:?}", outcome.result);
    let result = tool_result(&outcome.responses[1]);
    let created: Vec<&str> = result["created"]
        .as_array()
        .unwrap()
        .iter()
        .map(|name| name.as_str().unwrap())
        .collect();
    assert!(
        created.contains(&"greet_id_unique"),
        "the constraint name must be reported: {created:?}"
    );
}

#[test]
fn update_schema_rejects_a_data_write_and_a_read() {
    // The DDL guard: the plan's own type decides, exactly as read_cypher's
    // read guard does. Neither statement may touch the graph or the catalog.
    let mut input = opened();
    input.push(call(
        2,
        mcp::UPDATE_SCHEMA,
        json!({"statement": "CREATE (n:Greet)"}),
    ));
    input.push(call(
        3,
        mcp::UPDATE_SCHEMA,
        json!({"statement": "SHOW INDEXES YIELD name"}),
    ));
    let outcome = run_with(input, "per-batch", caps());
    assert!(outcome.result.is_ok(), "{:?}", outcome.result);
    for response in &outcome.responses[1..] {
        let (code, message) = refusal(response);
        assert_eq!(code, mcp::INVALID_PARAMS);
        assert_eq!(message, mcp::SCHEMA_ONLY_REJECTION);
    }
    assert_eq!(node_count(&outcome.db), 0);
}

#[test]
fn batch_cypher_returns_the_counters_in_order() {
    let mut input = opened();
    input.push(call(
        2,
        mcp::BATCH_CYPHER,
        json!({"statements": [
            {"statement": "CREATE (n:First)"},
            {"statement": "CREATE (n:Second)"},
        ]}),
    ));
    let outcome = run_with(input, "per-batch", caps());
    let result = tool_result(&outcome.responses[1]);
    let results = result["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["nodes_created"], json!(1));
    assert_eq!(results[1]["nodes_created"], json!(1));
    // The batch closes with the store's remaining bytes, like a single
    // write — the uncapped store counts down from u64::MAX. Read the store
    // before any later query: a read materializes statistics it charges.
    let (used, _) = outcome.db.memory_usage().unwrap();
    assert_eq!(
        result["storeRemainingBytes"],
        json!(u64::MAX - used),
        "the batch result reports the store's own reading after the call"
    );
    assert_eq!(node_count(&outcome.db), 2);
}

#[test]
fn batch_cypher_rolls_the_whole_call_back_when_a_statement_fails() {
    let mut input = opened();
    input.push(call(
        2,
        mcp::BATCH_CYPHER,
        json!({"statements": [
            {"statement": "CREATE (n:First)"},
            {"statement": "CREATE (n:Second {id: $missing})"},
        ]}),
    ));
    let outcome = run_with(input, "per-batch", caps());
    let (code, message) = refusal(&outcome.responses[1]);
    assert_eq!(code, mcp::INVALID_PARAMS);
    assert!(message.starts_with("engine error: "), "{message}");
    // One transaction around the call: the first statement rolled back.
    assert_eq!(node_count(&outcome.db), 0);
}

/// A result that would not fit one response line the child can read is a
/// typed refusal, not a line the child's capped read rejects as malformed.
#[test]
fn a_result_over_the_response_cap_is_refused() {
    let tight = Caps {
        statement_bytes: 128,
        ..Caps::MOUNT
    };
    let mut input = opened();
    input.push(call(
        2,
        mcp::READ_CYPHER,
        json!({"statement": "UNWIND range(1, 50) AS i RETURN i AS n"}),
    ));
    let outcome = run_with(input, "per-batch", tight);
    assert!(outcome.result.is_ok(), "{:?}", outcome.result);
    let (code, message) = refusal(&outcome.responses[1]);
    assert_eq!(code, mcp::INVALID_PARAMS);
    assert_eq!(
        message,
        "the tools/call result would exceed the 128-byte response cap"
    );
}

/// The shipped response cap is the kit's statement cap: the bound the
/// child's own capped response read runs under.
#[test]
fn the_shipped_response_cap_is_the_statement_cap() {
    assert_eq!(
        caps().mcp_response_bytes(),
        relate_wire::MAX_STATEMENT_BYTES
    );
}

#[test]
fn report_output_stores_the_summary_and_starters() {
    let mut input = opened();
    input.push(call(
        2,
        mcp::REPORT_OUTPUT,
        json!({"text": "imported 2 nodes", "starters": ["MATCH (n:Greet) RETURN n"]}),
    ));
    let outcome = run_with(input, "per-batch", caps());
    let summary = outcome
        .result
        .unwrap()
        .expect("the session reported a summary");
    assert_eq!(summary.text, "imported 2 nodes");
    assert_eq!(summary.starters, ["MATCH (n:Greet) RETURN n"]);
    assert_eq!(tool_result(&outcome.responses[1]), json!({}));
}

// ---------------------------------------------------------------------
// The pinned per-call refusals
// ---------------------------------------------------------------------

#[test]
fn an_unknown_method_is_a_method_not_found_refusal() {
    let mut input = opened();
    input.push(request(2, "resources/list", json!({})));
    let outcome = run_with(input, "per-batch", caps());
    let (code, message) = refusal(&outcome.responses[1]);
    assert_eq!(code, mcp::METHOD_NOT_FOUND);
    assert_eq!(message, "unknown method 'resources/list'");
}

#[test]
fn an_unknown_tool_is_an_invalid_params_refusal() {
    let mut input = opened();
    input.push(call(2, "drop_everything", json!({})));
    let outcome = run_with(input, "per-batch", caps());
    let (code, message) = refusal(&outcome.responses[1]);
    assert_eq!(code, mcp::INVALID_PARAMS);
    assert_eq!(message, "unknown tool 'drop_everything'");
}

#[test]
fn a_malformed_line_is_a_parse_error_and_the_session_continues() {
    let lines = vec![
        "this is not json".to_string(),
        initialize(1).to_string(),
        request(2, mcp::METHOD_TOOLS_LIST, json!({})).to_string(),
    ];
    let outcome = run_raw(lines, "per-batch", caps());
    assert!(outcome.result.is_ok(), "{:?}", outcome.result);
    let (code, message) = refusal(&outcome.responses[0]);
    assert_eq!(code, mcp::PARSE_ERROR);
    assert!(
        message.starts_with("malformed JSON-RPC message: "),
        "{message}"
    );
    assert_eq!(outcome.responses[1]["id"], json!(1));
    assert!(outcome.responses[2]["result"]["tools"].is_array());
}

/// A body that omits the `jsonrpc` member never decodes: the refusal is the
/// shared strict decode's pinned text and code, the same pair the product
/// MCP server answers with.
#[test]
fn a_body_without_the_jsonrpc_member_is_a_parse_error_refusal() {
    let lines = vec![
        r#"{"id":7,"method":"ping"}"#.to_string(),
        initialize(1).to_string(),
        request(2, mcp::METHOD_TOOLS_LIST, json!({})).to_string(),
    ];
    let outcome = run_raw(lines, "per-batch", caps());
    assert!(outcome.result.is_ok(), "{:?}", outcome.result);
    let (code, message) = refusal(&outcome.responses[0]);
    assert_eq!(code, mcp::PARSE_ERROR);
    assert_eq!(
        message,
        "malformed JSON-RPC message: missing field `jsonrpc` at line 1 column 24"
    );
    assert_eq!(outcome.responses[1]["id"], json!(1));
    assert!(outcome.responses[2]["result"]["tools"].is_array());
}

#[test]
fn a_missing_statement_is_an_invalid_params_refusal() {
    let mut input = opened();
    input.push(call(2, mcp::WRITE_CYPHER, json!({})));
    let outcome = run_with(input, "per-batch", caps());
    let (code, message) = refusal(&outcome.responses[1]);
    assert_eq!(code, mcp::INVALID_PARAMS);
    assert_eq!(message, "invalid params: missing string \"statement\"");
}

// ---------------------------------------------------------------------
// The caps (re-anchored per call)
// ---------------------------------------------------------------------

#[test]
fn the_statement_cap_refuses_before_the_engine() {
    let tight = Caps {
        statement_bytes: 32,
        ..Caps::MOUNT
    };
    let mut input = opened();
    input.push(call(
        2,
        mcp::WRITE_CYPHER,
        json!({"statement": "CREATE (n:ThisStatementIsLongerThanTheCap)"}),
    ));
    let outcome = run_with(input, "per-batch", tight);
    let (code, message) = refusal(&outcome.responses[1]);
    assert_eq!(code, mcp::INVALID_PARAMS);
    assert_eq!(message, "the statement exceeds the 32-byte statement cap");
    assert_eq!(node_count(&outcome.db), 0);
}

#[test]
fn the_rows_cap_refuses_before_the_engine() {
    let tight = Caps {
        rows_per_statement: 1,
        ..Caps::MOUNT
    };
    let mut input = opened();
    input.push(call(
        2,
        mcp::WRITE_CYPHER,
        json!({
            "statement": "UNWIND $rows AS row CREATE (n:Greet {id: row.id})",
            "parameters": {"rows": [{"id": 1}, {"id": 2}]},
        }),
    ));
    let outcome = run_with(input, "per-batch", tight);
    let (code, message) = refusal(&outcome.responses[1]);
    assert_eq!(code, mcp::INVALID_PARAMS);
    assert_eq!(message, "the call carries 2 rows, over the 1-row cap");
    assert_eq!(node_count(&outcome.db), 0);
}

#[test]
fn the_statements_cap_refuses_the_batch() {
    let tight = Caps {
        statements: 1,
        ..Caps::MOUNT
    };
    let mut input = opened();
    input.push(call(
        2,
        mcp::BATCH_CYPHER,
        json!({"statements": [{"statement": "CREATE (n:A)"}, {"statement": "CREATE (n:B)"}]}),
    ));
    let outcome = run_with(input, "per-batch", tight);
    let (code, message) = refusal(&outcome.responses[1]);
    assert_eq!(code, mcp::INVALID_PARAMS);
    assert_eq!(
        message,
        "the call carries 2 statements, over the 1-statement cap"
    );
    assert_eq!(node_count(&outcome.db), 0);
}

/// A statement of exactly `bytes` bytes: one tiny read padded with trailing
/// spaces. The result stays small on purpose — the record cap bounds a
/// response line too, so a padded *result* would trip the response cap
/// instead of framing the statement.
fn statement_at(bytes: usize) -> String {
    let prefix = "RETURN 1 AS s";
    assert!(bytes > prefix.len(), "the padding must be positive");
    format!("{prefix}{}", " ".repeat(bytes - prefix.len()))
}

#[test]
fn a_statement_at_the_published_cap_is_framed_and_runs() {
    // The published `maxStatementBytes` is a promise the framing keeps: a
    // statement of exactly the cap still fits its request line, because the
    // framing bound is derived from it (cap + the envelope allowance). The
    // cap also bounds responses, so it stays above one row's JSON.
    let tight = Caps {
        statement_bytes: 256,
        ..Caps::MOUNT
    };
    let mut input = opened();
    input.push(call(
        2,
        mcp::READ_CYPHER,
        json!({"statement": statement_at(256)}),
    ));
    let outcome = run_with(input, "per-batch", tight);
    assert!(outcome.result.is_ok(), "{:?}", outcome.result);
    let result = tool_result(&outcome.responses[1]);
    assert_eq!(result["rows"], json!([{"s": 1}]));
}

#[test]
fn a_statement_one_byte_over_the_published_cap_is_refused() {
    let tight = Caps {
        statement_bytes: 64,
        ..Caps::MOUNT
    };
    let mut input = opened();
    input.push(call(
        2,
        mcp::READ_CYPHER,
        json!({"statement": statement_at(65)}),
    ));
    let outcome = run_with(input, "per-batch", tight);
    let (code, message) = refusal(&outcome.responses[1]);
    assert_eq!(code, mcp::INVALID_PARAMS);
    assert_eq!(message, "the statement exceeds the 64-byte statement cap");
}

#[test]
fn the_record_cap_is_fatal() {
    let tight = Caps {
        statement_bytes: 16,
        ..Caps::MOUNT
    };
    let limit = tight.mcp_request_bytes();
    let oversized = "x".repeat(limit as usize + 1);
    let outcome = run_raw(
        vec![initialize(1).to_string(), oversized],
        "per-batch",
        tight,
    );
    assert_eq!(
        outcome.result.unwrap_err().to_string(),
        format!("the extension MCP request exceeds the {limit}-byte record cap")
    );
}
