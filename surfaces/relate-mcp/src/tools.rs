//! The MCP tool layer: one implementation of every Relate MCP tool, the
//! one read/write/DDL gating, and the one refusal vocabulary — shared by the
//! two surfaces that serve MCP (ADR-0447 §3):
//!
//! - the product server (`relate serve --mcp`, this crate's
//!   [`crate::McpServer`]) serves the three agent-facing tools and adds its
//!   own two policies on top: the namespace prefix and the read-only gate;
//! - the extension mount (`relate run <name>`, the CLI's
//!   `extensions::mcp`) serves the importer child's tools and adds its own
//!   lifecycle on top: the commit framing, the capacity facts and the
//!   summary.
//!
//! The tool names, titles, descriptions and input schemas are the wire kit's
//! one table ([`relate_wire::mcp::TOOL_TABLE`]); the bodies here run
//! against a host-owned connection ([`Tools`]) — one implementation per
//! tool, one gate per access class, one set of pinned refusal texts.
//!
//! Results keep each surface's own contract (the product server returns the
//! canonical wire envelope, the extension mount the importer-shaped counters
//! and rows): the tool *body* is shared, the rendering is the surface's.
//!
//! `read_cypher` and `write_cypher` return the JSON-serialized **wire
//! envelope** (`{"columns": […], "columnTypes": […], "rows": [{"column":
//! value, …}], "stats": {…}, "notifications": […], "elapsed_ms": <f64>,
//! "parse_ms": <f64>, "plan_ms": <f64>, "execute_ms": <f64>, "plan":
//! {…}|null, "error": null}`) with the engine-stamped per-column types
//! (ADR-0224), the engine-reported statement elapsed (ADR-0221) and its
//! PARSE / PLAN / EXECUTE phase split (ADR-0491), the classification
//! `EXPLAIN`'s plan-time notifications (ADR-0363) and its structured plan
//! (LABS-418, ADR-0381); `get_schema` returns the serialized **schema
//! map** (see [`crate::schema`]).

use std::collections::{BTreeMap, BTreeSet};

use relate::{Connection, Database, QueryResult, QueryType, Value};
use serde_json::{Map, Value as Json};

use relate_wire::mcp::{self, Mount};
pub use relate_wire::mcp::{
    BATCH_CYPHER, GET_SCHEMA, READ_CYPHER, READ_ONLY_REJECTION, REPORT_OUTPUT,
    SCHEMA_ONLY_REJECTION, UPDATE_SCHEMA, WRITE_CYPHER, WRITE_ONLY_REJECTION,
};

use crate::server::McpServer;

/// The per-call limits one tool run admits — the wire kit's published caps,
/// enforced here so both surfaces refuse the same payloads: one statement's
/// bytes, the rows one `write_cypher` call may carry, and the statements one
/// `batch_cypher` call may carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The per-statement byte cap.
    pub max_statement_bytes: u64,
    /// The rows-per-`write_cypher` cap.
    pub max_rows: usize,
    /// The statements-per-`batch_cypher` cap.
    pub max_statements: usize,
}

impl Limits {
    /// The shipped limits: the wire kit's numbers, one home for both sides.
    pub const fn shipped() -> Self {
        Self {
            max_statement_bytes: relate_wire::MAX_STATEMENT_BYTES,
            max_rows: relate_wire::MAX_ROWS_PER_STATEMENT,
            max_statements: relate_wire::MAX_STATEMENTS,
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self::shipped()
    }
}

/// A refused tool call — the one vocabulary both surfaces answer with. The
/// pinned texts live here (and the guard texts in the wire kit, re-exported
/// above); a surface renders the text through its own transport frame (a
/// JSON-RPC error on the extension mount, an `isError` tool result on the
/// product server).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolError {
    /// A writing or schema statement handed to `read_cypher`.
    ReadOnly,
    /// A pure read handed to `write_cypher` or `batch_cypher`.
    WriteOnly,
    /// A non-DDL statement handed to `update_schema`.
    SchemaOnly,
    /// A `tools/call` payload of the wrong shape.
    InvalidParams {
        /// What was wrong, phrased for the caller.
        detail: String,
    },
    /// The tool name is not one this surface serves.
    UnknownTool {
        /// The name the caller sent.
        name: String,
    },
    /// One statement's bytes exceed the statement cap.
    StatementTooLarge {
        /// The cap, in bytes.
        limit: u64,
    },
    /// A `write_cypher` call's `rows` array exceeds the row cap.
    TooManyRows {
        /// The rows the call carried.
        count: usize,
        /// The cap the count ran under.
        limit: usize,
    },
    /// A `batch_cypher` call carries more statements than the cap.
    TooManyStatements {
        /// The statements the call carried.
        count: usize,
        /// The cap the count ran under.
        limit: usize,
    },
    /// The engine refused the statement.
    Engine {
        /// The engine's own text, verbatim.
        detail: String,
    },
}

impl ToolError {
    /// The JSON-RPC error code a surface that answers refusals as JSON-RPC
    /// errors uses — every tool refusal is the caller's payload or statement,
    /// never a protocol fault (JSON-RPC `Invalid params`).
    pub fn code(&self) -> i64 {
        mcp::INVALID_PARAMS
    }
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReadOnly => f.write_str(READ_ONLY_REJECTION),
            Self::WriteOnly => f.write_str(WRITE_ONLY_REJECTION),
            Self::SchemaOnly => f.write_str(SCHEMA_ONLY_REJECTION),
            Self::InvalidParams { detail } => write!(f, "invalid params: {detail}"),
            Self::UnknownTool { name } => write!(f, "unknown tool '{name}'"),
            Self::StatementTooLarge { limit } => {
                write!(f, "the statement exceeds the {limit}-byte statement cap")
            }
            Self::TooManyRows { count, limit } => {
                write!(f, "the call carries {count} rows, over the {limit}-row cap")
            }
            Self::TooManyStatements { count, limit } => write!(
                f,
                "the call carries {count} statements, over the {limit}-statement cap"
            ),
            Self::Engine { detail } => write!(f, "engine error: {detail}"),
        }
    }
}

/// One executed cypher statement and the classification plan that admitted
/// it: `explained` is the `EXPLAIN` result the gate read, so a surface that
/// renders plan metadata (the product server's envelope) has it without a
/// second plan.
#[derive(Debug)]
pub struct QueryOutcome {
    /// The executed statement's result.
    pub result: QueryResult,
    /// The classification `EXPLAIN`'s result.
    pub explained: QueryResult,
}

/// What one tool call produced, before the surface renders it. The query
/// outcome is boxed: two executed results are the largest thing the layer
/// hands back, and the other variants would pay for that size per call.
#[derive(Debug)]
pub enum ToolOutcome {
    /// A cypher statement executed (`read_cypher`, `write_cypher`).
    Query(Box<QueryOutcome>),
    /// A DDL call's created schema names (`update_schema`).
    Created(Vec<String>),
    /// A batch call's per-statement results, in call order (`batch_cypher`).
    Batch(Vec<QueryResult>),
}

/// A payload-shape refusal from the wire kit's object read — the one text
/// wrap, so every member extraction names its own member and answers in
/// the tool layer's vocabulary.
fn invalid_params(detail: String) -> ToolError {
    ToolError::InvalidParams { detail }
}

/// The canonical empty JSON object — the one static the optional-object
/// reads fold absence into, so absent and empty answer alike wherever a
/// map-shaped member is optional.
static EMPTY_OBJECT: std::sync::LazyLock<Map<String, Json>> = std::sync::LazyLock::new(Map::new);

/// The tool name and the validated arguments object of one `tools/call`
/// params value — the one extraction both surfaces run before they
/// dispatch. Absent or `null` arguments fold to the empty map here, once (the
/// vocabulary's one absence rule); a non-object is refused with the shared
/// `"arguments" must be an object`.
pub fn call_parts(params: Option<&Json>) -> Result<(&str, &Map<String, Json>), ToolError> {
    // Both halves borrowed: the name is matched on and the arguments read
    // by the dispatch — neither surface needs an owned copy of the
    // payload. An absent params object folds into the one name guard — an
    // empty map has no "name" either, so absence and empty answer the same
    // refusal; the empty map is passed to the read as its default.
    let params = mcp::object_member("params", params).map_err(invalid_params)?;
    let Some(name) = params
        .unwrap_or(&EMPTY_OBJECT)
        .get("name")
        .and_then(Json::as_str)
    else {
        return Err(ToolError::InvalidParams {
            detail: "missing string \"name\"".to_string(),
        });
    };
    let arguments = mcp::object_member("arguments", params.and_then(|p| p.get("arguments")))
        .map_err(invalid_params)?;
    Ok((name, arguments.unwrap_or(&EMPTY_OBJECT)))
}

/// Whether `name` is a tool that writes — read from the one tool table, so a
/// caller that must frame writes in a transaction asks the vocabulary
/// instead of re-spelling names.
pub fn writes(name: &str) -> bool {
    mcp::TOOL_TABLE
        .iter()
        .find(|spec| spec.name == name)
        .is_some_and(|spec| !spec.read_only)
}

/// Which access a tool call runs under — the one gating (ADR-0447 §3): a
/// read admits only a pure read (schema DDL mutates the catalog, so it is
/// refused too), a write refuses a pure read and admits every writing type
/// schema included, and a DDL call admits only schema statements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gate {
    Read,
    Write,
    Schema,
}

impl Gate {
    /// The one access decision, exhaustive over the gate and the plan's
    /// query type.
    fn check(self, query_type: QueryType) -> Result<(), ToolError> {
        match (self, query_type) {
            (Gate::Read, QueryType::Read) => Ok(()),
            (Gate::Read, _) => Err(ToolError::ReadOnly),
            (Gate::Write, QueryType::Read) => Err(ToolError::WriteOnly),
            (Gate::Write, _) => Ok(()),
            (Gate::Schema, QueryType::Schema) => Ok(()),
            (Gate::Schema, _) => Err(ToolError::SchemaOnly),
        }
    }
}

/// The tool bodies over a host-owned connection: every MCP tool that runs a
/// statement, one implementation for both surfaces.
pub struct Tools<'a> {
    conn: &'a mut Connection,
    limits: Limits,
}

impl<'a> Tools<'a> {
    /// The tool bodies over `conn`, under `limits`.
    pub fn new(conn: &'a mut Connection, limits: Limits) -> Self {
        Self { conn, limits }
    }

    /// Dispatch one call by name — the extension mount's entry point, whose
    /// tool set is a subset of the table. `get_schema` needs the store
    /// handle rather than a connection ([`get_schema_json`]) and
    /// `report_output` is the extension mount's own lifecycle call, so
    /// neither is dispatched here.
    pub fn call(
        &mut self,
        name: &str,
        arguments: &Map<String, Json>,
    ) -> Result<ToolOutcome, ToolError> {
        match name {
            mcp::WRITE_CYPHER => self
                .write_cypher(arguments)
                .map(|outcome| ToolOutcome::Query(Box::new(outcome))),
            mcp::READ_CYPHER => self
                .read_cypher(arguments)
                .map(|outcome| ToolOutcome::Query(Box::new(outcome))),
            mcp::UPDATE_SCHEMA => self.update_schema(arguments).map(ToolOutcome::Created),
            mcp::BATCH_CYPHER => self.batch_cypher(arguments).map(ToolOutcome::Batch),
            other => Err(ToolError::UnknownTool {
                name: other.to_string(),
            }),
        }
    }

    /// `write_cypher`: one writing statement with its parameters (a `rows`
    /// array feeds `$rows`). A pure read is refused — the write tool is for
    /// writes on both surfaces.
    pub fn write_cypher(
        &mut self,
        arguments: &Map<String, Json>,
    ) -> Result<QueryOutcome, ToolError> {
        let (statement, params) = self.cypher_call(arguments)?;
        self.run(statement, params, Gate::Write)
    }

    /// `read_cypher`: one read-only statement. A writing or schema statement
    /// is refused with the reference server's pinned text.
    pub fn read_cypher(
        &mut self,
        arguments: &Map<String, Json>,
    ) -> Result<QueryOutcome, ToolError> {
        let (statement, params) = self.cypher_call(arguments)?;
        self.run(statement, params, Gate::Read)
    }

    /// `update_schema`: one DDL statement. Returns the schema object names it
    /// created — the `SHOW INDEXES` / `SHOW CONSTRAINTS` name difference
    /// across the call. A data write or a read is refused before it touches
    /// the graph.
    pub fn update_schema(
        &mut self,
        arguments: &Map<String, Json>,
    ) -> Result<Vec<String>, ToolError> {
        let statement = self.statement(arguments)?;
        self.gate(statement, BTreeMap::new(), Gate::Schema)?;
        let before = self.schema_names()?;
        self.conn.query(statement).map_err(|e| engine_error(&e))?;
        let after = self.schema_names()?;
        Ok(after.difference(&before).cloned().collect())
    }

    /// `batch_cypher`: several statements, each gated as a write. The
    /// transaction around them is the caller's framing (the extension
    /// mount's commit policy; the product server runs them as it receives
    /// them).
    pub fn batch_cypher(
        &mut self,
        arguments: &Map<String, Json>,
    ) -> Result<Vec<QueryResult>, ToolError> {
        let Some(statements) = arguments.get("statements") else {
            return Err(ToolError::InvalidParams {
                detail: "missing array \"statements\"".to_string(),
            });
        };
        let Some(statements) = statements.as_array() else {
            return Err(ToolError::InvalidParams {
                detail: "\"statements\" must be an array".to_string(),
            });
        };
        if statements.len() > self.limits.max_statements {
            return Err(ToolError::TooManyStatements {
                count: statements.len(),
                limit: self.limits.max_statements,
            });
        }
        // Prepare every statement before running one: a malformed call is
        // refused whole, never half-executed.
        let mut prepared = Vec::with_capacity(statements.len());
        for (index, item) in statements.iter().enumerate() {
            let Some(item) = item.as_object() else {
                return Err(ToolError::InvalidParams {
                    detail: format!("statements[{index}] must be an object"),
                });
            };
            let statement = self.statement(item)?.to_string();
            let parameters = self.parameters(item, &EMPTY_OBJECT)?;
            prepared.push((statement, params_map(parameters)?));
        }
        let mut results = Vec::with_capacity(prepared.len());
        for (statement, params) in prepared {
            results.push(self.run(&statement, params, Gate::Write)?.result);
        }
        Ok(results)
    }

    /// The one cypher-call front half: the `statement` and `parameters`
    /// arguments, caps applied.
    fn cypher_call<'v>(
        &self,
        arguments: &'v Map<String, Json>,
    ) -> Result<(&'v str, BTreeMap<String, Value>), ToolError> {
        let statement = self.statement(arguments)?;
        let parameters = self.parameters(arguments, &EMPTY_OBJECT)?;
        Ok((statement, params_map(parameters)?))
    }

    /// The one gated execution: classify through `EXPLAIN` (bound with the
    /// call's own parameters, so the plan is exactly the run's), apply the
    /// gate, then run.
    fn run(
        &mut self,
        statement: &str,
        params: BTreeMap<String, Value>,
        gate: Gate,
    ) -> Result<QueryOutcome, ToolError> {
        let explained = self.gate(statement, params.clone(), gate)?;
        let result = self
            .conn
            .query_with_params(statement, params)
            .map_err(|e| engine_error(&e))?;
        Ok(QueryOutcome { result, explained })
    }

    /// The plan-derived query type (ADR-0162) of `statement`, gated by
    /// `gate` — the classification `EXPLAIN` gives without running the
    /// statement.
    fn gate(
        &mut self,
        statement: &str,
        params: BTreeMap<String, Value>,
        gate: Gate,
    ) -> Result<QueryResult, ToolError> {
        let explained = self
            .conn
            .query_with_params(&format!("EXPLAIN {statement}"), params)
            .map_err(|e| engine_error(&e))?;
        gate.check(explained.header.query_type)?;
        Ok(explained)
    }

    /// The `statement` argument, cap-checked before it reaches the engine.
    fn statement<'v>(&self, arguments: &'v Map<String, Json>) -> Result<&'v str, ToolError> {
        let Some(statement) = arguments.get("statement").and_then(Json::as_str) else {
            return Err(ToolError::InvalidParams {
                detail: "missing string \"statement\"".to_string(),
            });
        };
        if statement.len() as u64 > self.limits.max_statement_bytes {
            return Err(ToolError::StatementTooLarge {
                limit: self.limits.max_statement_bytes,
            });
        }
        Ok(statement)
    }

    /// The optional `parameters` object, with its `rows` array capped before
    /// the engine sees it — returned as the borrow it is: the caps read the
    /// payload, `params_map` builds the engine's owned copy, and no third
    /// copy exists between them.
    fn parameters<'v>(
        &self,
        arguments: &'v Map<String, Json>,
        default: &'v Map<String, Json>,
    ) -> Result<&'v Map<String, Json>, ToolError> {
        // The caps run on the BORROWED object: an oversized or malformed
        // payload is refused before the engine's own copy is built, so a
        // too-large rows array never doubles the request's memory on its
        // way to the refusal. Absence (or null) answers the default the
        // caller passes in.
        let Some(parameters) = mcp::object_member("parameters", arguments.get("parameters"))
            .map_err(invalid_params)?
        else {
            return Ok(default);
        };
        // Null is absent, like every optional member of the vocabulary.
        if let Some(rows) = parameters.get("rows").filter(|rows| !rows.is_null()) {
            let Some(rows) = rows.as_array() else {
                return Err(ToolError::InvalidParams {
                    detail: "\"parameters.rows\" must be an array".to_string(),
                });
            };
            if rows.len() > self.limits.max_rows {
                return Err(ToolError::TooManyRows {
                    count: rows.len(),
                    limit: self.limits.max_rows,
                });
            }
        }
        Ok(parameters)
    }

    /// The names of every index and constraint in the store — the DDL
    /// tool's before/after reading. A `SHOW` row that is not one string is
    /// an engine invariant break, not a name to skip.
    fn schema_names(&mut self) -> Result<BTreeSet<String>, ToolError> {
        let mut names = BTreeSet::new();
        for statement in ["SHOW INDEXES YIELD name", "SHOW CONSTRAINTS YIELD name"] {
            let result = self.conn.query(statement).map_err(|e| engine_error(&e))?;
            for row in &result.rows {
                let Some(cell) = row.first() else {
                    return Err(ToolError::Engine {
                        detail: format!("{statement} returned an empty row"),
                    });
                };
                let Value::String(name) = cell else {
                    return Err(ToolError::Engine {
                        detail: format!("{statement} returned a non-string name: {cell:?}"),
                    });
                };
                names.insert(name.to_string());
            }
        }
        Ok(names)
    }
}

/// An engine failure as the tool layer's refusal — the message the
/// engine's classification allows across (ADR-0260, via
/// `ErrorInfo::client_message`: the internal class is genericized, so a
/// storage path never reaches an MCP host), the caller-facing classes
/// verbatim so a caller sees exactly what failed.
fn engine_error(error: &relate::Error) -> ToolError {
    ToolError::Engine {
        detail: relate::client_message(error).into_owned(),
    }
}

impl From<relate::Error> for ToolError {
    /// The write layer's framing converts the engine's typed error through
    /// this, so a failed `begin`/`commit` crosses the boundary as the
    /// tool layer's refusal instead of a flattened string.
    fn from(error: relate::Error) -> Self {
        engine_error(&error)
    }
}

/// Decode an arguments object into the engine's parameters through the
/// engine's canonical JSON→parameter fold (plain maps; typed values need
/// the statement's own constructors). A `rows` array becomes the `$rows`
/// list.
fn params_map(parameters: &Map<String, Json>) -> Result<BTreeMap<String, Value>, ToolError> {
    relate::params_from_map(parameters).map_err(|detail| ToolError::InvalidParams {
        detail: format!("parameters: {detail}"),
    })
}

// ---------------------------------------------------------------------
// `get_schema`: the schema map, read from the store itself
// ---------------------------------------------------------------------

/// `get_schema`'s result: the serialized schema map (see [`crate::schema`]).
/// `sample_size` bounds the per-label scan (`None` scans the whole graph).
/// A serialization failure is a tool error, never a silent empty schema.
pub fn get_schema_json(db: &Database, sample_size: Option<u64>) -> Result<String, ToolError> {
    let schema =
        crate::schema::collect_schema(db, sample_size).map_err(|detail| ToolError::Engine {
            detail: detail.to_string(),
        })?;
    serde_json::to_string(&schema).map_err(|error| ToolError::Engine {
        detail: error.to_string(),
    })
}

/// The `get_schema` call's `sample_size` argument, resolved against the
/// server's configured default: absent or null takes the default, a number
/// is converted at the wire edge (`-1` = the whole graph), anything else is
/// the pinned refusal.
fn sample_size_argument(
    arguments: &Map<String, Json>,
    configured: Option<u64>,
) -> Result<Option<u64>, ToolError> {
    let not_an_integer = || ToolError::InvalidParams {
        detail: "\"sample_size\" must be an integer".to_string(),
    };
    match arguments.get("sample_size") {
        None | Some(Json::Null) => Ok(configured),
        Some(value) => {
            let n = value.as_i64().ok_or_else(not_an_integer)?;
            crate::server::sample_size_at_edge(n)
                .map_err(|detail| ToolError::InvalidParams { detail })
        }
    }
}

// ---------------------------------------------------------------------
// The product server's face
// ---------------------------------------------------------------------

/// The `tools/list` result: the wire kit's product-mount rows, each name
/// carrying the server's namespace prefix, with the writing tools hidden
/// when the server runs read-only. The table, the schemas and the
/// annotations are the kit's; only the prefix and the gate are this
/// surface's policy.
pub fn tool_specs(server: &McpServer) -> Json {
    let read_only = server.read_only();
    mcp::tools_list(
        mcp::tools_for(Mount::Product).filter(|spec| !read_only || spec.read_only),
        &server.namespace_prefix(),
    )
}

/// Which cypher tool a server call runs under, and the response shape that
/// admitted access builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Access {
    Read,
    Write,
}

/// The result shape an admitted access builds.
enum ResponseShape {
    /// Row objects under `rows` (`read_cypher`).
    Rows,
    /// Empty rows; the mutation counters ride `stats` (`write_cypher`).
    Counters,
}

impl Access {
    /// The tool this access calls — the one name↔access mapping.
    fn tool(self) -> &'static str {
        match self {
            Self::Read => READ_CYPHER,
            Self::Write => WRITE_CYPHER,
        }
    }

    /// The response shape the access builds.
    fn shape(self) -> ResponseShape {
        match self {
            Self::Read => ResponseShape::Rows,
            Self::Write => ResponseShape::Counters,
        }
    }
}

impl ResponseShape {
    /// The tool envelope for `outcome`, shaped by the access that admitted
    /// it. `explained` is the classification EXPLAIN's result: its
    /// notifications are the one list the response carries (ADR-0363
    /// decision 10), and its plan tree is the statement's structured plan
    /// (LABS-418, ADR-0381) — the executed result carries no plan of its own.
    fn envelope(self, outcome: &QueryOutcome) -> Result<String, ToolError> {
        let rows = match self {
            ResponseShape::Rows => {
                let rows = relate::query_result_to_cypher_rows(&outcome.result);
                relate_model::wire::rows_for_result(&outcome.result.header.columns, &rows)
            }
            ResponseShape::Counters => Vec::new(),
        };
        result_envelope_json(&outcome.result, &outcome.explained, rows)
    }
}

/// Execute one `tools/call`: returns the tool's text content on success or
/// the tool-error text (surfaced as `isError: true`) on failure. Unknown or
/// disabled tool names are tool errors too.
pub fn call_tool(
    server: &McpServer,
    name: &str,
    arguments: &Map<String, Json>,
) -> Result<String, String> {
    let prefix = server.namespace_prefix();
    let Some(base) = name.strip_prefix(&prefix) else {
        return Err(unknown_tool(name));
    };
    match base {
        GET_SCHEMA => {
            let sample_size = sample_size_argument(arguments, server.schema_sample_size())
                .map_err(tool_error_text)?;
            get_schema_json(server.database(), sample_size).map_err(tool_error_text)
        }
        READ_CYPHER => call_cypher(server, arguments, Access::Read),
        WRITE_CYPHER if !server.read_only() => call_cypher(server, arguments, Access::Write),
        _ => Err(unknown_tool(name)),
    }
}

/// One cypher tool call over the shared tool bodies: the access names the
/// tool, the body classifies, gates and runs it, and the shape renders the
/// envelope.
fn call_cypher(
    server: &McpServer,
    arguments: &Map<String, Json>,
    access: Access,
) -> Result<String, String> {
    let mut conn = server
        .database()
        .connect()
        // Framed here, exactly once: `engine_error_text` hands back the
        // bare client-safe message and this route owns the `Error: `
        // convention its other failure paths already spell.
        .map_err(|err| format!("Error: {}", crate::errors::engine_error_text(&err)))?;
    let mut tools = Tools::new(&mut conn, Limits::shipped());
    let outcome = tools
        .call(access.tool(), arguments)
        .map_err(tool_error_text)?;
    let ToolOutcome::Query(outcome) = outcome else {
        // The dispatch above only names the two cypher tools, whose outcome
        // is a query: any other shape would be a dispatch bug, and the text
        // says so instead of rendering an empty result.
        return Err(format!(
            "Error: '{}' produced no statement result",
            access.tool()
        ));
    };
    access.shape().envelope(&outcome).map_err(tool_error_text)
}

/// An unknown tool's text — the pinned [`ToolError::UnknownTool`] wording.
fn unknown_tool(name: &str) -> String {
    ToolError::UnknownTool {
        name: name.to_string(),
    }
    .to_string()
}

/// A tool refusal as the product server's tool-error text: the engine's
/// failure keeps the server's `Error: ` convention, every other refusal is
/// the shared vocabulary verbatim.
fn tool_error_text(error: ToolError) -> String {
    match error {
        ToolError::Engine { detail } => format!("Error: {detail}"),
        other => other.to_string(),
    }
}

/// The shared tool-result envelope: the canonical wire envelope
/// (`columns` / `columnTypes` / `rows` / `stats` / `notifications` /
/// `elapsed_ms` / `error`), so the MCP surface carries the same result
/// metadata as the CLI, FFI and the other surfaces, plus the statement's
/// ADR-0491 phase split (`parse_ms` / `plan_ms` / `execute_ms`) beside
/// `elapsed_ms`. `notifications` is the classification EXPLAIN's list
/// (ADR-0363 decision 10), not the executed statement's. A serialization
/// failure is a tool error, never a silent empty result.
fn result_envelope_json(
    result: &QueryResult,
    explained: &QueryResult,
    rows: Vec<Json>,
) -> Result<String, ToolError> {
    let stats = relate::query_stats_to_wire(&result.stats);
    // ADR-0491: the phase split rides the canonical builder beside
    // `elapsed_ms`, re-sourced from the EXECUTED result's own accessors
    // (not the classification EXPLAIN's) through the one Duration→ms
    // conversion behind each — the one projection
    // (`relate::timing_stamp_ms`) every envelope-building surface
    // calls, so the split keys have one emission point.
    let timings = relate::timing_stamp_ms(result);
    // The structured plan document (LABS-418, ADR-0381) rides the tool
    // result as `plan` — the classification EXPLAIN's tree, so an agent
    // reads the plan structurally, never as text it must parse. The
    // canonical builder owns the key. A plan past the plan-wire budget
    // (ADR-0628) is a tool error carrying the typed refusal's text, which
    // states the scope: the statement itself executed; only its plan
    // document is refused.
    let plan = relate::plan_wire_json(explained).map_err(|e| ToolError::Engine {
        detail: e.to_string(),
    })?;
    let envelope = relate_model::wire::response_envelope(
        &result.header.columns,
        &result.header.column_types,
        rows,
        relate_model::wire::stats_to_wire_json(&stats),
        explained.notifications(),
        timings,
        plan,
    );
    serde_json::to_string(&envelope).map_err(|error| ToolError::Engine {
        detail: error.to_string(),
    })
}

#[cfg(test)]
mod tests;
