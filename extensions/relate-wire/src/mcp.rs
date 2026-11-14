//! The extension mount's MCP stdio wire (ADR-0442): the shared facts both
//! sides of the pipe need.
//!
//! The mount is MCP stdio (ADR-0447): the child writes JSON-RPC 2.0
//! requests on stdout and reads the host's JSON-RPC responses from stdin,
//! after one request envelope line on stdin — its resolved flag values. The
//! child is the MCP client (it drives the import), the host is the MCP
//! server (it owns the store).
//!
//! The kit has four parts, one module each:
//!
//! - `protocol` — the JSON-RPC 2.0 records ([`Request`], [`Response`],
//!   [`RpcError`]) with their strict decode, the error codes, the MCP
//!   revision this mount serves ([`MCP_PROTOCOL_VERSION`]) and its
//!   notification names, the one shared object-or-refuse read
//!   ([`object_member`]) the params and member extractions of both sides
//!   consult, the [`ServerFacts`] the `initialize` result carries (MCP's
//!   revision, capabilities and server identity, plus the mount's store
//!   facts — the commit policy, the caps, and the store's [`Capacity`]),
//!   and the request framing bound derived from the statement cap;
//! - `budget` — the whole-run [`StreamBudget`] both sides of the mount
//!   charge their request lines against under whole-import framing
//!   (ADR-0598): the one derivation of whether a session's stream
//!   answers to a cap, and the one line-cost rule a written or read
//!   line charges;
//! - `tools` — the one tool table both MCP surfaces serve ([`TOOL_TABLE`],
//!   [`tools_for`], [`tools_list`]): every name, description, input schema
//!   and annotation, plus the pinned rejection texts; the extension mount
//!   lists its five as they are, the product server its three under its
//!   namespace. The file also carries the [`ToolCallResult`] envelope the
//!   host writes and the child decodes, and the two writing results
//!   ([`WriteCypherResult`], [`BatchCypherResult`]) with the store's
//!   remaining bytes;
//! - `child` — the child-side client kit ([`McpChild`], [`serve`]): the
//!   initialize handshake, `tools/call` per record, the chunked
//!   `write_cypher`, and the typed [`McpChildError`] refusals.
//!
//! The host side — reading the child's requests, dispatching tools against
//! its own `Connection`, enforcing the caps, writing the responses — stays
//! in the CLI (`surfaces/relate-cli/src/extensions/mcp.rs`): it is host
//! execution, not a shared mechanism. The manifest's `protocol` and the
//! handshake's `protocolVersion` are the same fact: [`MCP_PROTOCOL_VERSION`],
//! the one revision this mount serves.
//!
//! The kit stays pure std + serde_json: a child links it, and a child never
//! touches a store.

mod budget;
mod child;
mod protocol;
mod tools;

pub use budget::{StreamBudget, StreamCapExceeded};
pub use child::{serve, McpChild, McpChildError, StdioChild};
pub use protocol::{
    decode_request, is_notification, object_member, request_bytes_cap, Capacity, CapsFacts,
    CommitFacts, Request, RequestError, Response, ResponseError, RpcError, ServerCapabilities,
    ServerFacts, ServerInfo, ToolsCapability, INVALID_PARAMS, INVALID_REQUEST, JSONRPC_VERSION,
    MCP_PROTOCOL_VERSION, METHOD_INITIALIZE, METHOD_INITIALIZED, METHOD_NOT_FOUND,
    METHOD_TOOLS_CALL, METHOD_TOOLS_LIST, PARSE_ERROR, REQUEST_ENVELOPE_HEADROOM,
};
pub use tools::{
    extension_tools_list, tool_result, tools_for, tools_list, BatchCypherResult, Mount, Served,
    ToolCallResult, ToolContent, ToolSpec, WriteCypherResult, BATCH_CYPHER, GET_SCHEMA,
    READ_CYPHER, READ_ONLY_REJECTION, REPORT_OUTPUT, SCHEMA_ONLY_REJECTION, STREAM_CAP_REMEDY,
    TOOL_TABLE, UPDATE_SCHEMA, WRITE_CYPHER, WRITE_ONLY_REJECTION,
};
