//! MCP (Model Context Protocol) server surface for Relate (ADR-0232 §4).
//!
//! The server speaks **hand-rolled JSON-RPC 2.0 over `serde_json`** — no
//! `rmcp`/external MCP SDK, per the charter — and exposes the three tools of
//! the reference `mcp-neo4j-cypher` server, adapted to Relate names:
//!
//! - `get_schema` — the store's schema (labels, relationship types, property
//!   keys with inferred types) in the official server's output shape,
//! - `read_cypher` — runs a read-only statement; a write statement is
//!   rejected with `"Only MATCH queries are allowed for read-query"`,
//! - `write_cypher` — runs a write statement and returns the mutation
//!   counters in the snake_case wire vocabulary (ADR-0223).
//!
//! Transports: **stdio** (newline-delimited JSON-RPC, for MCP hosts that
//! spawn the server as a child process) and **streamable HTTP** (`POST` of
//! JSON-RPC messages, for `relate serve --mcp PORT`-style listeners).
//! Legacy SSE is not implemented (superseded in the MCP spec).
//!
//! Auth follows the shared story (ADR-0232 §1): the stdio transport skips
//! the challenge entirely (no trust boundary — the parent spawned us); the
//! HTTP transport checks the RFC 7617 `Authorization: Basic` header through
//! [`relate_auth::AuthConfig`] and answers `401` with a
//! `WWW-Authenticate` challenge when the credential is missing or wrong.
//!
//! Session semantics on the HTTP transport are deliberately **stateless per
//! request** (the reference server runs `stateless_http=True` the same way):
//! no `Mcp-Session-Id` is issued, every POST carries a complete JSON-RPC
//! message, and the server holds no per-client state beyond the `Database`
//! itself.
//!
//! The CLI embedding point is [`serve`]: a `Database` plus a
//! [`McpServerConfig`] is all a caller needs.
//!
//! ```
//! use relate::Database;
//! use relate_auth::AuthConfig;
//! use relate_mcp::{McpServer, McpServerConfig};
//!
//! let db = Database::open(":memory:").unwrap();
//! let server = McpServer::new(db, McpServerConfig::default());
//!
//! // The initialize handshake answers with capabilities and serverInfo.
//! let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize",
//!   "params":{"protocolVersion":"2024-11-05","capabilities":{},
//!             "clientInfo":{"name":"doc","version":"0"}}}"#;
//! let response = server.handle_message(init).expect("initialize responds");
//! assert!(response.contains("\"serverInfo\""));
//! assert!(response.contains("relate-mcp"));
//!
//! // Notifications get no response.
//! let note = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
//! assert!(server.handle_message(note).is_none());
//! ```

mod errors;
mod http;
mod schema;
mod server;
mod stdio;
pub mod tools;

pub use http::{handle_http, serve_http_on, HttpResponse};
pub use server::{
    sample_size_at_edge, serve, serve_shared, McpServer, McpServerConfig, ServeError, Transport,
};
pub use stdio::serve_stdio;
