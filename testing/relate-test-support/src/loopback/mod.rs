//! Loopback HTTP fixtures in two directions: a scripted raw-HTTP **server**
//! on `127.0.0.1` plus the [`LoadOpts`] that point a pull at it (the
//! `relate-load` tests, the CLI e2e pull tests, the `pull_rca` loader
//! bench, ADR-0080), and the raw **client** helpers ([`exchange`],
//! [`exchange_bytes`], [`request`], [`post`]) the server-surface tests (Query
//! API in `relate-http`, the MCP streamable-HTTP transport, the CLI's
//! `e2e_serve`) drive against their own listeners. Every request head a test
//! sends is assembled here: [`post`] and its siblings ([`post_with_headers`],
//! [`post_with_content_type`], [`post_head_only`], [`post_with_version`]) and
//! [`request`] are thin cases over one assembler, so no test module re-spells
//! `Host`, `content-type` or `content-length`.
//!
//! The response side is parsed here too, so a wire test never carries its own
//! scanner: [`split_response`] cuts one raw response at the head/body
//! separator (keeping a compressed body's bytes intact), [`header_value`] and
//! [`header_values`] are the one case-insensitive header scan, [`dechunk`]
//! reassembles RFC 9112 chunk frames (and [`max_frame_size`] reports the
//! largest frame's payload for the framing-bound tests), and [`gunzip`]
//! inflates a gzip body.
//!
//! The [`spawn_*_capturing`](spawn_scripted_responses_capturing) variants
//! record the request head of every connection they serve — the server
//! thread's join value — so tests can pin wire-level request details
//! (ADR-0181: the pull path's typed `Accept` header); [`header_values`]
//! pulls one header's values out of a captured head.
//!
//! [`spawn_capture_server`] is the run-until-stopped variant: it answers
//! every request with one fixed response and records each request body
//! until the returned [`CaptureSink`] is dropped, so tests whose request
//! count is not known up front (a background worker flushing events) can
//! poll the recordings instead of joining a scripted server.
//!
//! std::net only — the fixture serves fixed bodies, so a real HTTP stack
//! would be dead weight.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

#[cfg(feature = "loopback")]
use relate_load::{Credentials, LoadOpts};

mod response;
mod server;

pub use response::{dechunk, gunzip, header_value, header_values, max_frame_size, split_response};
pub use server::{
    spawn_capture_server, spawn_capture_server_with_delay, spawn_scripted_responses,
    spawn_scripted_responses_capturing, spawn_scripted_responses_capturing_requests,
    spawn_single_response, spawn_single_response_bytes_capturing, spawn_single_response_capturing,
    CaptureSink, CapturedRequests, CapturingFixture,
};

/// The raw loopback exchange primitive: connect, send `request`, read the
/// whole response to EOF (the server surfaces answer `Connection: close`),
/// and return the status code plus the untouched raw response bytes (head
/// included). Byte-level so a compressed body — which is not UTF-8 — stays
/// intact; [`exchange_raw`] is its UTF-8 form.
pub fn exchange_bytes(port: u16, request: &[u8]) -> (u16, Vec<u8>) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream.write_all(request).expect("write request");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read response");
    let status = response::status_code(&raw);
    (status, raw)
}

/// The raw loopback exchange for text responses: [`exchange_bytes`] with the
/// whole response (head included) decoded as UTF-8 — the form tests that
/// assert on response *headers* use. A non-UTF-8 (compressed) body must go
/// through [`exchange_bytes`] instead.
pub fn exchange_raw(port: u16, request: &[u8]) -> (u16, String) {
    let (status, raw) = exchange_bytes(port, request);
    let raw = String::from_utf8(raw).expect("the raw response is UTF-8");
    (status, raw)
}

/// A raw loopback HTTP exchange for server-surface tests (the Query API
/// and MCP surfaces), split into status code and body (the head is
/// dropped). Built on [`exchange_raw`] and [`split_response`].
pub fn exchange(port: u16, request: &[u8]) -> (u16, String) {
    let (status, raw) = exchange_bytes(port, request);
    let (_, body) = split_response(&raw);
    (
        status,
        String::from_utf8(body.to_vec()).expect("the response body is UTF-8"),
    )
}

/// The `Host` every loopback request names unless a test varies it — the
/// discovery document echoes the request's Host back (`query_api`), so one
/// test does vary it.
pub const LOOPBACK_HOST: &str = "localhost";

/// The request media type every loopback POST defaults to.
pub const JSON_CONTENT_TYPE: &str = "application/json";

/// The request-line version every builder sends; the HTTP/1.0 refusal test
/// is the one variation ([`post_with_version`]).
const HTTP_1_1: &str = "HTTP/1.1";

/// The one request-head assembler: request line, `Host`, `headers` in wire
/// order, and — when `content_length` is `Some` — the declared
/// `content-length` followed by `body`. [`request`] and [`post_wire`] are its
/// only callers, so a wire-level change to a test request is one edit.
fn assemble<'a>(
    version: &str,
    method: &str,
    path: &str,
    host: &str,
    headers: impl IntoIterator<Item = (&'a str, &'a str)>,
    body: PostBody<'_>,
) -> String {
    let mut request = format!("{method} {path} {version}\r\nhost: {host}\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    let (declared, bytes) = match body {
        PostBody::Absent => (None, ""),
        PostBody::Sent(bytes) => (Some(bytes.len()), bytes),
        PostBody::Withheld(length) => (Some(length), ""),
    };
    if let Some(length) = declared {
        request.push_str(&format!("content-length: {length}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(bytes);
    request
}

/// Build a bodyless request (GET, OPTIONS, …) for `path` at `host`: the
/// request line, `Host`, and `headers` in wire order, with no body and no
/// `content-length`.
pub fn request(method: &str, path: &str, host: &str, headers: &[(&str, &str)]) -> String {
    assemble(
        HTTP_1_1,
        method,
        path,
        host,
        headers.iter().copied(),
        PostBody::Absent,
    )
}

/// One test request's head-to-body relation: the bytes written after the
/// head and the `content-length` the head declares for them. The normal case
/// declares exactly the bytes it sends ([`PostBody::Sent`]); the body-cap
/// fixtures declare a length they never send ([`PostBody::Withheld`]); a
/// bodyless request ([`PostBody::Absent`]) declares nothing.
enum PostBody<'a> {
    /// No body and no `content-length` (GET, OPTIONS, …).
    Absent,
    /// The normal case: the body is the given bytes and the head declares
    /// their length.
    Sent(&'a str),
    /// A head-only request: the head declares the length and no body bytes
    /// follow it.
    Withheld(usize),
}

/// The one POST assembler: request line at `version`, `Host`, the request
/// `content_type`, `extra_headers` in wire order, the declared
/// `content-length`, and the body. Every POST builder below delegates here,
/// so a wire-level change to a test POST is one edit.
fn post_wire(
    version: &str,
    path: &str,
    content_type: &str,
    extra_headers: &[(&str, &str)],
    body: PostBody<'_>,
) -> String {
    assemble(
        version,
        "POST",
        path,
        LOOPBACK_HOST,
        std::iter::once(("content-type", content_type)).chain(extra_headers.iter().copied()),
        body,
    )
}

/// Build a POST request for `path` against a JSON surface: JSON content
/// type, an optional pre-encoded `authorization` header value, and the
/// body.
pub fn post(path: &str, body: &str, auth: Option<&str>) -> String {
    let auth_header: &[(&str, &str)] = match auth {
        Some(auth) => &[("authorization", auth)],
        None => &[],
    };
    post_with_headers(path, body, auth_header)
}

/// [`post`] with `extra_headers` appended after the JSON content type, in
/// wire order — the caller-chosen request headers (`origin`,
/// `accept-encoding`, …) the compression and browser-guard tests vary.
pub fn post_with_headers(path: &str, body: &str, extra_headers: &[(&str, &str)]) -> String {
    post_with_content_type(path, JSON_CONTENT_TYPE, body, extra_headers)
}

/// A POST whose request content type is `content_type` rather than JSON,
/// with `extra_headers` in wire order: the media-type negotiation fixtures,
/// where the request media type is the behavior under test — and the one
/// content-type-carrying POST builder [`post`] and [`post_with_headers`]
/// delegate to.
pub fn post_with_content_type(
    path: &str,
    content_type: &str,
    body: &str,
    extra_headers: &[(&str, &str)],
) -> String {
    post_wire(
        HTTP_1_1,
        path,
        content_type,
        extra_headers,
        PostBody::Sent(body),
    )
}

/// A POST that sends only its head: it declares `content_length` and writes
/// no body bytes — the fixture for the reader's body cap and the
/// auth-before-body ordering, where the declared length deliberately exceeds
/// both the bytes on the wire and the configured cap.
pub fn post_head_only(path: &str, content_length: usize) -> String {
    post_wire(
        HTTP_1_1,
        path,
        JSON_CONTENT_TYPE,
        &[],
        PostBody::Withheld(content_length),
    )
}

/// [`post`] whose request line names `version` rather than HTTP/1.1 — the
/// HTTP/1.0 refusal fixture (RFC 9112 §2.3: the request version is part of
/// what the surface answers).
pub fn post_with_version(version: &str, path: &str, body: &str) -> String {
    post_wire(version, path, JSON_CONTENT_TYPE, &[], PostBody::Sent(body))
}

/// [`LoadOpts`] for `query` against a loopback fixture on `port`: plain
/// `http://127.0.0.1` (loopback needs no plaintext opt-in), fixed test
/// credentials, no params. The loader bridge, behind the `loopback` feature.
#[cfg(feature = "loopback")]
pub fn loopback_opts(port: u16, query: &str) -> LoadOpts {
    LoadOpts {
        credentials: Credentials {
            uri: format!("http://127.0.0.1:{port}"),
            username: "neo4j".into(),
            password: "pw".into(),
            database: "neo4j".into(),
        },
        query: query.into(),
        params: serde_json::Map::new(),
        allow_plaintext: false,
    }
}

/// Wait for the server to drop a connection whose request phase the caller
/// left incomplete (the slowloris/trickle probe), returning how long the
/// drop took. `settle` bounds the wait: a connection still open then panics,
/// so a server that never applies its deadline fails the test.
///
/// The caller has already written its surface-specific incomplete prefix
/// (and, for the trickling form, calls [`assert_dropped_while_trickling`]
/// instead); this helper owns the read side only. Both the Query API and MCP
/// admission tests call it so the probe's read loop (timeout handling,
/// reset-vs-EOF, settle bound) is stated once; the callers keep their own
/// deadline-window assertions and messages.
pub fn assert_dropped_by_deadline(stream: &mut TcpStream, settle: Duration) -> Duration {
    stream
        .set_read_timeout(Some(Duration::from_millis(50)))
        .expect("arm the probe's read timeout");
    let started = Instant::now();
    let mut buf = [0u8; 64];
    loop {
        match stream.read(&mut buf) {
            // EOF: the server closed the connection.
            Ok(0) => return started.elapsed(),
            Ok(_) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                if started.elapsed() > settle {
                    panic!(
                        "the connection was still open after {settle:?}; the admission deadline did not drop it"
                    );
                }
            }
            // A reset also means the server dropped us.
            Err(_) => return started.elapsed(),
        }
    }
}

/// The trickle gap: each byte arrives well inside any surface's per-read
/// timeout, so only an absolute deadline can drop the connection.
const TRICKLE_GAP: Duration = Duration::from_millis(50);
/// The trickle length: 1.5 s of activity, enough to outlive the short test
/// deadlines while staying inside the default settle bound.
const TRICKLE_BYTES: usize = 30;

/// Drive the slowloris probe end to end: `stream` has already written its
/// surface-specific incomplete request prefix; this spawns a writer clone
/// that trickles one byte every [`TRICKLE_GAP`], waits for the server to
/// drop the connection ([`assert_dropped_by_deadline`]), joins the writer,
/// and returns the elapsed drop time. The caller keeps its own deadline
/// window assertions and messages.
pub fn assert_dropped_while_trickling(stream: &mut TcpStream, settle: Duration) -> Duration {
    let mut writer = stream.try_clone().expect("clone the trickle writer");
    let trickle = std::thread::spawn(move || {
        for _ in 0..TRICKLE_BYTES {
            if writer.write_all(b"a").is_err() {
                return;
            }
            std::thread::sleep(TRICKLE_GAP);
        }
    });
    let elapsed = assert_dropped_by_deadline(stream, settle);
    trickle.join().expect("the trickle writer panicked");
    elapsed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_builds_a_json_post_with_optional_auth() {
        let request = post("/db/neo4j/query/v2", "{}", Some("Basic QQ=="));
        assert_eq!(
            request,
            "POST /db/neo4j/query/v2 HTTP/1.1\r\nhost: localhost\r\ncontent-type: application/json\r\nauthorization: Basic QQ==\r\ncontent-length: 2\r\n\r\n{}"
        );
        assert!(!post("/x", "{}", None).contains("authorization"));
    }

    #[test]
    fn post_with_headers_appends_caller_headers_before_the_length() {
        let request = post_with_headers(
            "/db/neo4j/query/v2",
            "{}",
            &[
                ("origin", "https://browser.neo4j.io"),
                ("accept-encoding", "gzip"),
            ],
        );
        assert_eq!(
            request,
            "POST /db/neo4j/query/v2 HTTP/1.1\r\nhost: localhost\r\ncontent-type: application/json\r\norigin: https://browser.neo4j.io\r\naccept-encoding: gzip\r\ncontent-length: 2\r\n\r\n{}"
        );
    }

    #[test]
    fn post_with_content_type_replaces_the_json_media_type() {
        let request = post_with_content_type(
            "/db/neo4j/query/v2",
            "application/vnd.neo4j.query",
            "{}",
            &[("accept", "application/json")],
        );
        assert_eq!(
            request,
            "POST /db/neo4j/query/v2 HTTP/1.1\r\nhost: localhost\r\ncontent-type: application/vnd.neo4j.query\r\naccept: application/json\r\ncontent-length: 2\r\n\r\n{}"
        );
        assert_eq!(request.matches("content-type:").count(), 1);
    }

    #[test]
    fn post_head_only_declares_a_length_it_does_not_send() {
        let request = post_head_only("/db/neo4j/query/v2", 17);
        assert_eq!(
            request,
            "POST /db/neo4j/query/v2 HTTP/1.1\r\nhost: localhost\r\ncontent-type: application/json\r\ncontent-length: 17\r\n\r\n"
        );
    }

    #[test]
    fn request_builds_a_bodyless_request_at_the_given_host() {
        let get = request(
            "GET",
            "/",
            "127.0.0.1:7474",
            &[("origin", "https://browser.example")],
        );
        assert_eq!(
            get,
            "GET / HTTP/1.1\r\nhost: 127.0.0.1:7474\r\norigin: https://browser.example\r\n\r\n"
        );
        assert_eq!(
            request("OPTIONS", "/db/neo4j/query/v2", LOOPBACK_HOST, &[]),
            "OPTIONS /db/neo4j/query/v2 HTTP/1.1\r\nhost: localhost\r\n\r\n"
        );
    }

    #[cfg(feature = "loopback")]
    #[test]
    fn loopback_opts_point_at_the_fixture_port() {
        let opts = loopback_opts(7474, "MATCH (n) RETURN n");
        assert_eq!(opts.credentials.uri, "http://127.0.0.1:7474");
        assert_eq!(opts.query, "MATCH (n) RETURN n");
        assert!(!opts.allow_plaintext);
    }
}
