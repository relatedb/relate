//! The Relate MCP tool catalog (ADR-0442 §2, ADR-0447 §3): one table of
//! every tool name, its client-facing title and description, its input
//! schema, its capability annotations, and the mounts that serve it.
//!
//! Both MCP surfaces derive from this table and add nothing of their own but
//! policy:
//!
//! - the extension mount (`surfaces/relate-cli/src/extensions/mcp.rs`)
//!   serves its five rows as they are, and
//! - the product MCP server (`surfaces/relate-mcp/src/tools.rs`) serves
//!   the three agent-facing rows with its namespace prefix applied and its
//!   writing tool dropped when it runs read-only.
//!
//! No second table, no per-mount description, no parallel refusal string:
//! a rename is an edit here, and every surface's `tools/list` moves with it.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// `write_cypher` — execute one writing statement, optionally with a
/// `rows` array the statement's `$rows` parameter takes.
pub const WRITE_CYPHER: &str = "write_cypher";
/// `read_cypher` — execute one read statement and return its columns and
/// rows.
pub const READ_CYPHER: &str = "read_cypher";
/// `get_schema` — the graph's labels, their property types, and the
/// relationship types between them.
pub const GET_SCHEMA: &str = "get_schema";
/// `update_schema` — execute one DDL statement (indexes, constraints).
pub const UPDATE_SCHEMA: &str = "update_schema";
/// `batch_cypher` — execute several statements in one transaction.
pub const BATCH_CYPHER: &str = "batch_cypher";
/// `report_output` — the child's summary text and starter queries.
pub const REPORT_OUTPUT: &str = "report_output";

/// The read guard's pinned rejection text — the reference `mtp-neo4j-cypher`
/// server's wording, kept verbatim so a client keying on it behaves the
/// same against every Relate MCP surface. Both mounts' read tools refuse
/// a writing or schema statement with it.
pub const READ_ONLY_REJECTION: &str = "Only MATCH queries are allowed for read-query";

/// The write guard's pinned rejection text — [`READ_ONLY_REJECTION`]'s
/// sibling in the same wording, for a writing tool handed a pure read.
pub const WRITE_ONLY_REJECTION: &str = "Only write queries are allowed for write-query";

/// The DDL guard's pinned rejection text — [`READ_ONLY_REJECTION`]'s
/// sibling, in the same wording, for `update_schema`: a statement whose plan
/// carries no schema clause is refused before it reaches the graph.
pub const SCHEMA_ONLY_REJECTION: &str = "Only DDL queries are allowed for update-schema";

/// The stream-cap refusals' remedy tail (ADR-0598) — the one spelling
/// both the host's fatal refusal and the child kit's earlier refusal
/// print, so the advice cannot drift between the two sides of the mount.
/// The cap it answers is whole-import framing's whole-run bound; a
/// `per-batch` session never sees either text.
pub const STREAM_CAP_REMEDY: &str = "window the import or move the package to per-batch framing";

/// Which mount serves a tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mount {
    /// The extension mount (`relate run <name>`): the importer child's
    /// five tools.
    Extension,
    /// The product MCP server (`relate serve --mcp`): the three tools an
    /// agent calls.
    Product,
}

/// The mounts one table row serves: one, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Served {
    /// Only the extension mount.
    Extension,
    /// Only the product MCP server.
    Product,
    /// Both mounts, under one name and one schema.
    Both,
}

impl Served {
    /// Whether this row is listed by `mount`.
    pub fn serves(self, mount: Mount) -> bool {
        match self {
            Self::Both => true,
            Self::Extension => mount == Mount::Extension,
            Self::Product => mount == Mount::Product,
        }
    }
}

/// One tool of the Relate MCP vocabulary: the name, the client-facing
/// title and description, the input schema, the capability annotations, and
/// the mounts that serve it. `input_schema` is a function so the table is a
/// `const` without building six JSON trees at startup.
#[derive(Debug, Clone, Copy)]
pub struct ToolSpec {
    /// The tool name a `tools/call` names (unprefixed; a serving mount may
    /// add its own namespace prefix).
    pub name: &'static str,
    /// The MCP annotation title a client shows.
    pub title: &'static str,
    /// The description a client reads — the tool-selection prompt.
    pub description: &'static str,
    /// The `inputSchema` builder.
    pub input_schema: fn() -> Value,
    /// The `readOnlyHint`: the tool never mutates the graph.
    pub read_only: bool,
    /// The `destructiveHint`: the tool can delete data.
    pub destructive: bool,
    /// The mounts that serve the tool.
    pub served: Served,
}

/// **The table** (ADR-0447 §3): every Relate MCP tool, spelled,
/// described and schema'd once. Table order is the listing order.
pub const TOOL_TABLE: &[ToolSpec] = &[
    ToolSpec {
        name: WRITE_CYPHER,
        title: "Write Cypher",
        description: "Execute one writing Cypher statement (CREATE, MERGE, SET, DELETE, …). \
             `parameters` is passed as the statement's parameters; an object with a `rows` \
             array feeds the statement's `$rows` parameter. Returns the mutation counters.",
        input_schema: cypher_input_schema,
        read_only: false,
        destructive: true,
        served: Served::Both,
    },
    ToolSpec {
        name: READ_CYPHER,
        title: "Read Cypher",
        description: "Execute one read-only Cypher statement. Returns the result's rows. \
             Write and schema (DDL) statements are rejected.",
        input_schema: cypher_input_schema,
        read_only: true,
        destructive: false,
        served: Served::Both,
    },
    ToolSpec {
        name: GET_SCHEMA,
        title: "Get Schema",
        description: "Get the schema of the graph store: node labels with their property \
             keys and inferred types, and the relationship types between them. `sample_size` \
             bounds how many nodes and relationships are examined per label (larger is \
             slower but more accurate; -1 examines all).",
        input_schema: schema_input_schema,
        read_only: true,
        destructive: false,
        served: Served::Product,
    },
    ToolSpec {
        name: UPDATE_SCHEMA,
        title: "Update Schema",
        description: "Execute one DDL statement (an index or a constraint). Returns the \
             schema object names it created. Data writes and reads are rejected.",
        input_schema: statement_only_schema,
        read_only: false,
        destructive: false,
        served: Served::Extension,
    },
    ToolSpec {
        name: BATCH_CYPHER,
        title: "Batch Cypher",
        description: "Execute several writing statements in one transaction and return \
             their mutation counters in order. Under `whole-import` a failing statement \
             fails the run (the session's one transaction cannot roll back a single call); \
             under `per-batch` the call alone rolls back.",
        input_schema: batch_cypher_input_schema,
        read_only: false,
        destructive: true,
        served: Served::Extension,
    },
    ToolSpec {
        name: REPORT_OUTPUT,
        title: "Report Output",
        description: "Report the run's summary: one line of text plus optional starter \
             queries for the interactive handoff. Returns an empty object.",
        input_schema: report_input_schema,
        read_only: true,
        destructive: false,
        served: Served::Extension,
    },
];

/// The table rows `mount` serves, in table order.
pub fn tools_for(mount: Mount) -> impl Iterator<Item = &'static ToolSpec> {
    TOOL_TABLE
        .iter()
        .filter(move |spec| spec.served.serves(mount))
}

/// The `tools/list` result for `specs`, each tool's name prefixed by
/// `prefix` — the namespace policy a serving mount applies (the product
/// server's `db1-`, empty on the extension mount). The one renderer, so an
/// annotation or a schema cannot differ between the mounts.
pub fn tools_list<'a>(specs: impl Iterator<Item = &'a ToolSpec>, prefix: &str) -> Value {
    let tools: Vec<Value> = specs
        .map(|spec| {
            json!({
                "name": format!("{prefix}{}", spec.name),
                "description": spec.description,
                "inputSchema": (spec.input_schema)(),
                "annotations": {
                    "title": spec.title,
                    "readOnlyHint": spec.read_only,
                    "destructiveHint": spec.destructive,
                    "idempotentHint": spec.read_only,
                    "openWorldHint": false,
                },
            })
        })
        .collect();
    json!({"tools": tools})
}

/// The extension mount's `tools/list` result — its five rows, unprefixed.
pub fn extension_tools_list() -> Value {
    tools_list(tools_for(Mount::Extension), "")
}

/// The one statement-argument schema: a `statement` string and an optional
/// `parameters` object — the shape both cypher tools take, the extension
/// mount's writing/reading calls and the product server's alike.
fn cypher_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "statement": {
                "type": "string",
                "description": "The Cypher statement.",
            },
            "parameters": {
                "type": "object",
                "description": "The statement's parameters; a `rows` array feeds `$rows`.",
                "default": {},
            },
        },
        "required": ["statement"],
    })
}

/// The `get_schema` argument schema: the optional per-label sample size.
fn schema_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "sample_size": {
                "type": "integer",
                "description": "The sample size used to infer the graph schema. Larger \
                                samples are slower, but more accurate. Smaller samples are \
                                faster, but might miss information. -1 samples the whole \
                                graph.",
            },
        },
    })
}

/// The `update_schema` argument schema: the DDL statement alone.
fn statement_only_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "statement": {"type": "string", "description": "The DDL statement."},
        },
        "required": ["statement"],
    })
}

/// The `batch_cypher` argument schema: the statements, each with its
/// optional parameters.
fn batch_cypher_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "statements": {
                "type": "array",
                "description": "The statements, each with its optional parameters.",
                "items": {
                    "type": "object",
                    "properties": {
                        "statement": {"type": "string"},
                        "parameters": {"type": "object", "default": {}},
                    },
                    "required": ["statement"],
                },
            },
        },
        "required": ["statements"],
    })
}

/// The `report_output` argument schema: the summary line and the optional
/// starters.
fn report_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "text": {"type": "string", "description": "The summary line."},
            "starters": {
                "type": "array",
                "items": {"type": "string"},
                "description": "Starter queries for the REPL handoff.",
            },
        },
        "required": ["text"],
    })
}

/// The `write_cypher` result (ADR-0442 §2): the statement's mutation
/// counters and the host-owned store's remaining byte budget after the
/// write. One schema for both sides — the host serializes it, a child
/// decodes it — so the capacity field cannot drift from what the
/// handshake published.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WriteCypherResult {
    /// The mutation counters, in the shared wire vocabulary
    /// (`nodes_created`, …).
    pub counts: Value,
    /// The store's remaining bytes after the write; subtract two of them
    /// to learn what one call cost (see [`Capacity`](super::protocol::Capacity)).
    pub store_remaining_bytes: u64,
}

/// The `batch_cypher` result (ADR-0442 §2): one counter object per
/// statement in call order, plus the store's remaining byte budget after
/// the call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BatchCypherResult {
    /// The statements' counters, in call order.
    pub results: Vec<Value>,
    /// The store's remaining bytes after the call.
    pub store_remaining_bytes: u64,
}

/// The `tools/call` result: MCP's content envelope around the tool's JSON
/// text. One type for both sides — the host serializes it through
/// [`tool_result`], the child decodes it — so the shape cannot drift.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCallResult {
    /// The result's content items.
    pub content: Vec<ToolContent>,
    /// Whether the tool reported a failure.
    #[serde(rename = "isError", default)]
    pub is_error: bool,
}

/// One content item of a `tools/call` result: the tagged `type` decides the
/// variant, so an unknown kind refuses at decode and neither side compares
/// a runtime string.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ToolContent {
    /// A text item — the only kind the mount writes.
    Text {
        /// The item's text.
        text: String,
    },
}

/// The success `tools/call` result around `text` — the one constructor the
/// host builds its responses from.
pub fn tool_result(text: &str) -> Value {
    serde_json::to_value(ToolCallResult {
        content: vec![ToolContent::Text {
            text: text.to_string(),
        }],
        is_error: false,
    })
    .expect("a tools/call result serializes")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_row(name: &str) -> &'static ToolSpec {
        TOOL_TABLE
            .iter()
            .find(|spec| spec.name == name)
            .unwrap_or_else(|| panic!("the table lists '{name}'"))
    }

    /// The vocabulary both mounts serve is spelled once, here: the
    /// extension mount's five and the product server's schema tool. A rename
    /// is an edit in this one place, and each surface's tests pin the same
    /// strings against its catalog.
    #[test]
    fn the_shared_tool_vocabulary_is_spelled_once() {
        assert_eq!(
            [
                WRITE_CYPHER,
                READ_CYPHER,
                GET_SCHEMA,
                UPDATE_SCHEMA,
                BATCH_CYPHER,
                REPORT_OUTPUT
            ],
            [
                "write_cypher",
                "read_cypher",
                "get_schema",
                "update_schema",
                "batch_cypher",
                "report_output"
            ]
        );
        let names: Vec<&str> = TOOL_TABLE.iter().map(|spec| spec.name).collect();
        assert_eq!(
            names,
            [
                WRITE_CYPHER,
                READ_CYPHER,
                GET_SCHEMA,
                UPDATE_SCHEMA,
                BATCH_CYPHER,
                REPORT_OUTPUT
            ]
        );
    }

    #[test]
    fn each_mount_lists_its_own_rows() {
        let extension: Vec<&str> = tools_for(Mount::Extension).map(|spec| spec.name).collect();
        assert_eq!(
            extension,
            [
                WRITE_CYPHER,
                READ_CYPHER,
                UPDATE_SCHEMA,
                BATCH_CYPHER,
                REPORT_OUTPUT
            ]
        );
        let product: Vec<&str> = tools_for(Mount::Product).map(|spec| spec.name).collect();
        assert_eq!(product, [WRITE_CYPHER, READ_CYPHER, GET_SCHEMA]);
    }

    #[test]
    fn the_extension_catalog_is_the_mounts_five() {
        let list = extension_tools_list();
        let tools = list["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            [
                WRITE_CYPHER,
                READ_CYPHER,
                UPDATE_SCHEMA,
                BATCH_CYPHER,
                REPORT_OUTPUT
            ]
        );
        let read_only = |name: &str| {
            tools.iter().find(|t| t["name"] == name).unwrap()["annotations"]["readOnlyHint"]
                .as_bool()
                .unwrap()
        };
        assert!(read_only(READ_CYPHER));
        assert!(read_only(REPORT_OUTPUT));
        assert!(!read_only(WRITE_CYPHER));
        assert!(!read_only(UPDATE_SCHEMA));
        assert!(!read_only(BATCH_CYPHER));
        // A writing tool that can delete says so; nothing else does.
        let destructive = |name: &str| {
            tools.iter().find(|t| t["name"] == name).unwrap()["annotations"]["destructiveHint"]
                .as_bool()
                .unwrap()
        };
        assert!(destructive(WRITE_CYPHER));
        assert!(destructive(BATCH_CYPHER));
        assert!(!destructive(READ_CYPHER));
        assert!(!destructive(UPDATE_SCHEMA));
        assert!(!destructive(REPORT_OUTPUT));
        // Every tool carries an object schema, and the ones that take an
        // argument name what they require.
        for tool in tools {
            assert_eq!(tool["inputSchema"]["type"], "object");
        }
        assert_eq!(
            (table_row(UPDATE_SCHEMA).input_schema)()["required"],
            json!(["statement"])
        );
        assert_eq!(
            (table_row(BATCH_CYPHER).input_schema)()["required"],
            json!(["statements"])
        );
        assert_eq!(
            (table_row(REPORT_OUTPUT).input_schema)()["required"],
            json!(["text"])
        );
    }

    /// The one renderer honours a prefix, which is all a serving mount adds
    /// on top of the table (the product server's namespace policy).
    #[test]
    fn a_prefixed_catalog_prefixes_every_name() {
        let list = tools_list(tools_for(Mount::Product), "db1-");
        let names: Vec<&str> = list["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            ["db1-write_cypher", "db1-read_cypher", "db1-get_schema"]
        );
    }

    /// The two writing results spell the capacity member the handshake's
    /// own way (`storeRemainingBytes`), and their writer and reader share
    /// one schema.
    #[test]
    fn the_writing_results_carry_the_remaining_bytes_camel_cased() {
        let write = json!({"counts": {"nodes_created": 2}, "storeRemainingBytes": 4096});
        let decoded: WriteCypherResult = serde_json::from_value(write.clone()).unwrap();
        assert_eq!(decoded.counts["nodes_created"], json!(2));
        assert_eq!(decoded.store_remaining_bytes, 4096);
        assert_eq!(serde_json::to_value(&decoded).unwrap(), write);

        let batch = json!({"results": [{"nodes_created": 1}], "storeRemainingBytes": 2048});
        let decoded: BatchCypherResult = serde_json::from_value(batch.clone()).unwrap();
        assert_eq!(decoded.results.len(), 1);
        assert_eq!(decoded.store_remaining_bytes, 2048);
        assert_eq!(serde_json::to_value(&decoded).unwrap(), batch);

        // Strict decode: a passed-through snake_case spelling is a foreign
        // field, not a second wire name.
        let snake = json!({"counts": {}, "store_remaining_bytes": 1});
        assert!(serde_json::from_value::<WriteCypherResult>(snake).is_err());
    }

    /// The writer and the reader share one schema: what `tool_result`
    /// writes, the tagged decode accepts.
    #[test]
    fn the_tool_result_round_trips_through_the_typed_decode() {
        let written = tool_result("{\"counts\":{}}");
        let decoded: ToolCallResult = serde_json::from_value(written).unwrap();
        assert_eq!(
            decoded,
            ToolCallResult {
                content: vec![ToolContent::Text {
                    text: "{\"counts\":{}}".to_string()
                }],
                is_error: false,
            }
        );
        // An unknown content kind refuses at decode — the tagged enum, not
        // a runtime string compare.
        let image = json!({"content": [{"type": "image", "data": "…"}], "isError": false});
        let err = serde_json::from_value::<ToolCallResult>(image).unwrap_err();
        assert!(err.to_string().contains("unknown variant"), "{err}");
    }
}
