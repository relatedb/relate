//! The child-side MCP client kit (ADR-0442): the `initialize` handshake,
//! `tools/call` per record the child wants to emit, the capped rows chunker,
//! and the typed [`McpChildError`] refusals.
//!
//! The kit stays pure std + serde_json: a child links it, and a child never
//! touches a store. The host side — reading the child's requests,
//! dispatching tools against its own `Connection`, enforcing the caps,
//! writing the responses — stays in the CLI: it is host execution, not a
//! shared mechanism.
//!
//! [`serve`] is the whole `main` of a first-party child: the stdio client,
//! the handshake, the import closure, and the non-zero exit that reports a
//! failure on stderr — the one channel the host renders.

use std::io::{BufRead, BufReader, Stdout, Write};

use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::read_cap::{read_line_capped, write_line, CappedLine};
use crate::{MAX_ROWS_PER_STATEMENT, MAX_STATEMENT_BYTES};

use super::budget::{StreamBudget, StreamCapExceeded};
use super::protocol::{
    Request, Response, ResponseError, ServerFacts, JSONRPC_VERSION, MCP_PROTOCOL_VERSION,
    METHOD_INITIALIZE, METHOD_INITIALIZED, METHOD_TOOLS_CALL,
};
use super::tools::{
    ToolCallResult, ToolContent, REPORT_OUTPUT, STREAM_CAP_REMEDY, UPDATE_SCHEMA, WRITE_CYPHER,
};

/// The framing bytes a chunk's own serialization adds around the rows it
/// carries: serde_json's compact form brackets an array with exactly `[`
/// and `]`, one byte each. The chunker counts this without serializing —
/// the assumption is serde_json's, stated here once instead of implied by
/// two string constants that were never written.
const ARRAY_FRAMING_BYTES: u64 = 2;

/// A refused child-side exchange — typed variants with pinned texts, the
/// host's `ProtocolError` pattern on the child side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpChildError {
    /// The host's request envelope was absent, oversized, or did not
    /// decode as the child's envelope type.
    Envelope {
        /// The refusal, verbatim.
        detail: String,
    },
    /// Writing a request failed: the pipe broke.
    Write {
        /// The I/O refusal, verbatim.
        detail: String,
    },
    /// Reading the host's response failed.
    Read {
        /// The I/O refusal, verbatim.
        detail: String,
    },
    /// The child's whole request stream would exceed the stream cap
    /// the handshake published (ADR-0598) — the refusal names the
    /// framing it guards and the two ways out. A `per-batch` session is
    /// never refused on it: the per-call caps and the store's live-graph
    /// limit are its bounds.
    StreamTooLarge {
        /// The cap the stream would have exceeded, in bytes.
        limit: u64,
    },
    /// One row's serialized bytes alone would fill a call past the statement
    /// cap: no chunking can carry it (ADR-0430 §4's cap, re-anchored per
    /// call).
    RowTooLarge {
        /// The cap the call would have exceeded, in bytes.
        limit: u64,
    },
    /// A response line did not decode as a JSON-RPC 2.0 response.
    MalformedResponse {
        /// The refusal, verbatim.
        detail: String,
    },
    /// A response answered a different id than the request sent.
    IdMismatch {
        /// The id the child sent.
        expected: u64,
        /// The id the response carried.
        got: Value,
    },
    /// The host answered with a JSON-RPC error.
    Rpc {
        /// The error code.
        code: i64,
        /// The error message.
        message: String,
    },
    /// The host's `tools/call` result reported a tool failure.
    Tool {
        /// The tool's failure text.
        text: String,
    },
    /// The host answered with another MCP revision than this child speaks.
    ProtocolMismatch {
        /// The revision the host reported.
        spoke: String,
        /// The revision this child speaks.
        expected: &'static str,
    },
}

impl std::fmt::Display for McpChildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Envelope { detail } => write!(f, "invalid request envelope: {detail}"),
            Self::Write { detail } => write!(f, "failed to write an MCP request: {detail}"),
            Self::Read { detail } => write!(f, "failed to read an MCP response: {detail}"),
            Self::StreamTooLarge { limit } => write!(
                f,
                "the whole-import MCP request stream exceeds the {limit}-byte stream cap; \
                 {STREAM_CAP_REMEDY}"
            ),
            Self::RowTooLarge { limit } => {
                write!(f, "a single row exceeds the {limit}-byte statement cap")
            }
            Self::MalformedResponse { detail } => {
                write!(f, "malformed MCP response: {detail}")
            }
            Self::IdMismatch { expected, got } => write!(
                f,
                "the host answered id {got}, but the child sent id {expected}"
            ),
            Self::Rpc { code, message } => {
                write!(f, "the host refused the call ({code}): {message}")
            }
            Self::Tool { text } => f.write_str(text),
            Self::ProtocolMismatch { spoke, expected } => write!(
                f,
                "the host speaks MCP protocol '{spoke}'; this child speaks '{expected}'"
            ),
        }
    }
}

impl std::error::Error for McpChildError {}

impl From<StreamCapExceeded> for McpChildError {
    /// The kit's stream-cap refusal: the budget's typed payload, rendered
    /// with the framing it guards and the remedy (ADR-0598) — the one
    /// conversion both charge sites convert through.
    fn from(refused: StreamCapExceeded) -> Self {
        Self::StreamTooLarge {
            limit: refused.limit,
        }
    }
}

impl From<McpChildError> for String {
    /// The `Display` text channel a child's failure report rides: a child's
    /// `run` returns `Result<(), String>`, so `?` on a kit call renders
    /// through this — no per-call-site adapter.
    fn from(error: McpChildError) -> Self {
        error.to_string()
    }
}

/// The outcome of [`McpChild::read_capped_line`]: one line with its
/// terminator stripped, the stream's end, or a line past the cap.
enum LineRead {
    /// One line, its newline stripped.
    Line(Vec<u8>),
    /// The stream ended before the next line.
    Eof,
    /// The line exceeded the cap.
    Exceeded,
}

/// The child's real stdio pair — what a first-party MCP child uses, and what
/// [`serve`] hands the import closure.
pub type StdioChild = McpChild<BufReader<std::io::Stdin>, Stdout>;

/// A first-party child's whole `main` (ADR-0442 §1, ADR-0447 §4): the stdio
/// client, the handshake, the import, and the failure report — the tail
/// every first-party child would otherwise re-write. `main` is one call:
/// `serve(NAME, env!("CARGO_PKG_VERSION"), run)`, where `run` is the child's
/// import over the decoded envelope and the client.
///
/// A child's failure is a non-zero exit with its text on stderr — the one
/// channel the host renders (`the extension child exited with 1: …`) — and
/// that covers both a broken handshake and an import that refuses. Nothing
/// resumable follows a failure: the session is over, so the exit is the
/// report.
pub fn serve<T, F>(name: &str, version: &str, import: F)
where
    T: DeserializeOwned,
    F: FnOnce(&mut StdioChild, T) -> Result<(), String>,
{
    let outcome = StdioChild::start(
        BufReader::new(std::io::stdin()),
        std::io::stdout(),
        name,
        version,
    )
    .map_err(String::from)
    .and_then(|(mut child, _facts, envelope)| import(&mut child, envelope));
    if let Some(text) = outcome.err() {
        eprintln!("{text}");
        std::process::exit(1);
    }
}

/// The child's MCP client: constructed by the `initialize` handshake
/// ([`Self::start`]), then JSON-RPC requests on `out` and the host's
/// responses from `input`.
///
/// [`Self::call`] sends one `tools/call` and returns the tool's result
/// JSON; [`Self::write_rows`] is the chunked `write_cypher`; and
/// [`Self::report_output`] is the `report_output` convenience. The
/// session's whole-run budget is framed by the handshake that builds
/// the client, so every written line charges it: under `whole-import`
/// framing the output side refuses before it would cross the published
/// stream cap, and under `per-batch` framing there is no whole-run cap —
/// a long import streams every chunk the per-call caps admit instead of
/// refusing at a byte the host would admit (ADR-0598).
pub struct McpChild<I: BufRead, O: Write> {
    input: I,
    out: O,
    next_id: u64,
    /// The whole-run request-stream budget [`Self::send`] charges every
    /// written line against — framed by the handshake that constructed
    /// the client ([`StreamBudget::under`], ADR-0598), with the
    /// handshake's own initialize line already charged, so a
    /// whole-import child's accounting matches the host's line-1-onward
    /// counting. Capped exactly under `whole-import` framing, cap-free
    /// under `per-batch`, whose bounds are the per-call caps and the
    /// store's live-graph limit.
    budget: StreamBudget,
}

impl<I: BufRead, O: Write> McpChild<I, O> {
    /// Build the client by reading the request envelope (one line,
    /// bounded) over `input` and performing the `initialize` handshake:
    /// send the MCP revision this kit speaks with its client identity on
    /// `out`, decode the host's
    /// [`ServerFacts`](super::protocol::ServerFacts), frame the
    /// session's whole-run budget from them, and confirm the handshake
    /// with `notifications/initialized` — returning the started client
    /// beside the facts and the decoded envelope. The only construction:
    /// a client exists only with its budget framed, so every later send
    /// charges it (ADR-0598).
    ///
    /// The envelope is read first because the host writes it before it
    /// starts reading requests; the order is the wire's, not a choice.
    /// The facts are returned here and not retained — with one
    /// exception: the commit policy and its stream cap stay on the
    /// client, the one session-long fact the kit's own output
    /// accounting consumes (ADR-0598), so the child's whole-run guard
    /// and the host's admit the same bytes under the same framing. A
    /// host that answers with another MCP revision fails the handshake —
    /// the mount speaks one published revision, never a silent
    /// downgrade.
    pub fn start<T: DeserializeOwned>(
        mut input: I,
        mut out: O,
        name: &str,
        version: &str,
    ) -> Result<(Self, ServerFacts, T), McpChildError> {
        let envelope = Self::read_envelope_line(&mut input)?;
        let envelope: T =
            serde_json::from_slice(&envelope).map_err(|e| McpChildError::Envelope {
                detail: e.to_string(),
            })?;
        let request = Request {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: Some(Value::from(1)),
            method: METHOD_INITIALIZE.to_string(),
            params: Some(json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": name, "version": version},
            })),
        };
        // The handshake's initialize line is written before the budget
        // exists — the policy that frames the session is inside the
        // answer — so it ships unguarded: only the host's own counting
        // enforces a cap on it. Its cost is charged into the budget once
        // the framing is known (ADR-0598), so every line after the
        // handshake counts exactly as the host counts, and the child
        // refuses those before the host kills it.
        let initialize = request.to_line();
        Self::write_request_line(&mut out, &initialize)?;
        let result = Self::read_result(&mut input, 1)?;
        let facts: ServerFacts =
            serde_json::from_value(result).map_err(|e| McpChildError::MalformedResponse {
                detail: e.to_string(),
            })?;
        if facts.protocol_version != MCP_PROTOCOL_VERSION {
            return Err(McpChildError::ProtocolMismatch {
                spoke: facts.protocol_version,
                expected: MCP_PROTOCOL_VERSION,
            });
        }
        // The session's whole-run stream budget, framed by the one fact
        // that says whether the session retains work across calls
        // (ADR-0598) — capped exactly under `whole-import`, cap-free
        // under `per-batch` — with the initialize line charged into it.
        let mut budget = StreamBudget::under(facts.commit_policy, facts.caps.max_stream_bytes);
        budget.charge_written_line(&initialize)?;
        let mut client = Self {
            input,
            out,
            next_id: 1,
            budget,
        };
        // MCP's handshake ends with the client's confirmation. It is the
        // protocol's shape, not a host requirement — the host reads every
        // notification and answers none.
        client.notify(METHOD_INITIALIZED)?;
        Ok((client, facts, envelope))
    }

    /// Send one `tools/call` and return the tool's result JSON.
    pub fn call(&mut self, tool: &str, arguments: Value) -> Result<Value, McpChildError> {
        let id = self.next_id();
        let request = Request {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: Some(Value::from(id)),
            method: METHOD_TOOLS_CALL.to_string(),
            params: Some(json!({"name": tool, "arguments": arguments})),
        };
        self.send(&request)?;
        let result = Self::read_result(&mut self.input, id)?;
        decode_tool_result(result)
    }

    /// Write `rows` through the `write_cypher` tool, one call per chunk
    /// under the mount's caps (ADR-0430 §4, re-anchored per call): at most
    /// [`MAX_ROWS_PER_STATEMENT`] rows per call, at most
    /// [`MAX_STATEMENT_BYTES`] of serialized rows per call — the chunker
    /// counts the array it is building, so the call it sends is the call
    /// the host admits — and a row that cannot fit a call of its own is the
    /// typed [`McpChildError::RowTooLarge`]. A zero-row phase writes
    /// nothing: its statement's empty batch is a no-op either way.
    pub fn write_rows(
        &mut self,
        statement: &str,
        rows: impl Iterator<Item = Value>,
    ) -> Result<(), McpChildError> {
        // The cap arithmetic runs in the cap's own type (u64, like send's
        // stream accounting): the measured row length is cast once, the
        // bound never.
        let mut chunk: Vec<Value> = Vec::new();
        let mut bytes = ARRAY_FRAMING_BYTES;
        for row in rows {
            let row_bytes = serde_json::to_string(&row)
                .expect("a parameter row serializes")
                .len() as u64;
            // A row that cannot fit an empty chunk alone is refused here —
            // the one bound the chunk arithmetic below cannot recover from:
            // after a flush the chunk is empty and this check is what stops
            // the row from opening a fresh chunk that ships over the cap.
            if ARRAY_FRAMING_BYTES + row_bytes > MAX_STATEMENT_BYTES {
                return Err(McpChildError::RowTooLarge {
                    limit: MAX_STATEMENT_BYTES,
                });
            }
            // The array this row would join: its bytes so far, the comma a
            // second row costs, and the row itself. Over the cap, the chunk
            // ships without the row — which opens the next call instead.
            // The empty-chunk case cannot reach this flush: the bound above
            // already refused the row it would have carried.
            let comma = u64::from(!chunk.is_empty());
            if bytes + comma + row_bytes > MAX_STATEMENT_BYTES {
                self.write_rows_chunk(statement, std::mem::take(&mut chunk))?;
                bytes = ARRAY_FRAMING_BYTES;
            }
            bytes += u64::from(!chunk.is_empty()) + row_bytes;
            chunk.push(row);
            if chunk.len() == MAX_ROWS_PER_STATEMENT {
                self.write_rows_chunk(statement, std::mem::take(&mut chunk))?;
                bytes = ARRAY_FRAMING_BYTES;
            }
        }
        if !chunk.is_empty() {
            self.write_rows_chunk(statement, chunk)?;
        }
        Ok(())
    }

    /// One `write_cypher` call carrying `chunk` as the statement's `$rows`
    /// parameter — the chunk writer [`Self::write_rows`] flushes through.
    fn write_rows_chunk(
        &mut self,
        statement: &str,
        chunk: Vec<Value>,
    ) -> Result<(), McpChildError> {
        self.call(
            WRITE_CYPHER,
            json!({"statement": statement, "parameters": {"rows": chunk}}),
        )
        .map(|_| ())
    }

    /// Execute one DDL statement (an index or a constraint) through the
    /// `update_schema` tool, returning the tool's result JSON — the schema
    /// names the call created.
    pub fn update_schema(&mut self, statement: &str) -> Result<Value, McpChildError> {
        self.call(UPDATE_SCHEMA, json!({"statement": statement}))
    }

    /// Report the run's summary (text plus optional starters) through the
    /// `report_output` tool.
    pub fn report_output(&mut self, text: &str, starters: &[String]) -> Result<(), McpChildError> {
        let arguments = if starters.is_empty() {
            json!({"text": text})
        } else {
            json!({"text": text, "starters": starters})
        };
        self.call(REPORT_OUTPUT, arguments).map(|_| ())
    }

    /// The next correlation id (the handshake took id 1).
    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// One capped line off the input, its terminator stripped: the read the
    /// envelope and every response share, so the cap, the newline strip and
    /// the I/O fan-out exist once. Each caller words its own refusal for
    /// the three outcomes. An associated function so the handshake can
    /// read before the client it constructs exists
    /// ([`Self::start`](McpChild::start)).
    fn read_capped_line(input: &mut I) -> std::io::Result<LineRead> {
        Ok(match read_line_capped(input, MAX_STATEMENT_BYTES)? {
            CappedLine::Line(mut line) => {
                if line.last() == Some(&b'\n') {
                    line.pop();
                }
                LineRead::Line(line)
            }
            CappedLine::Eof => LineRead::Eof,
            CappedLine::Exceeded => LineRead::Exceeded,
        })
    }

    /// Read the one-line request envelope under the statement cap.
    fn read_envelope_line(input: &mut I) -> Result<Vec<u8>, McpChildError> {
        match Self::read_capped_line(input) {
            Ok(LineRead::Line(line)) => Ok(line),
            Ok(LineRead::Eof) => Err(McpChildError::Envelope {
                detail: "the host wrote no request envelope".to_string(),
            }),
            Ok(LineRead::Exceeded) => Err(McpChildError::Envelope {
                detail: format!(
                    "the request envelope exceeds the {MAX_STATEMENT_BYTES}-byte statement cap"
                ),
            }),
            Err(e) => Err(McpChildError::Read {
                detail: e.to_string(),
            }),
        }
    }

    /// Send one notification: a request-shaped message without an id, which
    /// the host never answers.
    fn notify(&mut self, method: &str) -> Result<(), McpChildError> {
        let request = Request {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: None,
            method: method.to_string(),
            params: None,
        };
        self.send(&request)
    }

    /// Write one request line, charging the whole-run stream budget
    /// before the write — under `whole-import` framing only (ADR-0598):
    /// the budget owns the one line-cost rule
    /// ([`StreamBudget::charge_written_line`]), and a `per-batch`
    /// session writes without a whole-run bound — the per-call caps and
    /// the host store's live-graph limit are its bounds.
    fn send(&mut self, request: &Request) -> Result<(), McpChildError> {
        let line = request.to_line();
        self.budget.charge_written_line(&line)?;
        Self::write_request_line(&mut self.out, &line)
    }

    /// Write one serialized request line, mapping the pipe failure to
    /// the kit's one [`McpChildError::Write`] refusal — the writer both
    /// [`Self::send`] and the handshake's initialize line share, so the
    /// pipe-failure mapping exists once. An associated function over
    /// `out`, the write-side twin of [`Self::read_capped_line`]'s shape
    /// over the input: the handshake writes before the client it
    /// constructs exists.
    fn write_request_line(out: &mut O, line: &str) -> Result<(), McpChildError> {
        write_line(out, line).map_err(|e| McpChildError::Write {
            detail: e.to_string(),
        })
    }

    /// Read one response, correlate it with `id`, and return its result.
    /// An associated function so the handshake can read the initialize
    /// answer before the client it constructs exists.
    fn read_result(input: &mut I, id: u64) -> Result<Value, McpChildError> {
        let line = match Self::read_capped_line(input) {
            Ok(LineRead::Line(line)) => line,
            Ok(LineRead::Eof) => {
                return Err(McpChildError::MalformedResponse {
                    detail: "the host closed the response stream".to_string(),
                })
            }
            Ok(LineRead::Exceeded) => {
                return Err(McpChildError::MalformedResponse {
                    detail: format!(
                        "the response exceeds the {MAX_STATEMENT_BYTES}-byte statement cap"
                    ),
                })
            }
            Err(e) => {
                return Err(McpChildError::Read {
                    detail: e.to_string(),
                })
            }
        };
        let response: Response =
            serde_json::from_slice(&line).map_err(|e| McpChildError::MalformedResponse {
                detail: e.to_string(),
            })?;
        // The kit sends numeric ids; any other id (a string, null) is a
        // different correlation, so the comparison is numeric.
        if response.id.as_u64() != Some(id) {
            return Err(McpChildError::IdMismatch {
                expected: id,
                got: response.id,
            });
        }
        match response.into_result() {
            Ok(result) => Ok(result),
            Err(ResponseError::Rpc(error)) => Err(McpChildError::Rpc {
                code: error.code,
                message: error.message,
            }),
            Err(ResponseError::Shape(detail)) => Err(McpChildError::MalformedResponse { detail }),
        }
    }
}

/// Decode the host's `tools/call` result: the one text content parsed as the
/// tool's JSON result, or the tool's failure text. Exactly one content item
/// is the contract — a second item would be silently dropped, so its
/// presence is refused instead.
fn decode_tool_result(result: Value) -> Result<Value, McpChildError> {
    let result: ToolCallResult =
        serde_json::from_value(result).map_err(|e| McpChildError::MalformedResponse {
            detail: e.to_string(),
        })?;
    let [item] = result.content.as_slice() else {
        return Err(McpChildError::MalformedResponse {
            detail: format!(
                "the tools/call result must carry exactly one content item, got {}",
                result.content.len()
            ),
        });
    };
    let ToolContent::Text { text } = item;
    if result.is_error {
        return Err(McpChildError::Tool { text: text.clone() });
    }
    serde_json::from_str(text).map_err(|e| McpChildError::MalformedResponse {
        detail: format!("the tools/call text is not JSON: {e}"),
    })
}

// The kit's tests live in `child/tests.rs` (the dir-module pattern the
// importer children's plan and reader use, LABS-421) so this file stays
// under the repo's 1k-line budget.
#[cfg(test)]
mod tests;
