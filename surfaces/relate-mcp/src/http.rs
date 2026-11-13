//! The streamable-HTTP transport (ADR-0232 §4): `POST` of complete
//! JSON-RPC messages, answered with `application/json`. Session semantics
//! are deliberately **stateless per request** — no `Mcp-Session-Id` is
//! issued and the server keeps no per-client state, matching the reference
//! server's `stateless_http=True`. Legacy SSE is not implemented.
//!
//! Auth is the shared story (ADR-0232 §1): every request must carry the
//! RFC 7617 `Authorization: Basic` credential from
//! [`relate_auth::AuthConfig`]; a missing or wrong credential gets
//! `401` with a `WWW-Authenticate: Basic realm="relate"` challenge
//! (the shared [`relate_auth::REALM`]).
//!
//! The wire layer is the **shared** minimal HTTP/1.1 stack of
//! [`relate_http::http`] (request-head parsing via `httparse` out of a
//! bounded buffer, capped body read; one response, `Connection: close`) —
//! the charter rules out a heavy HTTP stack for this surface, and one
//! implementation serves both HTTP surfaces. [`handle_http`] is the
//! transport handler proper, exposed so tests drive it without a socket.

use std::net::TcpListener;
use std::sync::Arc;

use relate_http::http::{
    drain_reader, read_admitted_request, write_response, AdmissionOutcome, Cors, DrainPlan,
    OriginAllowlist, ReadLimits, REJECT_DRAIN_TIMEOUT,
};
use relate_serve::{
    serve_capped, ConnectionCap, DEFAULT_CONNECTION_STACK_BYTES, DEFAULT_MAX_BODY_BYTES,
};

use crate::server::{McpServer, ServeError};

/// One HTTP response: status code, extra headers, body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    /// The HTTP status code (200, 202, 401, 405, …).
    pub status: u16,
    /// Extra response headers (`Content-Type` and `Content-Length` are
    /// added by the writer; `WWW-Authenticate` rides here).
    pub headers: Vec<(String, String)>,
    /// The response body (empty for 202 notification acknowledgements).
    pub body: String,
}

impl HttpResponse {
    /// An error response with the MCP `{"error": message}` body — the one
    /// envelope constructor (built with `serde_json`, never string
    /// interpolation, so any message stays valid JSON).
    pub fn error(status: u16, message: impl std::fmt::Display) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: serde_json::json!({"error": message.to_string()}).to_string(),
        }
    }
}

/// The transport's POST-only refusal, also the safe answer to a preflight.
fn post_only_response() -> HttpResponse {
    let mut response = HttpResponse::error(405, "streamable HTTP MCP is POST-only");
    response
        .headers
        .push(("Allow".to_string(), "POST".to_string()));
    response
}

/// Answer an already-admitted message: the method gate and the JSON-RPC
/// dispatch, with no credential check. Private — only the admission-gated
/// connection path may call it, so there is exactly one live enforcement
/// point per path.
fn answer(server: &McpServer, method: &str, body: &str) -> HttpResponse {
    // RFC 9110 methods are case-sensitive (browsers only ever send
    // uppercase POST) — one exact predicate, shared by every POST gate.
    if method != "POST" {
        return post_only_response();
    }
    match server.handle_message(body) {
        Some(response) => HttpResponse {
            status: 200,
            headers: Vec::new(),
            body: response,
        },
        None => HttpResponse {
            status: 202,
            headers: Vec::new(),
            body: String::new(),
        },
    }
}

/// Handle one HTTP request against the MCP server, enforcing the credential.
///
/// This is the direct-caller entry: it always runs the 401-with-challenge
/// arm (skipped only when the server's auth is
/// [`relate_auth::AuthConfig::None`]) before delegating to the private
/// post-auth [`answer`]. The admission-gated connection path already ran the
/// shared gate, so it calls [`answer`] directly and the credential is never
/// checked twice.
///
/// After the gate:
/// - any method other than `POST` → `405`;
/// - a request message → `200` with the JSON-RPC response body (parse
///   errors also answer `200` with a JSON-RPC error envelope, so the client
///   always gets a protocol-level answer);
/// - a notification → `202 Accepted` with an empty body.
pub fn handle_http(
    server: &McpServer,
    method: &str,
    authorization: Option<&str>,
    body: &str,
) -> HttpResponse {
    if let Err(error) = server.auth().check_authorization(authorization) {
        let failure = server.auth().http_failure(error);
        let mut response = HttpResponse::error(failure.status, failure.message);
        response.headers = failure.challenge.into_iter().collect();
        return response;
    }
    answer(server, method, body)
}

/// Serve the streamable-HTTP transport on an already-bound listener: the
/// embedding CLI (`relate serve`) binds every surface's socket itself so
/// a taken port is a loud startup error before the startup banner prints,
/// and [`crate::serve_shared`] binds for the standalone binary's
/// `--transport http`. The accept loop is the shared [`serve_capped`]
/// (thread-per-connection under [`McpServer::max_connections`],
/// accept-then-close over cap, accept errors logged and retried — the
/// ADR-0110 policy, on sync primitives). Diverges by design: there is no
/// shutdown condition; the process owns the listener's lifetime.
pub fn serve_http_on(server: McpServer, listener: TcpListener) -> Result<(), ServeError> {
    // The listener entry validates and builds the admission limits in one
    // step (`ReadLimits::new` is the validating constructor), and returns
    // the typed error rather than ending the embedder's process; every
    // connection then carries the limits instead of re-deriving. On success
    // the accept loop diverges.
    let limits = ReadLimits::new(
        server.header_deadline(),
        server.body_deadline(),
        DEFAULT_MAX_BODY_BYTES,
        server.read_timeout(),
    )
    .map_err(ServeError::invalid_config)?;
    let cap = server
        .connection_cap()
        .unwrap_or_else(|| Arc::new(ConnectionCap::new(server.max_connections())));
    let server = Arc::new(server);
    serve_capped(
        listener,
        "mcp",
        DEFAULT_CONNECTION_STACK_BYTES,
        cap,
        move |stream| handle_connection(stream, &server, &limits),
    )
}

/// Read one request, write one response, close (no keep-alive: the
/// stateless-per-request design needs nothing more). The shared reader's
/// protections apply: headers and body are capped before allocation, a
/// malformed `Content-Length` is an error rather than a silent zero, and a
/// non-UTF-8 body is never lossy-mangled — every malformed request is
/// answered `400` (the JSON-RPC layer still gets only well-formed input).
///
/// One admission-to-response path: the shared pipeline
/// ([`read_admitted_request`] — browser guard, auth challenge with the
/// ADR-0139 delay, then the bounded body read) decides before a byte of the
/// body is buffered; every outcome maps to one [`HttpResponse`] plus a
/// [`DrainPlan`] (a rejection drains the already-sent body after the answer,
/// so the close does not RST the response away).
fn handle_connection(
    stream: std::net::TcpStream,
    server: &McpServer,
    limits: &ReadLimits,
) -> Result<(), ServeError> {
    // The stalled-reader guard McpServerConfig::read_timeout documents:
    // propagate a failure to install it (the serve_capped closure logs the
    // session error) — the same policy as the HTTP surface.
    stream.set_read_timeout(Some(server.read_timeout()))?;
    // A second handle to the same socket for writing, so the buffered
    // reader can borrow the first. It also arms the header deadline: both
    // handles name the same socket, so a timeout set on this one governs
    // reads on the other.
    let mut writer = stream.try_clone()?;
    let mut reader = std::io::BufReader::new(&stream);
    // The MCP transport is agent-facing: the fixed loopback-Origin policy
    // and the agent CORS policy (the browser-facing knobs are the Query API
    // surface's). The admission pipeline owns the gate, both absolute
    // deadlines and the malformed/deadline fold (ADR-0451); this transport
    // only renders the typed outcome.
    let admitted = read_admitted_request(
        &mut reader,
        server.auth(),
        &OriginAllowlist::LoopbackOnly,
        Cors::Agent,
        |_| false,
        limits,
        |remaining| writer.set_read_timeout(Some(remaining)),
    )
    .map_err(|error| ServeError::Io(error.into_io()))?;
    let (response, drain): (HttpResponse, DrainPlan) = match admitted {
        AdmissionOutcome::Closed => return Ok(()),
        // The Agent CORS policy does not produce preflights today; answering
        // one with the transport's POST-only refusal degrades safely if the
        // policy ever widens.
        AdmissionOutcome::Preflight { .. } => (post_only_response(), DrainPlan::None),
        AdmissionOutcome::BadRequest { message, drain } => {
            (HttpResponse::error(400, message), drain)
        }
        AdmissionOutcome::Rejected { rejection, drain } => {
            let wire = rejection.into_wire();
            let mut response = HttpResponse::error(wire.status, wire.message);
            response.headers = wire.extra_headers;
            (response, drain)
        }
        AdmissionOutcome::Request { request, .. } => {
            // The body is UTF-8 for the JSON-RPC layer. A non-UTF-8 body has
            // already been consumed, so there is nothing to drain.
            let head = request.head;
            match String::from_utf8(request.body) {
                Ok(body) => (answer(server, &head.method, &body), DrainPlan::None),
                Err(_) => (
                    HttpResponse::error(400, "body is not UTF-8"),
                    DrainPlan::None,
                ),
            }
        }
    };
    // `Content-Type`/`Content-Length`/the reason phrase are the writer's job;
    // everything else (`WWW-Authenticate`, `Allow`) rides as extra headers.
    write_response(
        &mut writer,
        response.status,
        "application/json",
        response.body.as_bytes(),
        &response.headers,
    )?;
    if !matches!(drain, DrainPlan::None) {
        // The transport owns the socket, so it arms the short drain timeout
        // (the same shape as the HTTP surface's drain closure).
        stream.set_read_timeout(Some(REJECT_DRAIN_TIMEOUT))?;
        drain_reader(&mut reader, drain, limits.max_body_bytes());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::McpServerConfig;
    use relate::Database;
    use relate_auth::AuthConfig;

    fn server_with_auth(auth: AuthConfig) -> McpServer {
        let db = Database::open(":memory:").unwrap();
        McpServer::new(
            db,
            McpServerConfig {
                auth,
                ..McpServerConfig::default()
            },
        )
    }

    const PING: &str = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
    // base64("neo4j:s3cret") — the RFC 7617 credential.
    const GOOD: &str = "Basic bmVvNGo6czNjcmV0";
    const BAD: &str = "Basic bmVvNGo6d3Jvbmc=";

    #[test]
    fn unauthenticated_post_gets_401_with_challenge() {
        let server = server_with_auth(AuthConfig::basic("neo4j", "s3cret"));
        let response = handle_http(&server, "POST", None, PING);
        assert_eq!(response.status, 401);
        assert!(response
            .headers
            .iter()
            .any(|(n, v)| n == "WWW-Authenticate" && v.contains("Basic realm=")));
        // A wrong credential is the same story.
        let response = handle_http(&server, "POST", Some(BAD), PING);
        assert_eq!(response.status, 401);
    }

    #[test]
    fn authenticated_post_roundtrips() {
        let server = server_with_auth(AuthConfig::basic("neo4j", "s3cret"));
        let response = handle_http(&server, "POST", Some(GOOD), PING);
        assert_eq!(response.status, 200);
        assert!(response.body.contains("\"result\":{}"));
    }

    #[test]
    fn auth_none_needs_no_header() {
        let server = server_with_auth(AuthConfig::None);
        let response = handle_http(&server, "POST", None, PING);
        assert_eq!(response.status, 200);
    }

    #[test]
    fn notifications_get_202_with_empty_body() {
        let server = server_with_auth(AuthConfig::None);
        let response = handle_http(
            &server,
            "POST",
            None,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        );
        assert_eq!(response.status, 202);
        assert!(response.body.is_empty());
    }

    #[test]
    fn non_post_gets_405() {
        let server = server_with_auth(AuthConfig::None);
        let response = handle_http(&server, "GET", None, "");
        assert_eq!(response.status, 405);
    }
}
