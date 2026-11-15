//! Query API v2 response-body vocabulary (ADR-0158): the size caps, the
//! buffered whole-body parser, the error truncation helper, and the
//! incremental `data.values` row scanner the streaming pull consumes.
//! `http.rs` owns URL validation and the ureq transport; everything that
//! interprets a response body lives here.
//!
//! A whole-database pull's Query API v2 response is chunked and progressive
//! (the server never precomputes it), so the scanner consumes rows off the
//! body as they arrive. Only one row is held at a time. Before `values`
//! opens, the (small, bounded) head is buffered: a body that never produces
//! a values array is an error response, and it gets the same treatment as
//! the buffered path's [`parse_v2_response`].

use std::io::Read;

use relate_formatter::strip_unsafe_output_chars;
use serde_json::Value as Json;

use crate::LoadError;

/// Whole-body cap for the buffered path, and the per-phase caps of the
/// streaming scanner (head, single row, trailing metadata).
pub(crate) const MAX_RESPONSE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ERROR_BODY: usize = 4 * 1024;

/// Truncate a response excerpt for an error message and strip characters
/// that are unsafe to print (control chars, bidi overrides).
pub(crate) fn truncate_for_error(s: &str) -> String {
    let truncated = if s.len() <= MAX_ERROR_BODY {
        s.to_string()
    } else {
        let mut end = MAX_ERROR_BODY;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}… (truncated)", &s[..end])
    };
    strip_unsafe_output_chars(&truncated)
}

/// Buffered whole-body parse: a 2xx body must be JSON, carry no query
/// errors, and hold a `data.values` array.
pub(crate) fn parse_v2_response(text: &str) -> Result<Json, LoadError> {
    let json: Json = serde_json::from_str(text).map_err(|e| {
        LoadError::Body(format!(
            "non-JSON 2xx response body: {} (parse error: {e})",
            truncate_for_error(text)
        ))
    })?;
    if let Some(errors) = json.get("errors").and_then(|e| e.as_array()) {
        if !errors.is_empty() {
            return Err(LoadError::Body(format!(
                "Neo4j query errors: {}",
                truncate_for_error(&format!("{errors:?}"))
            )));
        }
    }
    let data_values = json.get("data").and_then(|d| d.get("values"));
    if !matches!(data_values, Some(Json::Array(_))) {
        return Err(LoadError::Body(format!(
            "v2 response missing data.values on success: {}",
            truncate_for_error(&json.to_string())
        )));
    }
    Ok(json)
}

/// Feed the body to a [`RowScanner`], handing each completed row to
/// `on_rows`; at EOF, verify the body closed properly and check the bounded
/// trailing metadata for query errors.
pub(crate) fn stream_v2_rows(
    reader: impl Read,
    url: &str,
    on_rows: &mut dyn FnMut(&[Json]) -> Result<(), LoadError>,
) -> Result<(), LoadError> {
    let mut scanner = RowScanner::default();
    let mut chunk = [0u8; 64 * 1024];
    let mut reader = reader;
    loop {
        let n = reader
            .read(&mut chunk)
            .map_err(|e| mid_statement_read_error(url, e))?;
        if n == 0 {
            break;
        }
        // One callback per chunk; each element is a complete row, the
        // same shape the buffered path hands callers.
        let rows = scanner.feed(&chunk[..n])?;
        if !rows.is_empty() {
            on_rows(&rows)?;
        }
    }
    scanner.finish(url)
}

/// The error for an io failure while reading the response body from `url`
/// mid-statement.
///
/// A connection-reset-class error (`ConnectionReset`, `BrokenPipe`,
/// `UnexpectedEof`; e.g. "Peer disconnected") usually means the server
/// closed the response stream. Neo4j's Query API v2 closes a response whose
/// client stops reading: one measured stall ran roughly 400-500 s before the
/// close, but it has been seen much sooner. A stalled local read loop — e.g.
/// a long local checkpoint or slow ingest holding up the commit — can cause
/// that, so the message names it as one possible cause, not a diagnosis.
/// Only that class gets the hint — any other io error (`TimedOut`, TLS,
/// transport) keeps the plain text, so the hint cannot mislead on non-Neo4j
/// servers.
fn mid_statement_read_error(url: &str, e: std::io::Error) -> LoadError {
    let mut msg = format!("reading response body from {url} failed mid-statement: {e}");
    if matches!(
        e.kind(),
        std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::UnexpectedEof
    ) {
        msg.push_str(
            "; the server closed the response stream — Neo4j closes a response \
             whose client stops reading (measured after up to roughly 400-500 s \
             of read stall, and it can happen much sooner), so a stalled local \
             read loop, for example a long local checkpoint or slow ingest, \
             may be the cause",
        );
    }
    LoadError::Io(msg)
}

/// The JSON string/escape lexer the row scanner's per-phase byte handling
/// steps bytes through.
#[derive(Default)]
struct StrScan {
    in_string: bool,
    escaped: bool,
}

/// One byte's structural class from [`StrScan::step`].
enum Step {
    /// A byte outside any string literal.
    Structural(u8),
    /// The quote opening a string.
    Open,
    /// A string-content byte (escape pair included).
    Content,
    /// The quote closing a string.
    Close,
}

impl StrScan {
    fn step(&mut self, b: u8) -> Step {
        if self.in_string {
            if self.escaped {
                self.escaped = false;
            } else if b == b'\\' {
                self.escaped = true;
                return Step::Content;
            } else if b == b'"' {
                self.in_string = false;
                return Step::Close;
            }
            return Step::Content;
        }
        if b == b'"' {
            self.in_string = true;
            return Step::Open;
        }
        Step::Structural(b)
    }
}

/// The scanner's phase: the `hunting`/`in_row`/`finished` bool lattice of
/// the byte scan, made explicit. `in_string`/`escaped` stay orthogonal
/// lexer state ([`StrScan`]).
#[derive(Default)]
enum Phase {
    /// Before the `values` array opens: the head is buffered (bounded) and
    /// string tokens are hunted for the `values` key.
    #[default]
    Head,
    /// Inside `data.values`: one balanced row accumulates at a time
    /// (`depth` 0 = between rows).
    Rows,
    /// After the values array closed: the rest of the body is bounded
    /// trailing metadata.
    Tail,
}

/// Byte-level scanner over the Query API v2 body (ADR-0158). First it hunts
/// the `values` key (string-aware, so row contents cannot fake it), then it
/// accumulates one balanced row at a time — every `data.values` row is an
/// array — and hands each completed row out as parsed JSON.
#[derive(Default)]
struct RowScanner {
    phase: Phase,
    scan: StrScan,
    /// The string token currently being read (Head phase).
    token: Vec<u8>,
    /// Set when a closed string token spelled `values` (Head phase).
    expect_values: bool,
    /// Bracket depth inside the current row (Rows phase; 0 = between rows).
    depth: i32,
    /// The row accumulating between depth 1 and its close (Rows phase).
    row: Vec<u8>,
    /// Everything before the values array opened — bounded by the response
    /// cap; an errors-only body never opens `values`, and this head is its
    /// (full) body for error handling.
    head: Vec<u8>,
    /// Bytes after the values array's closing `]` (small; checked for
    /// trailing query errors at EOF).
    tail: Vec<u8>,
}

impl RowScanner {
    /// Completed rows from one chunk, each the row's `Json::Array` — the
    /// shape every boundary in this module speaks.
    fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Json>, LoadError> {
        let mut rows = Vec::new();
        for (i, &b) in bytes.iter().enumerate() {
            match self.phase {
                Phase::Head => self.feed_head(b)?,
                Phase::Rows => {
                    if let Some(row) = self.feed_row(b)? {
                        rows.push(row);
                    }
                }
                Phase::Tail => {
                    self.tail.extend_from_slice(&bytes[i..]);
                    if self.tail.len() as u64 > MAX_RESPONSE_BYTES {
                        return Err(LoadError::Io(format!(
                            "response body trailing metadata exceeded {MAX_RESPONSE_BYTES} bytes"
                        )));
                    }
                    break;
                }
            }
        }
        Ok(rows)
    }

    /// Head phase: buffer the (bounded) head and hunt the `"values"` key;
    /// noise between tokens disarms the detector.
    fn feed_head(&mut self, b: u8) -> Result<(), LoadError> {
        self.head.push(b);
        if self.head.len() as u64 > MAX_RESPONSE_BYTES {
            return Err(LoadError::Io(format!(
                "response body exceeded {MAX_RESPONSE_BYTES} bytes"
            )));
        }
        match self.scan.step(b) {
            Step::Open => self.token.clear(),
            Step::Content => self.token.push(b),
            Step::Close => {
                self.expect_values = std::mem::take(&mut self.token) == b"values";
            }
            Step::Structural(b) => match b {
                b'[' => {
                    if self.expect_values {
                        self.phase = Phase::Rows;
                    }
                    self.expect_values = false;
                }
                b'{' | b']' | b'}' => self.expect_values = false,
                _ => {
                    if !b.is_ascii_whitespace() && b != b':' && b != b',' {
                        self.expect_values = false;
                    }
                }
            },
        }
        Ok(())
    }

    /// Rows phase: accumulate one balanced row at a time; a completed row is
    /// parsed and handed out, the values array's close ends the phase.
    fn feed_row(&mut self, b: u8) -> Result<Option<Json>, LoadError> {
        // One row is held at a time, but a row itself is bounded: a row
        // bigger than the old whole-body cap is a hard error, not an
        // unbounded buffer.
        if self.row.len() as u64 > MAX_RESPONSE_BYTES {
            return Err(LoadError::Io(format!(
                "response body row exceeded {MAX_RESPONSE_BYTES} bytes"
            )));
        }
        match self.scan.step(b) {
            Step::Open | Step::Content | Step::Close => {
                if self.depth > 0 {
                    self.row.push(b);
                }
                Ok(None)
            }
            Step::Structural(b) => match b {
                b'[' | b'{' => {
                    if self.depth == 0 {
                        // A row opens.
                        self.row.clear();
                    }
                    self.row.push(b);
                    self.depth += 1;
                    Ok(None)
                }
                b']' | b'}' => {
                    if self.depth > 1 {
                        self.row.push(b);
                        self.depth -= 1;
                        return Ok(None);
                    }
                    if self.depth == 1 {
                        // The row closes.
                        self.row.push(b);
                        self.depth = 0;
                        let text = std::str::from_utf8(&self.row).map_err(|e| {
                            LoadError::Body(format!("non-UTF-8 row in response body: {e}"))
                        })?;
                        // v2 rows are arrays: serde rejects a `{`-opened
                        // span (or any non-array) inside this error.
                        let row: Vec<Json> = serde_json::from_str(text).map_err(|e| {
                            LoadError::Body(format!(
                                "unparseable row in response body: {e}: {}",
                                truncate_for_error(text)
                            ))
                        })?;
                        self.row.clear();
                        return Ok(Some(Json::Array(row)));
                    }
                    // The values array closes.
                    self.phase = Phase::Tail;
                    Ok(None)
                }
                _ => {
                    if self.depth > 0 {
                        self.row.push(b);
                    }
                    Ok(None)
                }
            },
        }
    }

    /// At EOF, the body must have produced and closed a values array:
    /// a partially accumulated row is a truncated body, and a body that
    /// never opened `values` is an error response (same contract as the
    /// buffered path).
    fn finish(&mut self, url: &str) -> Result<(), LoadError> {
        if self.depth > 0 || self.scan.in_string {
            return Err(LoadError::Body(format!(
                "truncated Query API v2 response body from {url}"
            )));
        }
        match self.phase {
            Phase::Head => {
                // `values` never opened: this is an error response (or a
                // malformed body) — the bounded head is its whole body.
                let text = String::from_utf8_lossy(&self.head);
                match parse_v2_response(&text) {
                    Err(e) => Err(e),
                    Ok(_) => Err(LoadError::Body(format!(
                        "v2 response missing data.values from {url}"
                    ))),
                }
            }
            Phase::Rows => Err(LoadError::Body(format!(
                "truncated Query API v2 response body from {url}"
            ))),
            // After the rows, the body's tail (bookmarks, trailing errors)
            // is reconstructed into the row-less response and run through
            // the one typed parser: a non-empty `errors` array fails exactly
            // as it does on the buffered path.
            Phase::Tail => {
                let mut body = Vec::with_capacity(self.head.len() + 1 + self.tail.len());
                body.extend_from_slice(&self.head);
                body.push(b']'); // the values array's closing bracket, dropped by the scan
                body.extend_from_slice(&self.tail);
                // Strict decode: the success path must be well-formed (the
                // Head-phase error path above is deliberately best-effort).
                let text = std::str::from_utf8(&body).map_err(|e| {
                    LoadError::Body(format!("non-UTF-8 response body metadata: {e}"))
                })?;
                parse_v2_response(text).map(|_| ())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(body: &str, chunk: usize) -> Result<Vec<Json>, String> {
        let mut scanner = RowScanner::default();
        let mut rows = Vec::new();
        for piece in body.as_bytes().chunks(chunk.max(1)) {
            rows.extend(scanner.feed(piece).map_err(|e| e.to_string())?);
        }
        scanner.finish("test").map_err(|e| e.to_string())?;
        Ok(rows)
    }

    #[test]
    fn rows_survive_arbitrary_chunk_boundaries() {
        let body = r#"{"data":{"fields":["e"],"values":[[{"a":1}],[{"b":"va[l]ue\"s"}],[1,2,3]]},"errors":[]}"#;
        for chunk in [1usize, 2, 3, 7, 64, 1024] {
            let rows = scan(body, chunk).unwrap();
            assert_eq!(rows.len(), 3, "chunk {chunk}: {rows:?}");
            assert_eq!(rows[0][0]["a"], serde_json::json!(1));
            assert_eq!(rows[1][0]["b"], serde_json::json!("va[l]ue\"s"));
            assert_eq!(rows[2][2], serde_json::json!(3));
        }
    }

    #[test]
    fn row_content_cannot_fake_the_values_key() {
        // A row carrying the literal text `"values":[` inside a string.
        let body =
            r#"{"data":{"fields":["e"],"values":[[{"s":"fake \"values\":[ text"}]]},"errors":[]}"#;
        let rows = scan(body, 3).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][0]["s"], serde_json::json!("fake \"values\":[ text"));
    }

    #[test]
    fn truncated_body_is_an_error() {
        // Row incomplete: error.
        let err = scan(r#"{"data":{"values":[[{"a":1"#, 64).unwrap_err();
        assert!(err.contains("truncated"), "{err}");
        // Row complete but the values array never closes: error.
        let err = scan(r#"{"data":{"values":[["x","y"]"#, 64).unwrap_err();
        assert!(err.contains("truncated"), "{err}");
    }

    const STREAM_CLOSED: &str = "the server closed the response stream";

    fn read_error_message(kind: std::io::ErrorKind) -> String {
        let e = std::io::Error::new(kind, "Peer disconnected");
        mid_statement_read_error("http://test/db/neo4j/query/v2", e).to_string()
    }

    #[test]
    fn connection_reset_class_read_errors_carry_the_stream_closed_hint() {
        for kind in [
            std::io::ErrorKind::ConnectionReset,
            std::io::ErrorKind::BrokenPipe,
            std::io::ErrorKind::UnexpectedEof,
        ] {
            let msg = read_error_message(kind);
            // The hint is added to the plain text, not in place of it.
            assert!(msg.contains("mid-statement"), "{kind:?}: {msg}");
            assert!(msg.contains("Peer disconnected"), "{kind:?}: {msg}");
            assert!(msg.contains(STREAM_CLOSED), "{kind:?}: {msg}");
            assert!(!msg.contains("debug build"), "{kind:?}: {msg}");
            assert!(!msg.contains("oversubscribed"), "{kind:?}: {msg}");
        }
    }

    #[test]
    fn other_read_errors_keep_the_plain_text() {
        // Any other io error (here TimedOut) is not a stream-closed shape:
        // the hint would mislead on non-Neo4j servers, so only the plain
        // text remains.
        let msg = read_error_message(std::io::ErrorKind::TimedOut);
        assert!(msg.contains("mid-statement"), "{msg}");
        assert!(msg.contains("Peer disconnected"), "{msg}");
        assert!(!msg.contains(STREAM_CLOSED), "{msg}");
    }

    #[test]
    fn a_mid_stream_read_failure_is_mapped_through_the_read_error_helper() {
        // The body starts streaming rows, then the reader fails
        // mid-statement.
        struct FailingReader {
            head: Option<&'static [u8]>,
        }
        impl Read for FailingReader {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                match self.head.take() {
                    Some(head) => {
                        buf[..head.len()].copy_from_slice(head);
                        Ok(head.len())
                    }
                    None => Err(std::io::Error::new(
                        std::io::ErrorKind::ConnectionReset,
                        "Peer disconnected",
                    )),
                }
            }
        }
        let reader = FailingReader {
            head: Some(br#"{"data":{"values":[[1]],"#),
        };
        let err =
            stream_v2_rows(reader, "http://test/db/neo4j/query/v2", &mut |_| Ok(())).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains(STREAM_CLOSED), "{msg}");
    }

    #[test]
    fn a_body_without_values_is_a_query_error_response() {
        let err = scan(r#"{"errors":[{"message":"boom"}]}"#, 64).unwrap_err();
        assert!(err.contains("boom"), "{err}");
    }

    #[test]
    fn trailing_errors_after_rows_fail_the_query() {
        let body =
            r#"{"data":{"fields":["e"],"values":[["x"]]},"errors":[{"message":"late boom"}]}"#;
        let err = scan(body, 64).unwrap_err();
        assert!(err.contains("late boom"), "{err}");
        // Trailing errors survive any chunking, including the close of the
        // values array landing in the same chunk as the rows.
        for chunk in [1usize, 5, 64] {
            let err = scan(body, chunk).unwrap_err();
            assert!(err.contains("late boom"), "chunk {chunk}: {err}");
        }
    }

    #[test]
    fn unparseable_row_is_an_error_not_a_silent_drop() {
        let err = scan(r#"{"data":{"values":[[not-json]]},"errors":[]}"#, 64).unwrap_err();
        assert!(err.contains("unparseable"), "{err}");
    }

    #[test]
    fn an_object_row_is_rejected() {
        // v2 rows are arrays; a `{`-opened span is not a row.
        let err = scan(r#"{"data":{"values":[{"a":1}],"errors":[]}}"#, 64).unwrap_err();
        assert!(err.contains("unparseable"), "{err}");
    }

    #[test]
    fn non_utf8_tail_metadata_is_an_error() {
        let mut body = br#"{"data":{"values":[["x"]]},"errors":["#.to_vec();
        body.push(0xff);
        body.extend_from_slice(b"]}");
        let mut scanner = RowScanner::default();
        let rows = scanner.feed(&body).map_err(|e| e.to_string()).unwrap();
        assert_eq!(rows.len(), 1);
        let err = scanner
            .finish("test")
            .map_err(|e| e.to_string())
            .unwrap_err();
        assert!(err.contains("non-UTF-8"), "{err}");
    }

    #[test]
    fn an_oversized_row_is_an_error_not_an_unbounded_buffer() {
        let big = "x".repeat(MAX_RESPONSE_BYTES as usize + 1);
        let body = format!(r#"{{"data":{{"values":[["{big}"]]],"errors":[]}}"#);
        let err = scan(&body, 64 * 1024).unwrap_err();
        assert!(err.contains("row exceeded"), "{err}");
    }

    #[test]
    fn truncate_for_error_caps_length() {
        let long = "x".repeat(MAX_ERROR_BODY + 50);
        let out = truncate_for_error(&long);
        assert!(out.len() < long.len());
        assert!(out.contains("truncated"));
        assert_eq!(truncate_for_error("short"), "short");
    }

    #[test]
    fn truncate_for_error_strips_unsafe_chars() {
        let out = truncate_for_error("bad\u{1b}\u{202e}body");
        assert_eq!(out, "badbody");
        assert!(!out.chars().any(relate_core::is_unsafe_output_char));
    }

    #[test]
    fn truncate_for_error_strips_unsafe_chars_after_cap() {
        let long = format!("\u{1b}{}", "x".repeat(MAX_ERROR_BODY + 10));
        let out = truncate_for_error(&long);
        assert!(out.contains("truncated"));
        assert!(!out.chars().any(relate_core::is_unsafe_output_char));
    }

    #[test]
    fn parse_v2_response_strips_unsafe_chars_from_non_json_body() {
        let text = "{\"errors\":[{\"message\":\"boom\u{1b}\u{202e}\"}]}";
        let err = parse_v2_response(text).unwrap_err();
        assert!(matches!(err, LoadError::Body(_)));
        let msg = err.to_string();
        assert!(!msg.contains('\u{1b}'), "raw ESC leaked: {msg:?}");
        assert!(!msg.contains('\u{202e}'), "raw RLO leaked: {msg:?}");
    }

    #[test]
    fn parse_v2_response_strips_unsafe_chars_from_missing_data_values() {
        // serde_json's Display escapes C0 controls but emits Cf chars (U+202E)
        // raw, so the truncate_for_error strip is load-bearing on this path.
        let text = "{\"data\":{\"fields\":[\"n\u{202e}\"]}}";
        let err = parse_v2_response(text).unwrap_err();
        match err {
            LoadError::Body(msg) => {
                assert!(
                    msg.starts_with("v2 response missing data.values on success:"),
                    "{msg:?}"
                );
                assert!(!msg.contains('\u{202e}'), "raw RLO leaked: {msg:?}");
                assert!(!msg.chars().any(relate_core::is_unsafe_output_char));
            }
            other => panic!("expected LoadError::Body, got {other:?}"),
        }
    }
}
