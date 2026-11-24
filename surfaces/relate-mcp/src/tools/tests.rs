//! The shared tool layer's behavior tests (the dir-module pattern, LABS-421):
//! one implementation, one gate, one vocabulary — asserted here so both
//! surfaces' suites can stay about their own transport and lifecycle.

use super::*;

/// A fresh in-memory store and the connection the tool layer runs over.
fn store() -> (Database, Connection) {
    let db = Database::open(":memory:").unwrap();
    let conn = db.connect().unwrap();
    (db, conn)
}

/// The object of one inline arguments literal, as the tool layer takes it.
macro_rules! args {
    ($($json:tt)*) => {
        serde_json::json!($($json)*)
            .as_object()
            .expect("an object literal")
    };
}

/// Run one tool call over a fresh store, returning the outcome.
fn run(name: &str, arguments: Json) -> Result<ToolOutcome, ToolError> {
    let (_db, mut conn) = store();
    Tools::new(&mut conn, Limits::shipped()).call(name, arguments.as_object().expect("an object"))
}

/// The counters of the first result of a written statement.
fn created(outcome: &ToolOutcome) -> i64 {
    let ToolOutcome::Query(outcome) = outcome else {
        panic!("a cypher call answers a query outcome");
    };
    outcome.result.stats.nodes_created
}

#[test]
fn write_cypher_executes_the_statement_and_the_row_parameter() {
    let outcome = run(
        WRITE_CYPHER,
        serde_json::json!({
            "statement": "UNWIND $rows AS row CREATE (n:N {id: row.id})",
            "parameters": {"rows": [{"id": 1}, {"id": 2}]},
        }),
    )
    .unwrap();
    assert_eq!(created(&outcome), 2);
}

#[test]
fn read_cypher_returns_the_rows_and_refuses_a_write() {
    let outcome = run(
        READ_CYPHER,
        serde_json::json!({"statement": "RETURN 1 AS one"}),
    )
    .unwrap();
    let ToolOutcome::Query(outcome) = outcome else {
        panic!("a read answers a query outcome");
    };
    assert_eq!(outcome.result.rows.len(), 1);

    assert_eq!(
        run(
            READ_CYPHER,
            serde_json::json!({"statement": "CREATE (n:Person {name: 'Ada'})"}),
        )
        .unwrap_err(),
        ToolError::ReadOnly
    );
    // Schema DDL mutates the catalog: the read tool refuses it too.
    assert_eq!(
        run(
            READ_CYPHER,
            serde_json::json!({"statement": "CREATE INDEX person_name FOR (n:Person) ON (n.name)"}),
        )
        .unwrap_err(),
        ToolError::ReadOnly
    );
}

#[test]
fn write_cypher_refuses_a_pure_read() {
    assert_eq!(
        run(
            WRITE_CYPHER,
            serde_json::json!({"statement": "MATCH (n) RETURN n"}),
        )
        .unwrap_err(),
        ToolError::WriteOnly
    );
}

#[test]
fn update_schema_reports_the_created_names_and_refuses_a_data_write() {
    let outcome = run(
        UPDATE_SCHEMA,
        serde_json::json!({"statement": "CREATE INDEX n_id FOR (n:N) ON (n.id)"}),
    )
    .unwrap();
    let ToolOutcome::Created(created) = outcome else {
        panic!("a DDL call answers the created names");
    };
    assert_eq!(created, ["n_id"]);

    for statement in ["CREATE (n:N)", "MATCH (n) RETURN n"] {
        assert_eq!(
            run(UPDATE_SCHEMA, serde_json::json!({"statement": statement})).unwrap_err(),
            ToolError::SchemaOnly,
            "{statement}"
        );
    }
}

#[test]
fn batch_cypher_runs_every_statement_in_order() {
    let outcome = run(
        BATCH_CYPHER,
        serde_json::json!({"statements": [
            {"statement": "CREATE (n:Batch {slot: 1})"},
            {"statement": "CREATE (n:Batch {slot: 2})"},
        ]}),
    )
    .unwrap();
    let ToolOutcome::Batch(results) = outcome else {
        panic!("a batch call answers the per-statement results");
    };
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|result| result.stats.nodes_created == 1));
}

#[test]
fn an_unknown_tool_is_the_pinned_refusal() {
    assert_eq!(
        run("drop_everything", serde_json::json!({})).unwrap_err(),
        ToolError::UnknownTool {
            name: "drop_everything".to_string()
        }
    );
    // `get_schema` and `report_output` are not statement tools: the former
    // reads the store handle, the latter is the extension mount's lifecycle
    // call, so neither is dispatched here.
    assert!(matches!(
        run(GET_SCHEMA, serde_json::json!({})).unwrap_err(),
        ToolError::UnknownTool { .. }
    ));
    assert!(matches!(
        run(REPORT_OUTPUT, serde_json::json!({"text": "x"})).unwrap_err(),
        ToolError::UnknownTool { .. }
    ));
}

/// ADR-0610's `tx_metadata` argument on the two writing tools: on a store
/// opened without the `value-provenance` opt-in a call carrying it answers
/// the authored preview error through the shared engine vocabulary —
/// never a silent drop. The open side — the map reaching the commit's
/// transaction record — is the gated binary `tests/tx_metadata.rs`.
#[test]
fn the_writing_tools_tx_metadata_is_the_authored_preview_error_while_off() {
    relate::test_opts::assert_preview_unset();
    for name in [WRITE_CYPHER, BATCH_CYPHER] {
        let arguments = if name == WRITE_CYPHER {
            serde_json::json!({"statement": "CREATE (n:N)", "tx_metadata": {"by": "app"}})
        } else {
            serde_json::json!({"statements": [{"statement": "CREATE (n:N)"}],
                               "tx_metadata": {"by": "app"}})
        };
        let error = run(name, arguments).unwrap_err();
        assert!(
            matches!(error, ToolError::Engine { .. }),
            "{name}: an engine refusal, got {error:?}"
        );
        assert!(
            error.to_string().contains(
                "preview feature 'value-provenance' is not enabled: \
                 set RELATE_PREVIEW=value-provenance"
            ),
            "{name}: {error}"
        );
    }
    // A call that carries no metadata is byte-for-byte today's: the two
    // tools run their writes with no channel cost while off.
    assert_eq!(
        run(
            WRITE_CYPHER,
            serde_json::json!({"statement": "CREATE (n:N)"}),
        )
        .map(|outcome| created(&outcome)),
        Ok(1)
    );
}

/// A `tx_metadata` that is not an object is the pinned invalid-params
/// refusal naming the member — the same vocabulary the `parameters`
/// argument answers.
#[test]
fn a_non_object_tx_metadata_is_the_invalid_params_refusal() {
    for name in [WRITE_CYPHER, BATCH_CYPHER] {
        let arguments = if name == WRITE_CYPHER {
            serde_json::json!({"statement": "CREATE (n:N)", "tx_metadata": "by=app"})
        } else {
            serde_json::json!({"statements": [{"statement": "CREATE (n:N)"}],
                               "tx_metadata": "by=app"})
        };
        let error = run(name, arguments).unwrap_err();
        assert!(
            matches!(error, ToolError::InvalidParams { .. }),
            "{name}: {error:?}"
        );
        assert!(
            error
                .to_string()
                .contains("\"tx_metadata\" must be an object"),
            "{name}: {error}"
        );
    }
}

#[test]
fn the_caps_refuse_before_the_engine() {
    let (_db, mut conn) = store();
    let limits = Limits {
        max_statement_bytes: 16,
        max_rows: 2,
        max_statements: 1,
    };
    let mut tools = Tools::new(&mut conn, limits);
    assert_eq!(
        tools
            .call(
                WRITE_CYPHER,
                args!({"statement": "UNWIND $rows AS row CREATE (n:N {id: row.id})"}),
            )
            .unwrap_err(),
        ToolError::StatementTooLarge { limit: 16 }
    );
    assert_eq!(
        tools
            .call(
                WRITE_CYPHER,
                args!({
                    "statement": "X",
                    "parameters": {"rows": [{"id": 1}, {"id": 2}, {"id": 3}]},
                }),
            )
            .unwrap_err(),
        ToolError::TooManyRows { count: 3, limit: 2 }
    );
    assert_eq!(
        tools
            .call(
                BATCH_CYPHER,
                args!({"statements": [{"statement": "X"}, {"statement": "Y"}]}),
            )
            .unwrap_err(),
        ToolError::TooManyStatements { count: 2, limit: 1 }
    );
}

#[test]
fn a_malformed_payload_names_what_is_missing() {
    let (_db, mut conn) = store();
    let mut tools = Tools::new(&mut conn, Limits::shipped());
    for (call, detail) in [
        (
            tools.call(WRITE_CYPHER, args!({})),
            "missing string \"statement\"",
        ),
        (
            tools.call(
                WRITE_CYPHER,
                args!({"statement": "RETURN 1", "parameters": 4}),
            ),
            "\"parameters\" must be an object",
        ),
        (
            tools.call(BATCH_CYPHER, args!({})),
            "missing array \"statements\"",
        ),
        (
            tools.call(BATCH_CYPHER, args!({"statements": [4]})),
            "statements[0] must be an object",
        ),
    ] {
        assert_eq!(
            call.unwrap_err(),
            ToolError::InvalidParams {
                detail: detail.to_string()
            }
        );
    }
}

/// The tool name and arguments of one `tools/call` — the one extraction both
/// surfaces run, and its refusals.
#[test]
fn call_parts_reads_the_name_and_arguments() {
    let params = serde_json::json!({"name": "read_cypher", "arguments": {"statement": "RETURN 1"}});
    let (name, arguments) = call_parts(Some(&params)).unwrap();
    assert_eq!(name, READ_CYPHER);
    assert_eq!(arguments, args!({"statement": "RETURN 1"}));
    // Absent or null arguments is the empty call, not a refusal.
    for params in [
        serde_json::json!({"name": "get_schema"}),
        serde_json::json!({"name": "get_schema", "arguments": null}),
    ] {
        assert!(call_parts(Some(&params)).unwrap().1.is_empty());
    }
    // A non-object arguments is refused in the one shared wording.
    assert_eq!(
        call_parts(Some(
            &serde_json::json!({"name": "read_cypher", "arguments": 4})
        ))
        .unwrap_err(),
        ToolError::InvalidParams {
            detail: "\"arguments\" must be an object".to_string()
        }
    );
    for params in [
        Json::Null,
        serde_json::json!({}),
        serde_json::json!({"name": 4}),
    ] {
        assert_eq!(
            call_parts(Some(&params)).unwrap_err(),
            ToolError::InvalidParams {
                detail: "missing string \"name\"".to_string()
            }
        );
    }
    assert_eq!(
        call_parts(Some(&serde_json::json!([1]))).unwrap_err(),
        ToolError::InvalidParams {
            detail: "\"params\" must be an object".to_string()
        }
    );
}

/// The framing fact is the table's: a tool that is not read-only writes.
#[test]
fn writes_reads_the_one_tool_table() {
    assert!(writes(WRITE_CYPHER));
    assert!(writes(BATCH_CYPHER));
    assert!(writes(UPDATE_SCHEMA));
    assert!(!writes(READ_CYPHER));
    assert!(!writes(GET_SCHEMA));
    assert!(!writes(REPORT_OUTPUT));
    // An unserved name is not a write — its dispatch refuses it first.
    assert!(!writes("drop_everything"));
}

#[test]
fn the_refusal_texts_are_the_pinned_vocabulary() {
    assert_eq!(ToolError::ReadOnly.to_string(), READ_ONLY_REJECTION);
    assert_eq!(ToolError::WriteOnly.to_string(), WRITE_ONLY_REJECTION);
    assert_eq!(ToolError::SchemaOnly.to_string(), SCHEMA_ONLY_REJECTION);
    assert_eq!(
        ToolError::UnknownTool {
            name: "x".to_string()
        }
        .to_string(),
        "unknown tool 'x'"
    );
    assert_eq!(
        ToolError::InvalidParams {
            detail: "missing string \"statement\"".to_string()
        }
        .to_string(),
        "invalid params: missing string \"statement\""
    );
    assert_eq!(
        ToolError::Engine {
            detail: "boom".to_string()
        }
        .to_string(),
        "engine error: boom"
    );
    // Every tool refusal is the caller's payload, never a protocol fault.
    assert_eq!(ToolError::ReadOnly.code(), mcp::INVALID_PARAMS);
}

#[test]
fn the_shipped_limits_are_the_kit_numbers() {
    let limits = Limits::shipped();
    assert_eq!(limits.max_statement_bytes, relate_wire::MAX_STATEMENT_BYTES);
    assert_eq!(limits.max_rows, relate_wire::MAX_ROWS_PER_STATEMENT);
    assert_eq!(limits.max_statements, relate_wire::MAX_STATEMENTS);
    assert_eq!(Limits::default(), limits);
}

#[test]
fn get_schema_lists_a_labels_properties() {
    let (db, mut conn) = store();
    conn.query("CREATE (n:Person {name: 'Ada'})").unwrap();
    let schema: Json =
        serde_json::from_str(&get_schema_json(&db, Some(100)).unwrap()).expect("a schema map");
    assert!(
        schema.to_string().contains("Person"),
        "the schema names the label: {schema}"
    );
    // An explicit full scan is the same shape.
    assert!(get_schema_json(&db, None).is_ok());
}

// ---------------------------------------------------------------------
// The product server's face: the same table, its two policies on top
// ---------------------------------------------------------------------

/// A server over a fresh in-memory store.
fn server() -> McpServer {
    McpServer::new(
        Database::open(":memory:").unwrap(),
        crate::server::McpServerConfig::default(),
    )
}

fn tool_names(specs: &Json) -> Vec<&str> {
    specs["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect()
}

#[test]
fn the_server_lists_the_product_rows_under_its_namespace() {
    let specs = tool_specs(&server());
    assert_eq!(tool_names(&specs), [WRITE_CYPHER, READ_CYPHER, GET_SCHEMA]);
    // The kit's annotations, not a per-surface copy.
    assert_eq!(
        specs["tools"][1]["annotations"]["readOnlyHint"],
        serde_json::json!(true)
    );
    assert_eq!(
        specs["tools"][0]["annotations"]["destructiveHint"],
        serde_json::json!(true)
    );
    // The read tool's input schema is the shared one: a statement and its
    // parameters.
    assert_eq!(
        specs["tools"][1]["inputSchema"]["required"],
        serde_json::json!(["statement"])
    );

    let namespaced = McpServer::new(
        Database::open(":memory:").unwrap(),
        crate::server::McpServerConfig {
            namespace: "db1".to_string(),
            ..crate::server::McpServerConfig::default()
        },
    );
    assert_eq!(
        tool_names(&tool_specs(&namespaced)),
        [
            format!("db1-{WRITE_CYPHER}"),
            format!("db1-{READ_CYPHER}"),
            format!("db1-{GET_SCHEMA}")
        ]
    );
}

#[test]
fn a_read_only_server_hides_and_rejects_the_writing_tool() {
    let read_only = McpServer::new(
        Database::open(":memory:").unwrap(),
        crate::server::McpServerConfig {
            read_only: true,
            ..crate::server::McpServerConfig::default()
        },
    );
    assert_eq!(
        tool_names(&tool_specs(&read_only)),
        [READ_CYPHER, GET_SCHEMA]
    );
    assert_eq!(
        call_tool(&read_only, WRITE_CYPHER, args!({"statement": "CREATE (n)"}),).unwrap_err(),
        format!("unknown tool '{WRITE_CYPHER}'")
    );
}

/// The tool results keep the product server's envelope contract: rows under
/// `rows` with the engine's column types for a read, the counters under
/// `stats` for a write.
#[test]
fn the_tool_results_carry_the_canonical_envelope() {
    let server = server();
    let counters = call_tool(
        &server,
        WRITE_CYPHER,
        args!({
            "statement": "CREATE (n:Person {name: $name})",
            "parameters": {"name": "Ada"},
        }),
    )
    .unwrap();
    let counters: Json = serde_json::from_str(&counters).unwrap();
    assert_eq!(counters["stats"]["nodes_created"], serde_json::json!(1));

    let rows = call_tool(
        &server,
        READ_CYPHER,
        args!({"statement": "MATCH (n:Person) RETURN n.name AS name"}),
    )
    .unwrap();
    let rows: Json = serde_json::from_str(&rows).unwrap();
    assert_eq!(rows["rows"], serde_json::json!([{"name": "Ada"}]));
    assert_eq!(rows["columnTypes"], serde_json::json!(["STRING"]));
    // The classification plan rides the result (LABS-418, ADR-0381).
    assert_eq!(rows["plan"]["schema"], serde_json::json!("relate.plan/v1"));
}

/// Virtual plan operators: the classification plan an index-seek-started
/// MATCH rides lists its fused children — the seek step and one step per
/// hop — each naming the fused parent operator's id.
#[test]
fn the_plan_lists_the_fused_children_of_an_index_seek_match() {
    let server = server();
    for statement in relate_test_support::fused_seek::FUSED_SEEK_SEED {
        call_tool(&server, WRITE_CYPHER, args!({ "statement": statement })).unwrap();
    }
    let rows = call_tool(
        &server,
        READ_CYPHER,
        args!({
            "statement": relate_test_support::fused_seek::FUSED_SEEK_MATCH
        }),
    )
    .unwrap();
    let rows: Json = serde_json::from_str(&rows).unwrap();
    let seek = &rows["plan"]["root"]["children"][0];
    assert!(
        seek["fused_into"].is_null(),
        "the parent is a real operator"
    );
    let fused = seek["children"].as_array().expect("fused children");
    assert_eq!(fused.len(), 2, "the seek step plus one hop: {seek:?}");
    let parent_id = seek["children"][0]["fused_into"]
        .as_i64()
        .expect("parent id");
    assert!(fused.iter().all(|c| c["fused_into"] == parent_id));
}

/// ADR-0491: both cypher tools carry the executed statement's phase split —
/// `parse_ms` / `plan_ms` / `execute_ms` beside the envelope's
/// `elapsed_ms` — as non-negative milliseconds whose sum never exceeds
/// the total (§4: the phases partition the window the total spans).
#[test]
fn the_tool_results_carry_the_phase_split_beside_elapsed_ms() {
    let server = server();
    let rows = call_tool(&server, READ_CYPHER, args!({"statement": "RETURN 1 AS n"})).unwrap();
    let envelope: Json = serde_json::from_str(&rows).unwrap();
    let elapsed = envelope["elapsed_ms"].as_f64().expect("elapsed_ms");
    let mut sum = 0.0;
    for key in ["parse_ms", "plan_ms", "execute_ms"] {
        let ms = envelope[key]
            .as_f64()
            .unwrap_or_else(|| panic!("{key} must be a JSON number: {envelope}"));
        assert!(ms >= 0.0, "{key} is a non-negative duration: {ms}");
        sum += ms;
    }
    assert!(
        sum <= elapsed,
        "the phases partition the total's window (ADR-0491 §4): {sum} > {elapsed}"
    );
}

/// A guard refusal reaches the client as the shared text, and an engine
/// failure keeps the server's `Error:` convention.
#[test]
fn the_guard_and_engine_refusals_keep_their_texts() {
    let server = server();
    assert_eq!(
        call_tool(&server, READ_CYPHER, args!({"statement": "CREATE (n)"}),).unwrap_err(),
        READ_ONLY_REJECTION
    );
    assert_eq!(
        call_tool(
            &server,
            WRITE_CYPHER,
            args!({"statement": "MATCH (n) RETURN n"}),
        )
        .unwrap_err(),
        WRITE_ONLY_REJECTION
    );
    let engine = call_tool(
        &server,
        READ_CYPHER,
        args!({"statement": "MATCH (n RETURN n"}),
    )
    .unwrap_err();
    assert!(engine.starts_with("Error: "), "{engine}");
}

/// `get_schema` answers the schema map, and an unknown prefix is an unknown
/// tool.
#[test]
fn get_schema_answers_the_map_and_unknown_names_are_refused() {
    let server = server();
    assert_eq!(
        call_tool(&server, "drop_everything", args!({})).unwrap_err(),
        "unknown tool 'drop_everything'"
    );
    let schema: Json = serde_json::from_str(&call_tool(&server, GET_SCHEMA, args!({})).unwrap())
        .expect("the schema map is JSON");
    assert!(schema.is_object());
}

/// Null is absent for every optional member: `parameters.rows: null` is not
/// a shape error, exactly like a null `parameters` or `arguments`.
#[test]
fn null_rows_is_absent_like_every_optional_member() {
    let outcome = run(
        READ_CYPHER,
        serde_json::json!({"statement": "RETURN 1 AS x", "parameters": {"rows": null}}),
    )
    .unwrap();
    assert!(matches!(outcome, ToolOutcome::Query(_)));
    let outcome = run(
        READ_CYPHER,
        serde_json::json!({"statement": "RETURN 1 AS x", "parameters": null}),
    )
    .unwrap();
    assert!(matches!(outcome, ToolOutcome::Query(_)));
}
