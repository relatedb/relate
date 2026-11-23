//! The extension mount's own tool pieces (ADR-0447 §3): the one tool this
//! mount implements itself, and the rendering of the shared layer's
//! outcomes into the importer-shaped results.
//!
//! `report_output` is the mount's lifecycle call — the run's summary text
//! and starter queries for the REPL handoff, which the product MCP server
//! has no business with. Everything else is the shared tool layer's
//! ([`relate_mcp::tools::Tools`]); this module only shapes its outcomes
//! the way an importer child reads them — `{counts, storeRemainingBytes}`
//! for the writing calls, `{columns, rows}` for the read, `{created}` for the
//! DDL call — and adds the host-owned store's capacity, the mount's own
//! lifecycle fact.

use std::io::Write;

use serde_json::{json, Value};

use relate::Database;
use relate_mcp::tools::{ToolError, ToolOutcome};
use relate_wire::mcp::{
    BatchCypherResult, WriteCypherResult, BATCH_CYPHER, READ_CYPHER, UPDATE_SCHEMA, WRITE_CYPHER,
};

use super::{store_capacity, McpError, McpRefusal, Session, ToolFailure};
use crate::extensions::host::RunSummary;

impl<O: Write> Session<O> {
    /// `report_output`: the child's summary text and starters, printed by
    /// the host after the session ends — the mount's own lifecycle call.
    pub(super) fn report_output(
        &mut self,
        arguments: &serde_json::Map<String, Value>,
    ) -> Result<Value, ToolFailure> {
        let Some(text) = arguments.get("text").and_then(Value::as_str) else {
            return Err(ToolError::InvalidParams {
                detail: "missing string \"text\"".to_string(),
            }
            .into());
        };
        self.summary = Some(RunSummary {
            text: text.to_string(),
            starters: string_array_arg(arguments.get("starters"), "starters", Vec::new())?,
        });
        Ok(json!({}))
    }

    /// Render one shared-layer outcome as this mount's result for `name`:
    /// the importer-facing shape the child decodes, with the store's
    /// remaining bytes on the writing calls. A tool answering a shape its
    /// name does not promise is the typed fatal report, never an empty
    /// result.
    pub(super) fn render_tool(
        &self,
        name: &str,
        store: &Database,
        outcome: ToolOutcome,
    ) -> Result<Value, ToolFailure> {
        match name {
            WRITE_CYPHER => {
                let ToolOutcome::Query(outcome) = outcome else {
                    return Err(unexpected(name));
                };
                Ok(serde_json::to_value(WriteCypherResult {
                    counts: counts_value(&outcome.result),
                    store_remaining_bytes: self.remaining_bytes(store)?,
                })
                .expect("a write_cypher result serializes"))
            }
            READ_CYPHER => {
                let ToolOutcome::Query(outcome) = outcome else {
                    return Err(unexpected(name));
                };
                let rows = relate::query_result_to_cypher_rows(&outcome.result);
                let rows =
                    relate_model::wire::rows_for_result(&outcome.result.header.columns, &rows);
                Ok(json!({
                    "columns": outcome.result.header.columns,
                    "rows": rows,
                }))
            }
            UPDATE_SCHEMA => {
                let ToolOutcome::Created(created) = outcome else {
                    return Err(unexpected(name));
                };
                Ok(json!({"created": created}))
            }
            BATCH_CYPHER => {
                let ToolOutcome::Batch(results) = outcome else {
                    return Err(unexpected(name));
                };
                let results: Vec<Value> = results.iter().map(counts_value).collect();
                Ok(serde_json::to_value(BatchCypherResult {
                    results,
                    store_remaining_bytes: self.remaining_bytes(store)?,
                })
                .expect("a batch_cypher result serializes"))
            }
            // `report_output` never reaches here (the session answers it
            // itself), and an unknown name never reaches the tool layer's
            // dispatch at all: either way the shape is not one this mount
            // renders.
            _ => Err(unexpected(name)),
        }
    }

    /// The host-owned store's remaining bytes after one writing call — the
    /// mount's capacity fact, surfaced as the call's refusal when the
    /// engine refuses the read (a poisoned/fenced handle).
    fn remaining_bytes(&self, store: &Database) -> Result<u64, ToolFailure> {
        store_capacity(store)
            .map(|capacity| capacity.store_remaining_bytes)
            .map_err(|error| {
                ToolFailure::Refusal(McpRefusal::Tool(ToolError::Engine {
                    detail: error.to_string(),
                }))
            })
    }
}

/// A tool that answered a shape its name does not promise — a dispatch bug,
/// reported as the typed fatal rather than rendered as an empty result.
fn unexpected(tool: &str) -> ToolFailure {
    ToolFailure::Fatal(McpError::UnexpectedOutcome {
        tool: tool.to_string(),
    })
}

/// An optional string-array argument: absent or null is `default`, an array
/// of strings is itself, anything else the pinned refusal naming the key.
fn string_array_arg(
    value: Option<&Value>,
    key: &str,
    default: Vec<String>,
) -> Result<Vec<String>, ToolError> {
    let detail = || ToolError::InvalidParams {
        detail: format!("\"{key}\" must be an array of strings"),
    };
    match value {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| item.as_str().map(str::to_string).ok_or_else(detail))
            .collect(),
        Some(_) => Err(detail()),
    }
}

/// One statement's mutation counters, in the wire's canonical vocabulary —
/// the same rendering every Relate surface publishes.
fn counts_value(result: &relate::QueryResult) -> Value {
    relate_model::wire::stats_to_wire_json(&relate::query_stats_to_wire(&result.stats))
}
