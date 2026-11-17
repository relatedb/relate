//! The MCP server core: protocol dispatch plus the [`serve`] entry point
//! `relate serve --mcp PORT|stdio` calls (LABS-381, ADR-0368 retired the
//! standalone `relate-mcp` binary; mirroring how `relate-bolt`'s lib
//! exposes `serve`).

use std::fmt;

use relate::Database;
use relate_auth::AuthConfig;
use relate_wire::mcp::{decode_request, Response, RpcError, METHOD_NOT_FOUND};
use serde_json::{json, Value};

/// Which wire the server listens on (ADR-0232 §4: both stdio and streamable
/// HTTP; legacy SSE is deliberately absent).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transport {
    /// Newline-delimited JSON-RPC over stdin/stdout — for MCP hosts that
    /// spawn the server as a child process. No auth challenge (no trust
    /// boundary to cross).
    Stdio,
    /// Streamable HTTP: `POST` of complete JSON-RPC messages on
    /// `host:port`, RFC 7617 Basic auth on every request, stateless per
    /// request (no `Mcp-Session-Id`).
    Http {
        /// Bind host (e.g. `127.0.0.1`).
        host: String,
        /// Bind port (e.g. `8000`).
        port: u16,
    },
}

/// Server configuration: transport, auth, and tool behavior.
#[derive(Clone, Debug)]
pub struct McpServerConfig {
    /// The transport to serve on.
    pub transport: Transport,
    /// The resolved credential story (ADR-0232 §1). Only the HTTP transport
    /// consults it; stdio skips the challenge entirely.
    pub auth: AuthConfig,
    /// Optional tool-name prefix: with namespace `"db"`, the tools list as
    /// `db-get-schema`, `db-read-cypher`, `db-write-cypher` (mirrors the
    /// reference server's namespacing).
    pub namespace: String,
    /// When true, `write-cypher` is not listed and cannot be called (the
    /// reference server's `--read-only`).
    pub read_only: bool,
    /// Default per-label sample size for `get-schema`; `None` scans every
    /// node (the `-1` of the wire/CLI edge, converted on the way in). A
    /// call's `sample_size` argument overrides it.
    pub schema_sample_size: Option<u64>,
    /// Cap on simultaneously open HTTP connections (default
    /// [`relate_serve::DEFAULT_MAX_CONNECTIONS`]); an over-cap
    /// connection is accepted then immediately closed (ADR-0110's policy).
    /// The stdio transport has no sockets and ignores it.
    pub max_connections: usize,
    /// Per-read timeout on HTTP connections: a stalled reader is dropped
    /// instead of pinning its connection thread (default
    /// [`relate_serve::DEFAULT_READ_TIMEOUT`]).
    pub read_timeout: std::time::Duration,
    /// Absolute deadline for a complete HTTP request header block
    /// (default [`relate_serve::DEFAULT_HEADER_DEADLINE`], 10 s;
    /// ADR-0451): the per-read timeout is re-armed to the remaining time,
    /// so trickled headers cannot hold a worker. `Duration::ZERO` disables
    /// it. The stdio transport has no sockets and ignores it.
    pub header_deadline: std::time::Duration,
    /// The process-wide connection cap shared with the other listeners
    /// (ADR-0333), the same `HttpConfig` precedent. `None` builds a cap
    /// from [`Self::max_connections`].
    pub connection_cap: Option<std::sync::Arc<relate_serve::ConnectionCap>>,
    /// Absolute deadline for a complete HTTP request body
    /// (default [`relate_serve::DEFAULT_BODY_DEADLINE`], 60 s;
    /// ADR-0451): re-armed per read like the header deadline, so a trickled
    /// declared body cannot hold a worker. `Duration::ZERO` disables it.
    /// The stdio transport has no sockets and ignores it.
    pub body_deadline: std::time::Duration,
}

impl McpServerConfig {
    /// Validate the admission deadlines against the library bound
    /// (ADR-0451). The CLI resolver already enforces it; this is the guard
    /// for a directly constructed config entering the library.
    pub fn validate(&self) -> Result<(), String> {
        relate_serve::validate_admission_deadlines(self.header_deadline, self.body_deadline)
    }
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            transport: Transport::Stdio,
            auth: AuthConfig::None,
            namespace: String::new(),
            read_only: false,
            schema_sample_size: Some(1000),
            max_connections: relate_serve::DEFAULT_MAX_CONNECTIONS,
            read_timeout: relate_serve::DEFAULT_READ_TIMEOUT,
            header_deadline: relate_serve::DEFAULT_HEADER_DEADLINE,
            connection_cap: None,
            body_deadline: relate_serve::DEFAULT_BODY_DEADLINE,
        }
    }
}

/// Errors from [`serve`] and the transport loops.
#[derive(Debug)]
pub enum ServeError {
    /// An underlying I/O failure (bind, accept, read, write).
    Io(std::io::Error),
    /// A configuration value failed validation at an entry point.
    InvalidConfig(String),
}

impl fmt::Display for ServeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServeError::Io(err) => write!(f, "I/O error: {err}"),
            ServeError::InvalidConfig(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ServeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ServeError::Io(err) => Some(err),
            ServeError::InvalidConfig(_) => None,
        }
    }
}

impl From<std::io::Error> for ServeError {
    fn from(err: std::io::Error) -> Self {
        ServeError::Io(err)
    }
}

impl ServeError {
    /// A config validation failure: the one typed-error mapping for every
    /// entry point that accepts a directly constructed config.
    pub(crate) fn invalid_config(message: String) -> Self {
        Self::InvalidConfig(message)
    }
}

/// The wire/CLI edge's sample-size convention (reference-server
/// compatibility): `-1` means a full scan, other negatives are invalid.
/// Everything inside the server carries the typed `Option<u64>` instead
/// (`None` = full scan).
pub fn sample_size_at_edge(value: i64) -> Result<Option<u64>, String> {
    match value {
        -1 => Ok(None),
        n if n < 0 => Err(format!("sample_size must be -1 or non-negative, got {n}")),
        n => Ok(Some(n as u64)),
    }
}

/// The MCP server: a `Database` handle plus configuration, dispatching
/// JSON-RPC messages for `initialize`, `ping`, `tools/list` and
/// `tools/call`.
pub struct McpServer {
    db: std::sync::Arc<Database>,
    /// The configuration as resolved by the caller; accessors delegate so
    /// the knobs are stored and read in one place (the connection cap
    /// included).
    config: McpServerConfig,
}

impl McpServer {
    /// Build a server over `db` from its configuration.
    pub fn new(db: Database, config: McpServerConfig) -> Self {
        Self::new_shared(std::sync::Arc::new(db), config)
    }

    /// Build a server over a shared `db` handle: the embedding CLI
    /// (`relate serve`) opens one store and hands an `Arc` of it to
    /// every network surface it hosts (ADR-0232 Consequences).
    pub fn new_shared(db: std::sync::Arc<Database>, config: McpServerConfig) -> Self {
        Self { db, config }
    }

    /// The resolved auth configuration (the HTTP transport checks it).
    pub fn auth(&self) -> &AuthConfig {
        &self.config.auth
    }

    /// The database this server exposes.
    pub fn database(&self) -> &Database {
        &self.db
    }

    /// The tool-name namespace prefix (`""` or `"<namespace>-"`).
    pub fn namespace_prefix(&self) -> String {
        if self.config.namespace.is_empty() || self.config.namespace.ends_with('-') {
            self.config.namespace.clone()
        } else {
            format!("{}-", self.config.namespace)
        }
    }

    /// Whether `write-cypher` is disabled.
    pub fn read_only(&self) -> bool {
        self.config.read_only
    }

    /// The default `get-schema` sample size (`None` = full scan).
    pub fn schema_sample_size(&self) -> Option<u64> {
        self.config.schema_sample_size
    }

    /// Validate this server's admission deadlines (ADR-0451).
    pub fn validate(&self) -> Result<(), String> {
        self.config.validate()
    }

    // The admission-accessor family (cap, per-read timeout, both deadlines)
    // is crate-internal: an embedder configures through the public
    // `McpServerConfig`, and only this crate's own transport reads the
    // resolved values back. One visibility policy for the whole family
    // (the repo's pub(crate)-until-a-second-crate-needs-it rule).
    /// The configured process-wide cap (`None` builds one from
    /// [`Self::max_connections`]).
    pub(crate) fn connection_cap(&self) -> Option<std::sync::Arc<relate_serve::ConnectionCap>> {
        self.config.connection_cap.clone()
    }

    /// The HTTP connection cap (the ADR-0110 bar).
    pub(crate) fn max_connections(&self) -> usize {
        self.config.max_connections
    }

    /// The per-read timeout on HTTP connections.
    pub(crate) fn read_timeout(&self) -> std::time::Duration {
        self.config.read_timeout
    }

    /// The absolute header deadline on HTTP connections (zero = disabled).
    pub(crate) fn header_deadline(&self) -> std::time::Duration {
        self.config.header_deadline
    }

    /// The absolute body deadline on HTTP connections (zero = disabled).
    pub(crate) fn body_deadline(&self) -> std::time::Duration {
        self.config.body_deadline
    }

    /// Handle one JSON-RPC message body. Returns `None` for notifications
    /// (no response is written), `Some(response_json)` otherwise — including
    /// for decode refusals, which answer with the shared strict decode's
    /// JSON-RPC error envelope (`id: null`) and its pinned wording, the
    /// same policy the extension mount answers with.
    pub fn handle_message(&self, body: &str) -> Option<String> {
        let request = match decode_request(body.as_bytes()) {
            Ok(request) => request,
            Err(error) => {
                return Some(
                    Response::error(Value::Null, error.code(), error.to_string()).to_line(),
                );
            }
        };
        let outcome = match &request.params {
            Some(params) => self.dispatch(&request.method, params),
            None => self.dispatch(&request.method, &Value::Null),
        };
        // The notification rule frames the whole response: no id, no answer.
        request.id.map(|id| match outcome {
            Ok(result) => Response::success(id, result).to_line(),
            Err(error) => Response::error(id, error.code, error.message).to_line(),
        })
    }

    fn dispatch(&self, method: &str, params: &Value) -> Result<Value, RpcError> {
        match method {
            "initialize" => Ok(self.initialize_result(params)),
            "ping" => Ok(json!({})),
            m if relate_wire::mcp::is_notification(m) => Ok(Value::Null),
            "tools/list" => Ok(crate::tools::tool_specs(self)),
            "tools/call" => self.call_tool(params),
            other => Err(RpcError {
                code: METHOD_NOT_FOUND,
                message: format!("Method not found: {other}"),
            }),
        }
    }

    fn initialize_result(&self, params: &Value) -> Value {
        let requested = params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty());
        json!({
            // Echo the client's version when it sent one: the tool surface
            // is identical across the protocol versions in the field.
            "protocolVersion": requested.unwrap_or("2024-11-05"),
            "capabilities": {"tools": {}},
            "serverInfo": {
                "name": "relate-mcp",
                "version": env!("CARGO_PKG_VERSION"),
            },
        })
    }

    fn call_tool(&self, params: &Value) -> Result<Value, RpcError> {
        // The one extraction both surfaces run: a malformed payload is
        // refused in the shared vocabulary, with the same words the
        // extension mount answers.
        let (name, arguments) =
            crate::tools::call_parts(Some(params)).map_err(|error| RpcError {
                code: error.code(),
                message: error.to_string(),
            })?;
        // One envelope shape; the call's outcome only picks the flag.
        let (text, is_error) = match crate::tools::call_tool(self, name, arguments) {
            Ok(text) => (text, false),
            Err(text) => (text, true),
        };
        Ok(json!({"content": [{"type": "text", "text": text}], "isError": is_error}))
    }
}

/// Serve a `Database` on the configured transport — the entry point the
/// CLI's `serve --mcp PORT|stdio` command calls. Blocks until the transport ends
/// (stdin EOF for stdio; forever for the HTTP listener).
pub fn serve(db: Database, config: McpServerConfig) -> Result<(), ServeError> {
    serve_shared(std::sync::Arc::new(db), config)
}

/// [`serve`] over a shared `Database` handle: a host running several
/// surfaces on one store (the CLI's `serve` with both `--http` and `--mcp`)
/// passes an `Arc` clone to each.
pub fn serve_shared(
    db: std::sync::Arc<Database>,
    config: McpServerConfig,
) -> Result<(), ServeError> {
    config.validate().map_err(ServeError::invalid_config)?;
    let transport = config.transport.clone();
    let server = McpServer::new_shared(db, config);
    match transport {
        Transport::Stdio => {
            let stdin = std::io::stdin();
            let stdout = std::io::stdout();
            crate::stdio::serve_stdio(&server, stdin.lock(), stdout.lock())
        }
        Transport::Http { host, port } => {
            // Bind here so a taken port is a loud startup error before the
            // (diverging) serve loop starts.
            let listener = std::net::TcpListener::bind((host.as_str(), port))?;
            crate::http::serve_http_on(server, listener)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory_server() -> McpServer {
        let db = Database::open(":memory:").unwrap();
        McpServer::new(db, McpServerConfig::default())
    }

    #[test]
    fn initialize_echoes_protocol_version_and_server_info() {
        let server = memory_server();
        let response = server
            .handle_message(
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                     "protocolVersion":"2025-03-26","capabilities":{},
                     "clientInfo":{"name":"t","version":"0"}}}"#,
            )
            .unwrap();
        let value: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["id"], json!(1));
        assert_eq!(value["result"]["protocolVersion"], json!("2025-03-26"));
        assert_eq!(value["result"]["serverInfo"]["name"], json!("relate-mcp"));
        assert!(value["result"]["capabilities"]["tools"].is_object());
    }

    #[test]
    fn initialize_defaults_protocol_version() {
        let server = memory_server();
        let response = server
            .handle_message(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#)
            .unwrap();
        let value: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["result"]["protocolVersion"], json!("2024-11-05"));
    }

    #[test]
    fn notifications_initialized_gets_no_response() {
        let server = memory_server();
        assert!(server
            .handle_message(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_none());
    }

    #[test]
    fn ping_answers_empty_object() {
        let server = memory_server();
        let response = server
            .handle_message(r#"{"jsonrpc":"2.0","id":9,"method":"ping"}"#)
            .unwrap();
        let value: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["result"], json!({}));
    }

    #[test]
    fn unknown_method_is_method_not_found() {
        let server = memory_server();
        let response = server
            .handle_message(r#"{"jsonrpc":"2.0","id":2,"method":"resources/list"}"#)
            .unwrap();
        let value: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["error"]["code"], json!(METHOD_NOT_FOUND));
        assert!(value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("resources/list"));
    }

    #[test]
    fn garbage_gets_parse_error_with_null_id() {
        let server = memory_server();
        let response = server.handle_message("{ not json").unwrap();
        let value: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["id"], Value::Null);
        assert_eq!(value["error"]["code"], json!(relate_wire::mcp::PARSE_ERROR));
    }

    #[test]
    fn namespace_prefix_formats_like_the_reference_server() {
        let db = Database::open(":memory:").unwrap();
        let config = McpServerConfig {
            namespace: "db1".to_string(),
            ..McpServerConfig::default()
        };
        let server = McpServer::new(db, config);
        assert_eq!(server.namespace_prefix(), "db1-");
    }
}
