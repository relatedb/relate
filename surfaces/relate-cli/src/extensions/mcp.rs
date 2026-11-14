//! The extension mount's MCP stdio server (ADR-0442, ADR-0447): the host
//! side of the wire whose shared facts live in [`relate_wire::mcp`] and
//! whose tool bodies live in [`relate_mcp::tools`].
//!
//! A command-kind extension is driven as a minimal MCP server over the
//! child's two std pipes: the child writes JSON-RPC 2.0 requests on stdout
//! and reads the host's responses from stdin, after the one-line request
//! envelope. The child is the client (it decides what to import); the host
//! is the server (it alone owns the `Database`).
//!
//! This module is the session: the request loop, the handshake, the caps,
//! the commit framing and the refusal surface. The process half — the
//! spawned child, its stderr drain, and the exit-classifying end hook —
//! lives in `process`, and the tool bodies this mount adds to the shared
//! layer's in `tools_impl`. The *tools* are not
//! re-implemented here — `write_cypher`, `read_cypher`, `update_schema` and
//! `batch_cypher` are the product MCP server's one implementation
//! ([`relate_mcp::tools::Tools`]), run against the host-owned connection
//! under the manifest's commit policy. The mount adds what only a host
//! lifecycle has: `report_output` (the REPL handoff's summary), the store's
//! capacity facts in the writing results, and the framing.
//!
//! Under `per-batch` every writing call commits as it lands; `whole-import`
//! frames the whole session in one transaction closed at its end. Under
//! `whole-import` a failed `batch_cypher` ends the run: the session's one
//! transaction cannot roll a single call back, so answering a per-call
//! refusal would promise a continuation the framing cannot keep.
//!
//! Caps are the wire kit's numbers, re-anchored per call (ADR-0430 §4): the
//! statement byte cap, the rows-per-statement cap, the statements-per-call
//! cap, the whole-import stream cap (ADR-0598: a `per-batch` session has no
//! whole-run stream cap — its bounds are the per-call caps and the host
//! store's live-graph limit), and — in the response direction — the
//! statement cap one response line may carry. A per-call cap is a JSON-RPC
//! refusal the child can see and react to; the whole-import stream cap and
//! the per-request framing cap are fatal (the host kills the child). The
//! per-call caps themselves are the shared tool layer's
//! ([`relate_mcp::tools::Limits`], built from [`Caps`]).
//!
//! The session's first request must be `initialize`; the initialize result
//! carries MCP's handshake facts (the negotiated [`MCP_PROTOCOL_VERSION`],
//! the tools capability, the server identity) plus the host facts
//! (fresh-store requirement, commit policy, caps, and the host-owned
//! store's capacity) as the kit's typed [`ServerFacts`]. The capacity
//! triple is the baseline every writing tool counts down from: each
//! `write_cypher` and `batch_cypher` result repeats the store's remaining
//! bytes, read from the `Database` the host owns after the write, so a
//! child subtracting two results knows what its own calls cost.
//! `notifications/*` are read and ignored (they carry no id); a message
//! without an id that is not a notification is fatal, so a child waiting
//! for an answer it can never get fails the session instead of hanging it.
//! After the handshake only `tools/list` and `tools/call` are served. A
//! child that closes its request stream and then exits non-zero fails the
//! run *inside* the framed import, before a whole-import commit — an exit
//! is never a silent success.
//!
//! The mount's read deadline ([`protocol::READ_DEADLINE`]) bounds only the
//! handshake window — the reads before the first request line arrives. A
//! child that takes its envelope and then writes nothing fails the run with
//! its own refusal naming the deadline, never a silent hang. Once the first
//! request has established the session, reads block without a bound: a
//! legitimate child may do arbitrarily expensive work (a whole tree walk, a
//! full `git log`) before its first tool call, and a child that handshakes
//! and then hangs mid-import is the pre-existing behaviour, killable with
//! Ctrl-C. The `check` verb keeps its per-stage deadline: as a diagnostic it
//! bounds every stage, where the mount bounds only the handshake.

use std::io::{BufRead, Write};
use std::time::Duration;

use relate::{Connection, Database};
use relate_mcp::tools::{self as tool_layer, Limits, ToolError, ToolOutcome};
use relate_wire::mcp::{
    self, Capacity, CommitFacts, RequestError, Response, ServerCapabilities, ServerFacts,
    ServerInfo, StreamBudget, StreamCapExceeded, INVALID_PARAMS, MCP_PROTOCOL_VERSION,
    METHOD_NOT_FOUND, STREAM_CAP_REMEDY,
};
use relate_wire::read_cap::{write_line, DeadlineReader, LineArrival};
use serde_json::Value;

use crate::extensions::host::{HostError, RunSummary};
use crate::extensions::manifest::CommandManifest;
use crate::extensions::protocol::{self, Caps};
use crate::extensions::write::CommitFraming;
use process::{EndHook, SessionEnd};

pub(crate) mod process;
mod tools_impl;

/// The pinned refusals a single `tools/call` can answer with — the typed,
/// text-stable vocabulary the child sees as a JSON-RPC error message. A
/// per-call refusal is answered and the session continues (the child may
/// correct itself); the fatal failures are [`McpError`]'s.
///
/// The tool-level refusals are the shared layer's
/// ([`relate_mcp::tools::ToolError`]): one vocabulary for both MCP
/// surfaces. What stays here is the session's own: the JSON-RPC decode, the
/// method routing, and the response cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum McpRefusal {
    /// The shared strict decode refused the line (a body that is not JSON,
    /// a missing or unknown member, a foreign `jsonrpc` version): the
    /// kit's own refusal, code and wording included.
    Request(RequestError),
    /// The method is not one this mount serves.
    UnknownMethod {
        /// The method name the child sent.
        method: String,
    },
    /// The tool's result would not fit one response line the child can
    /// read.
    ResultTooLarge {
        /// The response record cap, in bytes.
        limit: u64,
    },
    /// A tool call the shared layer refused.
    Tool(ToolError),
}

impl From<ToolError> for McpRefusal {
    fn from(error: ToolError) -> Self {
        Self::Tool(error)
    }
}

impl From<RequestError> for McpRefusal {
    /// The decoder's refusal is the session's: one conversion, so
    /// `handle_line` has one `Err` arm and the code and wording stay the
    /// decode's own ([`RequestError::code`], its `Display`).
    fn from(error: RequestError) -> Self {
        Self::Request(error)
    }
}

impl McpRefusal {
    /// The JSON-RPC error code this refusal answers with. `pub(crate)` for
    /// the `check` verb, which answers the same refusals over a hostless
    /// session (one code mapping, never a second).
    pub(crate) fn code(&self) -> i64 {
        match self {
            Self::Request(error) => error.code(),
            Self::UnknownMethod { .. } => METHOD_NOT_FOUND,
            Self::ResultTooLarge { .. } => INVALID_PARAMS,
            Self::Tool(error) => error.code(),
        }
    }
}

impl std::fmt::Display for McpRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Request(error) => write!(f, "{error}"),
            Self::UnknownMethod { method } => write!(f, "unknown method '{method}'"),
            Self::ResultTooLarge { limit } => write!(
                f,
                "the tools/call result would exceed the {limit}-byte response cap"
            ),
            Self::Tool(error) => write!(f, "{error}"),
        }
    }
}

/// The fatal failures of the MCP session — a broken stream or a promise
/// the framing cannot keep, not a per-call answer. Each renders as one
/// `Error:` line through [`HostError`] and exits 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum McpError {
    /// The child ended the session without an `initialize` request.
    InitializeMissing,
    /// The first request was not `initialize`.
    InitializeNotFirst {
        /// The method that arrived first.
        method: String,
    },
    /// A message without an id asked for a method that needs an answer.
    RequestWithoutId {
        /// The method the message named.
        method: String,
    },
    /// A `batch_cypher` statement failed inside the whole-import
    /// transaction, which cannot roll one call back alone.
    WholeImportBatchFailed {
        /// The write layer's failure text, verbatim.
        detail: String,
    },
    /// A tool answered a shape this mount cannot render — a dispatch bug,
    /// reported instead of rendering an empty result.
    UnexpectedOutcome {
        /// The tool that answered it.
        tool: String,
    },
    /// A request line exceeded the per-request framing cap.
    RecordTooLarge {
        /// The cap, in bytes.
        limit: u64,
    },
    /// The request stream exceeded the whole-import stream cap (ADR-0598) —
    /// the one framing that retains every call in one transaction. A
    /// `per-batch` session is never refused on it: its bounds are the
    /// per-call caps and the store's live-graph limit, never a whole-run
    /// byte count.
    StreamTooLarge {
        /// The cap, in bytes.
        limit: u64,
    },
    /// Reading the child's requests failed.
    ReadFailed(String),
    /// No first request line arrived within the handshake deadline: the
    /// child holds its pipes and writes nothing, so the read is a stall,
    /// not a slow answer (ADR-0442's deferred read deadline). Only the
    /// handshake window is bounded; once a request has arrived, later
    /// reads block without a deadline.
    ReadDeadline {
        /// The bound the child overran.
        deadline: Duration,
    },
    /// Writing a response to the child failed.
    ResponseWriteFailed(String),
}

impl From<StreamCapExceeded> for McpError {
    /// The session's stream-cap refusal: the budget's typed payload,
    /// rendered with the framing it guards and the remedy (ADR-0598) —
    /// the one conversion the session's charge converts through.
    fn from(refused: StreamCapExceeded) -> Self {
        Self::StreamTooLarge {
            limit: refused.limit,
        }
    }
}

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InitializeMissing => f.write_str(
                "the extension child ended its MCP session without an initialize request",
            ),
            Self::InitializeNotFirst { method } => write!(
                f,
                "the first MCP request must be initialize, got '{method}'"
            ),
            Self::RequestWithoutId { method } => write!(
                f,
                "the MCP request '{method}' carries no id; only notifications may omit it"
            ),
            Self::WholeImportBatchFailed { detail } => write!(
                f,
                "the extension MCP batch failed inside the whole-import transaction: {detail}"
            ),
            Self::UnexpectedOutcome { tool } => write!(
                f,
                "the MCP tool '{tool}' answered a result shape this mount cannot render"
            ),
            Self::RecordTooLarge { limit } => write!(
                f,
                "the extension MCP request exceeds the {limit}-byte record cap"
            ),
            Self::StreamTooLarge { limit } => write!(
                f,
                "the extension MCP request stream exceeds the {limit}-byte whole-import \
                 stream cap; {STREAM_CAP_REMEDY}"
            ),
            Self::ReadFailed(detail) => {
                write!(
                    f,
                    "failed to read the extension MCP request stream: {detail}"
                )
            }
            Self::ReadDeadline { deadline } => {
                write!(f, "the extension wrote no MCP request within {deadline:?}")
            }
            Self::ResponseWriteFailed(detail) => {
                write!(
                    f,
                    "failed to send an MCP response to the extension: {detail}"
                )
            }
        }
    }
}

/// Why a `tools/call` produced no result: the two failure classes the
/// session distinguishes. A per-call refusal is answered and the session
/// continues (the child may correct itself); a fatal failure ends the run
/// ([`McpError`]) — a `whole-import` batch the session's one transaction
/// cannot roll back alone is the one tool failure that is fatal.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ToolFailure {
    /// Answered as a JSON-RPC error; the session continues.
    Refusal(McpRefusal),
    /// The run ends — the runtime-failure class.
    Fatal(McpError),
}

impl From<McpRefusal> for ToolFailure {
    fn from(refusal: McpRefusal) -> Self {
        Self::Refusal(refusal)
    }
}

impl From<ToolError> for ToolFailure {
    fn from(error: ToolError) -> Self {
        Self::Refusal(McpRefusal::from(error))
    }
}

impl From<McpError> for ToolFailure {
    fn from(error: McpError) -> Self {
        Self::Fatal(error)
    }
}

// ---------------------------------------------------------------------
// The session (generic over its pipes so the tests drive it directly)
// ---------------------------------------------------------------------

/// One live MCP session: the child's request stream, the host's response
/// stream, the caps, and the manifest facts the `initialize` result
/// publishes. The request stream is the shared deadline-bounded reader
/// ([`DeadlineReader`]), so a child that writes nothing fails the run instead
/// of hanging it; the response pipe stays generic so the tests script a
/// session without a process and the real mount pins it to the child's
/// stdin.
pub(crate) struct Session<O: Write> {
    input: DeadlineReader,
    /// The response pipe; `None` once the session closed it at the request
    /// stream's end (the real mount, so a child waiting for stdin's end can
    /// exit). The scripted tests keep their capture.
    out: Option<O>,
    caps: Caps,
    fresh_store_required: bool,
    commit: CommitFacts,
    /// The per-request framing cap, derived from the published statement
    /// cap in one place ([`Caps::mcp_request_bytes`]): the statement's own
    /// bytes plus the JSON-RPC envelope around them, so a statement at the
    /// cap can always be framed.
    read_bytes: u64,
    /// How long the first request line may take — the handshake window's
    /// bound ([`protocol::READ_DEADLINE`]), overridable in the tests so a
    /// stall costs milliseconds. After the first line arrives, reads are
    /// unbounded.
    read_deadline: Duration,
    /// Whether the first request line has arrived: once it has, the session
    /// is established and the read deadline no longer applies.
    established: bool,
    /// The whole-run request-stream budget the session charges each read
    /// line against — framed once at the session's build, from the
    /// manifest's commit policy and the mount's caps ([`StreamBudget`],
    /// ADR-0598): capped exactly under `whole-import` framing, cap-free
    /// under `per-batch`.
    budget: StreamBudget,
    initialized: bool,
    summary: Option<RunSummary>,
    end: Option<EndHook>,
}

impl<O: Write> Session<O> {
    /// Build a session over `input`/`out` under the mount's caps and the
    /// resolved action's manifest facts.
    pub(crate) fn new<R: BufRead + Send + 'static>(
        input: R,
        out: O,
        caps: Caps,
        command: &CommandManifest,
    ) -> Self {
        Self {
            input: DeadlineReader::spawn(input, caps.mcp_request_bytes()),
            out: Some(out),
            caps,
            fresh_store_required: command.requires_fresh_store,
            commit: command.commit,
            read_bytes: caps.mcp_request_bytes(),
            read_deadline: protocol::READ_DEADLINE,
            established: false,
            budget: StreamBudget::under(command.commit, caps.stream_bytes),
            initialized: false,
            summary: None,
            end: None,
        }
    }

    /// The read deadline override the suites use: a stalled scripted pipe
    /// must fail in milliseconds, not the shipped ten seconds.
    #[cfg(test)]
    pub(crate) fn with_read_deadline(mut self, deadline: Duration) -> Self {
        self.read_deadline = deadline;
        self
    }

    /// The per-call limits the shared tool layer enforces: this mount's
    /// caps under the layer's names.
    fn limits(&self) -> Limits {
        Limits {
            max_statement_bytes: self.caps.statement_bytes,
            max_rows: self.caps.rows_per_statement,
            max_statements: self.caps.statements,
        }
    }

    /// Install the end-of-session hook (the real mount's child reaping).
    fn set_end(&mut self, end: EndHook) {
        self.end = Some(end);
    }

    /// Drive the session against `conn` under the manifest's commit
    /// policy and return the reported summary, if any. The framing is the
    /// write layer's one policy: `whole-import` wraps the whole session in
    /// one transaction, `per-batch` gives every writing call its own.
    ///
    /// `store` is the `Database` the host owns and `conn` reads — the one
    /// source of the capacity facts the handshake publishes and the
    /// writing tools report after each call.
    ///
    /// The end hook runs *inside* the frame — at the request stream's end
    /// for a clean loop, after the frame for a refusal — so a child that
    /// streams cleanly and then exits non-zero fails the run before a
    /// whole-import commit, never after it.
    pub(crate) fn drive(
        &mut self,
        store: &Database,
        conn: &mut Connection,
    ) -> Result<Option<RunSummary>, HostError> {
        let framed = self
            .commit
            .frame_import(conn, |conn| self.run_loop(store, conn));
        if let Some(end) = self.end.take() {
            // The loop never reached its end (the framing refused, or the
            // stream did): nothing more is read, and the refusal's own text
            // is the contract — the hook kills and reaps, and its result is
            // not rendered over the original failure.
            let _ = end(SessionEnd::Refused);
        }
        framed?;
        Ok(self.summary.take())
    }

    /// The response sink, for the tests' assertions.
    #[cfg(test)]
    pub(crate) fn into_out(self) -> O {
        self.out
            .expect("a scripted session keeps its response capture open")
    }

    /// Read requests until the child closes its stream, answering each.
    /// The handshake window's reads wait at most [`Self::read_deadline`]: a
    /// child that holds its pipes without writing its first request is a
    /// stall, and the run fails with the typed refusal naming the bound.
    /// Once the first request line has arrived the session is established
    /// and later reads block without a bound — a working child may spend
    /// arbitrarily long between calls.
    fn run_loop(&mut self, store: &Database, conn: &mut Connection) -> Result<(), HostError> {
        loop {
            let arrival = if self.established {
                self.input.next_blocked()
            } else {
                match self.input.next(self.read_deadline) {
                    Some(arrival) => arrival,
                    None => {
                        return Err(McpError::ReadDeadline {
                            deadline: self.read_deadline,
                        }
                        .into())
                    }
                }
            };
            match arrival {
                LineArrival::Eof => {
                    if !self.initialized {
                        return Err(McpError::InitializeMissing.into());
                    }
                    if let Some(end) = self.end.take() {
                        // Close the response pipe first: a child waiting
                        // for end-of-input on stdin can then exit, and the
                        // classification sees that exit — before the frame
                        // commits.
                        self.out = None;
                        end(SessionEnd::Complete)?;
                    }
                    return Ok(());
                }
                LineArrival::Exceeded => {
                    return Err(McpError::RecordTooLarge {
                        limit: self.read_bytes,
                    }
                    .into())
                }
                LineArrival::Failed(detail) => {
                    return Err(McpError::ReadFailed(detail).into());
                }
                LineArrival::Line(line) => {
                    // The first request line establishes the session: from
                    // here on the read deadline no longer applies.
                    self.established = true;
                    self.charge_stream(&line)?;
                    self.handle_line(store, conn, &line)?;
                }
            }
        }
    }

    /// Charge one read request line against the session's whole-run stream
    /// budget (ADR-0598). The budget is framed once at the session's
    /// build — [`StreamBudget::under`], the one derivation both sides of
    /// the mount share — and owns the one line-cost rule this delegates
    /// to, so the loop consults no policy and re-derives no arithmetic:
    /// a whole-import session's request stream refuses past the cap (the
    /// pre-commit bound its one transaction has), and a per-batch
    /// session's budget is cap-free, its bounds being the per-call caps
    /// and the store's live-graph limit.
    fn charge_stream(&mut self, line: &[u8]) -> Result<(), HostError> {
        self.budget.charge_read_line(line).map_err(McpError::from)?;
        Ok(())
    }

    /// Answer one request line: the method dispatch, of which the
    /// `initialize` arm owns the handshake. A notification (no `id`) is read
    /// and ignored.
    fn handle_line(
        &mut self,
        store: &Database,
        conn: &mut Connection,
        line: &[u8],
    ) -> Result<(), HostError> {
        let request = match mcp::decode_request(line) {
            Ok(request) => request,
            Err(error) => {
                let refusal = McpRefusal::from(error);
                return self.respond(Response::error(
                    Value::Null,
                    refusal.code(),
                    refusal.to_string(),
                ));
            }
        };
        // A partial move: the id is needed by every answer below, the
        // method and params by their own arms — no reason to clone the id
        // out of a request this loop owns.
        let Some(id) = request.id else {
            // A notification carries no id: nothing to answer. A method that
            // needs an answer cannot be a notification, so a child that
            // omitted its id would wait forever — the session fails instead
            // of hanging on a response that will never be written.
            if mcp::is_notification(&request.method) {
                return Ok(());
            }
            return Err(McpError::RequestWithoutId {
                method: request.method,
            }
            .into());
        };
        let response = match request.method.as_str() {
            mcp::METHOD_INITIALIZE => {
                if self.initialized {
                    Response::error(id, INVALID_PARAMS, "initialize was already answered")
                } else {
                    match require_protocol_version(request.params.as_ref()) {
                        // The mount answers its one revision whatever the
                        // child named — the value is the check verb's to
                        // compare, not this arm's.
                        Ok(_) => {
                            let facts = self.initialize_result(store)?;
                            self.initialized = true;
                            Response::success(id, facts)
                        }
                        Err(refusal) => Response::error(id, refusal.code(), refusal.to_string()),
                    }
                }
            }
            // Every other method is served only after the handshake: the
            // initialize arm above is the handshake's only owner.
            _ if !self.initialized => {
                return Err(McpError::InitializeNotFirst {
                    method: request.method,
                }
                .into())
            }
            // The mount's catalog is the wire kit's extension-mount rows —
            // the one tool table, no per-surface copy.
            mcp::METHOD_TOOLS_LIST => Response::success(id, mcp::extension_tools_list()),
            mcp::METHOD_TOOLS_CALL => self.call_tool(store, conn, id, request.params)?,
            other => {
                let refusal = McpRefusal::UnknownMethod {
                    method: other.to_string(),
                };
                Response::error(id, refusal.code(), refusal.to_string())
            }
        };
        self.respond(response)
    }

    /// The `initialize` result: the kit's typed [`ServerFacts`], so the
    /// writer here and the child's decoder share one schema. The revision
    /// is always the one this mount serves — MCP's answer for a client
    /// version the server does not speak; a child that speaks another
    /// revision fails the handshake on the decode. The capacity facts are
    /// the host-owned store's reading at the handshake, the baseline every
    /// later writing result counts down from; a store the engine refuses to
    /// read fails the handshake (`Error::Storage`, ADR-0099) rather than
    /// publishing an apparent empty, unlimited graph.
    fn initialize_result(&self, store: &Database) -> Result<Value, HostError> {
        Ok(initialize_facts(
            self.caps,
            self.fresh_store_required,
            self.commit,
            store_capacity(store)?,
        ))
    }

    /// One `tools/call`: the tool's result as MCP text content, or the
    /// typed refusal as a JSON-RPC error. A fatal tool failure ends the
    /// session without an answer — the run's own error text is the
    /// contract.
    fn call_tool(
        &mut self,
        store: &Database,
        conn: &mut Connection,
        id: Value,
        params: Option<Value>,
    ) -> Result<Response, HostError> {
        match self.dispatch_tool(store, conn, params) {
            Ok(result) => Ok(self.tool_response(id, &result)),
            Err(ToolFailure::Refusal(refusal)) => {
                Ok(Response::error(id, refusal.code(), refusal.to_string()))
            }
            Err(ToolFailure::Fatal(error)) => Err(error.into()),
        }
    }

    /// The success response for one tool result, bounded by the response
    /// record cap before it is written: a line the child's capped read
    /// would refuse is answered as the typed result-cap refusal instead, so
    /// an oversized read never surfaces as a malformed response.
    fn tool_response(&self, id: Value, result: &Value) -> Response {
        let text = result.to_string();
        let response = Response::success(id.clone(), mcp::tool_result(&text));
        let limit = self.caps.mcp_response_bytes();
        if response.to_line().len() as u64 > limit {
            let refusal = McpRefusal::ResultTooLarge { limit };
            return Response::error(id, refusal.code(), refusal.to_string());
        }
        response
    }

    /// Dispatch one tool call: `report_output` is this mount's own
    /// lifecycle call, every other tool is the shared layer's
    /// ([`relate_mcp::tools::Tools`]) under the manifest's commit
    /// framing.
    fn dispatch_tool(
        &mut self,
        store: &Database,
        conn: &mut Connection,
        params: Option<Value>,
    ) -> Result<Value, ToolFailure> {
        let (name, arguments) = tool_layer::call_parts(params.as_ref())?;
        if name == mcp::REPORT_OUTPUT {
            return self.report_output(arguments);
        }
        let outcome = self.run_tool(name, arguments, conn)?;
        self.render_tool(name, store, outcome)
    }

    /// Run one shared-layer tool under the commit framing: a writing tool
    /// runs inside the policy's transaction (its own under `per-batch`,
    /// the session's under `whole-import`), a reading one runs bare.
    fn run_tool(
        &mut self,
        name: &str,
        arguments: &Value,
        conn: &mut Connection,
    ) -> Result<ToolOutcome, ToolFailure> {
        let limits = self.limits();
        let call =
            |conn: &mut Connection| tool_layer::Tools::new(conn, limits).call(name, arguments);
        if !tool_layer::writes(name) {
            return Ok(call(conn)?);
        }
        match self.commit.write_batch(conn, call) {
            Ok(outcome) => Ok(outcome),
            Err(error) => Err(match self.commit {
                CommitFacts::PerBatch => ToolFailure::Refusal(McpRefusal::from(error)),
                CommitFacts::WholeImport => ToolFailure::Fatal(McpError::WholeImportBatchFailed {
                    detail: error.to_string(),
                }),
            }),
        }
    }

    /// Write one response line to the child.
    fn respond(&mut self, response: Response) -> Result<(), HostError> {
        let line = response.to_line();
        let out = self
            .out
            .as_mut()
            .expect("the response pipe is open until the session's end");
        write_line(out, &line)
            .map_err(|e| HostError::from(McpError::ResponseWriteFailed(e.to_string())))
    }
}

/// The `initialize` result (ADR-0442 §4): the handshake facts, the mount's
/// store facts and a capacity reading, as one construction — the mount
/// passes the host-owned store's live reading, the `check` verb an empty
/// unlimited one (it opens no store), so the two can never publish a
/// handshake that differs in shape.
pub(crate) fn initialize_facts(
    caps: Caps,
    fresh_store_required: bool,
    commit: CommitFacts,
    capacity: Capacity,
) -> Value {
    serde_json::to_value(ServerFacts {
        server_info: server_info(),
        protocol_version: MCP_PROTOCOL_VERSION.to_string(),
        capabilities: ServerCapabilities::tools_only(),
        fresh_store_required,
        commit_policy: commit,
        caps: caps.mcp_facts(),
        capacity,
    })
    .expect("the initialize facts serialize")
}

/// The host's identity in the handshake — the one `serverInfo` every
/// mount answer carries, and the `check` verb's stage line reads.
pub(crate) fn server_info() -> ServerInfo {
    ServerInfo {
        name: "relate".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

/// The `initialize` params's MCP handshake member: `protocolVersion` must be
/// a string, and the validated value is returned for the one caller that
/// reads it. The mount serves exactly one revision
/// ([`MCP_PROTOCOL_VERSION`]) and answers with it whatever the client asked
/// for — MCP's rule for a version the server does not speak — so the mount
/// arm ignores the value; a child that cannot speak the answer fails on the
/// decode. The `check` verb shares the requirement (and so its refusal
/// text) and compares the value itself, reporting a child of another
/// revision instead of answering it.
pub(crate) fn require_protocol_version(params: Option<&Value>) -> Result<&str, McpRefusal> {
    // The one member this check reads is borrowed straight out of the
    // request — the whole initialize params object (serverInfo included)
    // is never cloned to check a single field. The object read itself is
    // the wire kit's one labeled extraction, so the shape refusal's text
    // has one home on both sides of the mount.
    let member = mcp::object_member("params", params)
        .map_err(|detail| McpRefusal::from(ToolError::InvalidParams { detail }))?
        .and_then(|params| params.get("protocolVersion"));
    match member {
        Some(Value::String(version)) => Ok(version),
        _ => Err(McpRefusal::from(ToolError::InvalidParams {
            detail: "initialize requires a string \"protocolVersion\"".to_string(),
        })),
    }
}

/// The host-owned store's capacity right now, in the wire's typed triple —
/// the one reading the handshake and the writing tools publish, so a child
/// compares like with like. The numbers are the engine's own accounting
/// (`Database::memory_usage`, ADR-0042), read from the store the host owns,
/// never from a `Connection` or a wire-side estimate. A poisoned/fenced
/// handle is the engine's typed error rather than an apparent empty,
/// unlimited store (ADR-0099).
fn store_capacity(store: &Database) -> Result<Capacity, HostError> {
    let (used, limit) = store.memory_usage().map_err(HostError::from)?;
    Ok(Capacity::new(used, limit))
}

#[cfg(test)]
mod tests;
