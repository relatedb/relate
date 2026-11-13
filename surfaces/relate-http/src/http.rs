//! Minimal HTTP/1.1: request-head parsing via `httparse` out of a
//! `MAX_HEADER_BYTES`-bounded buffer (the buffer is the cap; ADR-0234), a
//! `Content-Length`-capped body reader, and a JSON response writer.
//! Deliberately small per ADR-0232's ethos — one request per connection, no
//! keep-alive, request bodies read by `Content-Length` only, no TLS (the TLS
//! policy stays with the listener, as in ADR-0109). Every response head is
//! written by `write_head` with one `Framing`: a one-shot body under
//! `Content-Length` by [`write_response`], or — at or above
//! `stream::STREAM_THRESHOLD_BYTES` — `Transfer-Encoding: chunked` followed by the
//! RFC 9112 frames `crate::stream` writes (ADR-0386). The head writer derives
//! the reason phrase from the status (`reason`), so no caller carries a
//! second spelling of the same fact.
//!
//! The HTTP surfaces answer HTTP/1.1 only: [`RequestHead`] records the
//! parsed request-line version, and a non-HTTP/1.1 request is refused as a
//! parsed 400 rejection carrying the head (ADR-0386 §8) — so
//! `Transfer-Encoding: chunked`, an HTTP/1.1 framing (RFC 9112 §7.1), never
//! reaches a peer that does not speak it.
//!
//! [`write_response`] stays encoding-agnostic: the Query API's optional gzip
//! response encoding (ADR-0385) is negotiated one layer up, at that
//! surface's reply seam (`crate::content_encoding`), so this writer and the
//! MCP transport's direct use of it keep their byte-identical behavior.

use std::io::{self, Write};

// The bounded pre-body read pipeline and drain policy (ADR-0451) live in
// their own sibling module (`admission`), re-exported here so the surfaces
// keep one import path.
pub use crate::admission::{
    drain_reader, read_admitted_request, AdmissionError, AdmissionOutcome, Cors, DrainPlan,
    ReadLimits, Rejection, RejectionWire, FAILED_AUTH_DELAY, REJECT_DRAIN_TIMEOUT,
};

// The accept loop and admission defaults (ADR-0238, ADR-0451) live in
// `relate-serve`; each surface imports them there directly, so this
// module re-exports none of them.

// The browser-origin policy (ADR-0266) is its own module (`origin`):
// re-exported here so this crate's consumers keep one import path — the
// same one-path policy as the accept loop above.
pub use crate::origin::{
    browser_guard, GuardRejection, OriginAllowlist, OriginFlagValue, OriginMatch,
};

/// The HTTP version a request line declares (RFC 9112 §2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HttpVersion {
    /// `HTTP/1.0` (RFC 1945): parsed, then refused — see [`read_admitted_request`].
    Http10,
    /// `HTTP/1.1` — the one version this surface answers.
    Http11,
}

/// The parsed request head: method and path from the request line and the
/// headers (lowercased names), with the body not yet read. Splitting the
/// read into head + body phases lets the surfaces reject a request (browser
/// guard, auth challenge) before a byte of the body is buffered.
pub struct RequestHead {
    /// The request method, verbatim.
    pub method: String,
    /// The request path, verbatim.
    pub path: String,
    /// The request line's HTTP version.
    pub(crate) version: HttpVersion,
    /// The headers, names lowercased.
    pub headers: Vec<(String, String)>,
    /// Body bytes the head read already pulled past the head/body boundary
    /// (the read buffer can straddle it); the admission reader serves them
    /// before touching the reader again.
    pub(crate) leftover: Vec<u8>,
}

impl RequestHead {
    /// The first header value for `name` (case-insensitive per RFC 9110).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// A header value's parameter-free token: the text before the first
    /// `;` parameter, trimmed (RFC 9110 field parameters). The one strip
    /// every consumer — the content-type guard, the Accept media-type
    /// parse, the Accept-Encoding weight parse — consults.
    pub(crate) fn media_token(value: &str) -> &str {
        value[..value.find(';').unwrap_or(value.len())].trim()
    }

    /// All values of a comma-separated list header joined with `, ` (RFC
    /// 9110 §5.2: a field split across lines is one list). The form a list
    /// header's consumer — the `Accept-Encoding` negotiation — reads; a
    /// client that splits `gzip` across two lines negotiates gzip, not
    /// identity.
    pub(crate) fn header_list(&self, name: &str) -> Option<String> {
        let mut values = self
            .headers
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str());
        let first = values.next()?;
        Some(values.fold(first.to_string(), |mut joined, value| {
            joined.push_str(", ");
            joined.push_str(value);
            joined
        }))
    }

    /// Pair the head with its body into a full [`Request`].
    pub fn into_request(self, body: Vec<u8>) -> Request {
        Request { head: self, body }
    }
}

/// The plain-JSON media type: the response form every client understands
/// and the request dialect's plain spelling — one constant beside the
/// vendor family, consulted at every parse and emit site.
pub(crate) const APPLICATION_JSON: &str = "application/json";

/// The Query API v2 media type family's root: responses answer the bare
/// `application/vnd.neo4j.query`, requests carry a versioned member
/// (`.v1.2`, `.v1.0+jsonl`) — the bare spelling is a fail-closed 400 as a
/// request Content-Type (ADR-0425).
pub(crate) const VND_NEO4J_QUERY: &str = "application/vnd.neo4j.query";

/// A media-type header value normalized the one way every consumer needs
/// it: the parameter-free token, lowercased (the case-insensitive compare
/// and the family prefix both read it) — beside the predicate it feeds.
pub(crate) fn lowercased_media_type(value: &str) -> String {
    RequestHead::media_token(value).to_ascii_lowercase()
}

/// Whether a (lowercased) media-type token is a **versioned** member of
/// the `application/vnd.neo4j.query` family (`.v1.2`, `.v1.0+jsonl`, …) —
/// the request spelling real servers accept (ADR-0425). The one
/// classifier both consumers consult: the request dialect's family arm
/// and the Accept negotiation (which additionally matches the bare root —
/// responses answer the bare type, requests never carry it). It lives
/// beside [`RequestHead::media_token`], the strip every one of those
/// consumers already imports, so the media-type vocabulary has one home
/// neither the request nor the response module owns.
pub(crate) fn is_versioned_query_family(media_type: &str) -> bool {
    media_type
        .strip_prefix(VND_NEO4J_QUERY)
        .is_some_and(|rest| rest.starts_with(".v"))
}

/// One fully-read request: the [`RequestHead`] plus the body bytes
/// (`Content-Length` only). The one request type — it dereferences to the
/// head, so `request.method` / `request.path` / `request.header(..)` read
/// the head fields directly.
pub struct Request {
    /// The request line and headers.
    pub head: RequestHead,
    /// The body bytes.
    pub body: Vec<u8>,
}

impl std::ops::Deref for Request {
    type Target = RequestHead;

    fn deref(&self) -> &RequestHead {
        &self.head
    }
}

impl std::ops::DerefMut for Request {
    fn deref_mut(&mut self) -> &mut RequestHead {
        &mut self.head
    }
}

/// The header section never exceeds this; the read buffer below IS the
/// cap (ADR-0234): a peer that has not terminated its headers within it is
/// cut off as malformed, and no line-level accumulation can grow past it.
pub(crate) const MAX_HEADER_BYTES: usize = 64 * 1024;

/// The most headers a request may carry; httparse parses into a fixed
/// header slice, and overflowing it is a parse error (the same 400).
pub(crate) const MAX_HEADERS: usize = 128;

/// How a response body is delimited (RFC 9112 §6/§7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Framing {
    /// The body is exactly these bytes, written under `Content-Length`.
    Length(usize),
    /// The body follows the head as RFC 9112 §7.1 chunk frames and carries
    /// no `Content-Length`; the zero-size last chunk ends it (ADR-0386 §2).
    Chunked,
}

/// Write one response head (`Connection: close`): the one head writer every
/// response of this crate goes through — the one-shot [`write_response`] and
/// the Query API's chunked path alike — so the status line, the reason
/// phrase (`reason`, derived from `status`), the framing header and the
/// extra-header loop are spelled once.
pub(crate) fn write_head(
    stream: &mut impl Write,
    status: u16,
    content_type: &str,
    framing: Framing,
    extra_headers: &[(String, String)],
) -> io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {status} {}\r\ncontent-type: {content_type}\r\n",
        reason(status)
    );
    match framing {
        Framing::Length(length) => head.push_str(&format!("content-length: {length}\r\n")),
        Framing::Chunked => head.push_str("transfer-encoding: chunked\r\n"),
    }
    head.push_str("connection: close\r\n");
    for (name, value) in extra_headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())
}

/// Write one response with an already-encoded body and close
/// (`Connection: close`). JSON bodies serialize at the call site
/// (`serde_json::to_vec`); surfaces whose payload is a pre-encoded string
/// (the MCP transport's JSON-RPC envelopes) use this directly. The status
/// line's phrase is the head writer's (`reason`), never a caller's.
///
/// This writer keeps its one-shot `Content-Length` body (ADR-0386 §7): a
/// body at or above `STREAM_THRESHOLD_BYTES` is framed by the Query API's
/// sibling path instead — `write_head` with its chunked `Framing` plus the
/// chunk frames `crate::stream` writes.
pub fn write_response(
    stream: &mut impl Write,
    status: u16,
    content_type: &str,
    body: &[u8],
    extra_headers: &[(String, String)],
) -> io::Result<()> {
    write_head(
        stream,
        status,
        content_type,
        Framing::Length(body.len()),
        extra_headers,
    )?;
    stream.write_all(body)?;
    stream.flush()
}

/// The reason phrase for every status these HTTP surfaces emit: the Query API
/// routes and the MCP transport (ADR-0232/ADR-0237), with the explicit
/// `500` an internal engine error answers (`query_api::run_error`). A status
/// neither surface emits still gets a phrase instead of an empty reason —
/// the same server-error phrase — so the status line is always well-formed.
pub(crate) fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        406 => "Not Acceptable",
        415 => "Unsupported Media Type",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        507 => "Insufficient Storage",
        _ => "Internal Server Error",
    }
}

// The head, gate and writer tests live in their own file (the same `#[path]`
// pattern as `server_tests.rs`), reached through this module. Crate-visible
// so every in-crate test builds its request heads from the one fixture
// (`tests::head_at`) instead of hand-building `RequestHead`.
#[cfg(test)]
#[path = "http_tests.rs"]
pub(crate) mod tests;
