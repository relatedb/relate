//! The server half of the loopback kit (see [`super`]'s docs for the
//! client half): the scripted raw-HTTP fixture family and the
//! run-until-stopped capture server, std::net only — the fixtures serve
//! fixed bodies, so a real HTTP stack would be dead weight.
//!
//! Every fixture here serves one fixed response per connection and records
//! what the client sent: the `spawn_*_capturing` variants expose the
//! captured request head (or the whole request) as the server thread's
//! join value, and [`spawn_capture_server`](super::spawn_capture_server)
//! records each request body on a [`CaptureSink`](super::CaptureSink) the
//! test polls until it drops the sink — the shape for a background worker
//! whose request count is not known up front.
//!
//! All public items are re-exported by the parent module, so tests import
//! them from `relate_test_support::loopback` as before — one public path.

use std::borrow::Cow;
use std::io::{BufRead, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::JSON_CONTENT_TYPE;

/// Serve scripted raw HTTP responses on loopback, one body per connection.
/// The server thread's join value is the captured request head of every
/// connection it served, in arrival order.
pub fn spawn_scripted_responses(
    status_line: &'static str,
    bodies: Vec<impl Into<Cow<'static, str>>>,
) -> (std::thread::JoinHandle<Vec<String>>, u16) {
    spawn_scripted(status_line, bodies, serve_head)
}

/// Serve one raw HTTP response on loopback. The server thread's join value
/// is the captured request head of the connection it served.
pub fn spawn_single_response(
    status_line: &'static str,
    body: impl Into<Cow<'static, str>>,
) -> (std::thread::JoinHandle<Vec<String>>, u16) {
    spawn_scripted_responses(status_line, vec![body])
}

/// Like [`spawn_scripted_responses`], but the returned [`CapturingFixture`]
/// exposes the request head of every connection served, so tests can assert
/// wire-level request details (ADR-0181).
pub fn spawn_scripted_responses_capturing(
    status_line: &'static str,
    bodies: Vec<impl Into<Cow<'static, str>>>,
) -> (CapturingFixture, u16) {
    let (thread, port) = spawn_scripted(status_line, bodies, serve_head);
    (CapturingFixture { thread }, port)
}

/// Like [`spawn_scripted_responses_capturing`], but the returned
/// [`CapturedRequests`] fixture exposes the **whole request** — head and
/// body — of every connection served, for suites that assert on request
/// payloads (the push write path's statements and parameters, ADR-0401).
pub fn spawn_scripted_responses_capturing_requests(
    status_line: &'static str,
    bodies: Vec<impl Into<Cow<'static, str>>>,
) -> (CapturedRequests, u16) {
    let (thread, port) = spawn_scripted(status_line, bodies, serve_request);
    (CapturedRequests { thread }, port)
}

/// A scripted fixture that records the whole request — head and body — of
/// every connection it served (see
/// [`spawn_scripted_responses_capturing_requests`]). Panics on join when
/// the server thread panicked, like [`CapturingFixture`].
pub struct CapturedRequests {
    thread: std::thread::JoinHandle<Vec<(String, String)>>,
}

impl CapturedRequests {
    /// Wait for the fixture to serve every scripted body and return every
    /// captured request as `(head, body)` pairs, in arrival order.
    pub fn join(self) -> Vec<(String, String)> {
        self.thread.join().expect("scripted server thread")
    }
}

/// Like [`spawn_single_response`], but captures the request head.
pub fn spawn_single_response_capturing(
    status_line: &'static str,
    body: impl Into<Cow<'static, str>>,
) -> (CapturingFixture, u16) {
    spawn_scripted_responses_capturing(status_line, vec![body])
}

/// Like [`spawn_single_response_capturing`], but the body is raw bytes —
/// the binary fixture a suite serves verbatim (an extension tarball,
/// ADR-0431): the response carries the exact bytes and their length, so
/// no string conversion can corrupt them in transit. One body, one
/// connection; the join value is the captured request head. Serves
/// through the same [`serve_connection`] the scripted fixtures do, with
/// the binary body's octet-stream content type — the one fixed-response
/// dance, not a second copy of it.
pub fn spawn_single_response_bytes_capturing(
    status_line: &'static str,
    body: Vec<u8>,
) -> (CapturingFixture, u16) {
    let (listener, port) = bound_listener();
    let thread = std::thread::spawn(move || {
        let Some(stream) = accept_one(&listener) else {
            return Vec::new();
        };
        serve_connection(
            &stream,
            status_line,
            "application/octet-stream",
            &body,
            None,
        )
        .map(|served| vec![served.head])
        .unwrap_or_default()
    });
    (CapturingFixture { thread }, port)
}

/// Serve one fixed raw HTTP response per request on loopback, running until
/// the returned [`CaptureSink`] is stopped or dropped. Unlike the scripted
/// spawns, the server has no fixed request count: it records every request
/// body it serves, in arrival order, for the sink to expose.
pub fn spawn_capture_server(
    status_line: &'static str,
    body: impl Into<Cow<'static, str>>,
) -> (CaptureSink, u16) {
    spawn_capture_server_with_delay(status_line, body, Duration::ZERO)
}

/// [`spawn_capture_server`] with a per-request response delay: the server
/// sleeps `delay` before it reads the request and answers, so a client that
/// abandons the wait mid-request sees nothing recorded and no answer —
/// the network-latency shape the instant server cannot reproduce (a
/// telemetry drain whose POST outlives a short-lived client, say).
pub fn spawn_capture_server_with_delay(
    status_line: &'static str,
    body: impl Into<Cow<'static, str>>,
    delay: Duration,
) -> (CaptureSink, u16) {
    let body = body.into();
    let (listener, port) = bound_listener();
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let thread = {
        let bodies = Arc::clone(&bodies);
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        // The wait happens before the read and the
                        // response: the delayed shape's client sees
                        // neither a record nor an answer until it
                        // passes — and a zero delay waits nothing.
                        std::thread::sleep(delay);
                        let bodies = &bodies;
                        let _ = serve_connection(
                            &stream,
                            status_line,
                            JSON_CONTENT_TYPE,
                            body.as_bytes(),
                            // Record before the response bytes go out.
                            // `push` samples the sink the moment the client
                            // finishes, and the record used to happen after
                            // the write, so the last request was missing.
                            Some(&|served| {
                                bodies.lock().expect("bodies").push(served.body.clone());
                            }),
                        );
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        })
    };
    (
        CaptureSink {
            bodies,
            stop,
            thread: Some(thread),
        },
        port,
    )
}

/// A run-until-stopped capture server (see [`spawn_capture_server`]):
/// records the body of every request it serves. Dropping the sink stops the
/// server and joins its thread.
pub struct CaptureSink {
    bodies: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl CaptureSink {
    /// The request bodies recorded so far, in arrival order.
    pub fn bodies(&self) -> Vec<String> {
        self.bodies.lock().expect("bodies").clone()
    }

    /// Every recorded body parsed as the JSON array it must be, flattened
    /// into its elements — the one statement of the array-body contract
    /// (a `/track` batch body is a JSON array of envelopes) every
    /// capturing suite reads. Panics loudly on a non-JSON body or a
    /// non-array one: a broken capture must fail the test, not pass it.
    /// Behind `wire`, the fixture side whose feature carries the JSON
    /// parser.
    #[cfg(feature = "wire")]
    pub fn json_bodies(&self) -> Vec<serde_json::Value> {
        self.bodies()
            .iter()
            .flat_map(|body| {
                let value: serde_json::Value = serde_json::from_str(body)
                    .unwrap_or_else(|e| panic!("mock received a non-JSON body: {e}: {body}"));
                value
                    .as_array()
                    .expect("a captured body must be a JSON array")
                    .clone()
            })
            .collect()
    }

    /// How many requests the server has recorded so far.
    pub fn request_count(&self) -> usize {
        self.bodies.lock().expect("bodies").len()
    }

    /// Stop the server; the thread exits on its next accept poll (≤ 5 ms)
    /// and is joined on drop. Idempotent.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for CaptureSink {
    fn drop(&mut self) {
        self.stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A scripted-fixture server that records the request head of every
/// connection it served (request line + headers, CRLF-terminated), one entry
/// per connection in arrival order.
pub struct CapturingFixture {
    thread: std::thread::JoinHandle<Vec<String>>,
}

impl CapturingFixture {
    /// Wait for the fixture to serve every scripted body and return the
    /// captured request heads. Panics when the server thread panicked, so a
    /// broken fixture fails the test here instead of surfacing as a
    /// misleading captured-heads count.
    pub fn join(self) -> Vec<String> {
        self.thread.join().expect("scripted server thread")
    }
}

/// Bind the loopback listener every fixture shares: ephemeral port,
/// nonblocking so an accept loop can poll a stop flag between attempts,
/// and a panic on bind — a fixture that cannot bind is a broken test
/// environment, not a condition to report.
fn bound_listener() -> (TcpListener, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().expect("local addr").port();
    listener.set_nonblocking(true).expect("nonblocking");
    (listener, port)
}

/// Bind a scripted server and spawn its thread; the thread's join value
/// is what `serve` returns for every connection it reads, in arrival
/// order — the one scripted-spawn skeleton every scripted shape rides,
/// with the per-connection projection as its only variable.
fn spawn_scripted<T: Send + 'static>(
    status_line: &'static str,
    bodies: Vec<impl Into<Cow<'static, str>>>,
    serve: fn(&TcpListener, &'static str, &str) -> Option<T>,
) -> (std::thread::JoinHandle<Vec<T>>, u16) {
    let bodies: Vec<Cow<'static, str>> = bodies.into_iter().map(Into::into).collect();
    let (listener, port) = bound_listener();
    let handle = std::thread::spawn(move || {
        bodies
            .iter()
            .map_while(|body| serve(&listener, status_line, body))
            .collect()
    });
    (handle, port)
}

/// One served request: the captured head (request line + headers,
/// CRLF-terminated) and the request body.
struct ServedRequest {
    head: String,
    body: String,
}

/// Serve one connection of a scripted fixture: accept it, read its request,
/// write the response, and return the whole captured request — the
/// `(head, body)` pair. Returns `None` when no connection arrived before
/// the deadline or the peer hung up mid-request, so a test that never
/// connects cannot hang the server thread.
fn serve_request(
    listener: &TcpListener,
    status_line: &str,
    body: &str,
) -> Option<(String, String)> {
    let stream = accept_one(listener)?;
    serve_connection(
        &stream,
        status_line,
        JSON_CONTENT_TYPE,
        body.as_bytes(),
        None,
    )
    .map(|served| (served.head, served.body))
}

/// The head-only projection of [`serve_request`], for the scripted shapes
/// whose join value is the captured heads.
fn serve_head(listener: &TcpListener, status_line: &str, body: &str) -> Option<String> {
    serve_request(listener, status_line, body).map(|(head, _)| head)
}

/// Serve one already-accepted connection: force the socket back to blocking
/// mode (on macOS/BSD an accepted socket inherits the listener's O_NONBLOCK
/// flag, and `set_read_timeout` does not make a nonblocking socket block),
/// read the request, write the response, and return the captured head and
/// body. `None` when the peer hung up mid-request.
///
/// The one fixed-response writer every fixture spawn shares — the scripted
/// string fixtures (JSON content type, the body's UTF-8 bytes) and the
/// binary fixture (octet-stream, the body served verbatim, no string
/// conversion in transit) — so the response shape cannot drift between
/// them: `content_type` names the response's media type and `body` is the
/// exact bytes written after the head, their length declared.
fn serve_connection(
    stream: &TcpStream,
    status_line: &str,
    content_type: &str,
    body: &[u8],
    before_write: Option<&dyn Fn(&ServedRequest)>,
) -> Option<ServedRequest> {
    stream
        .set_nonblocking(false)
        .expect("blocking accepted socket");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("set read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .expect("set write timeout");
    let served = read_request(stream)?;
    if let Some(before_write) = before_write {
        before_write(&served);
    }
    let head = format!(
        "HTTP/1.1 {status_line}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len(),
    );
    let mut write_half = stream;
    let _ = write_half.write_all(head.as_bytes());
    let _ = write_half.write_all(body);
    Some(served)
}

/// Accept one connection from the nonblocking `listener`, polling on a 30 s
/// deadline; `None` when the deadline expires.
fn accept_one(listener: &TcpListener) -> Option<TcpStream> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match listener.accept() {
            Ok((stream, _)) => return Some(stream),
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return None,
        }
    }
}

/// Read one request head and its `Content-Length` body; `None` when the peer
/// hangs up first. Returns the head text (request line + headers + the
/// terminating CRLF) and the body for capture. `read_line` and `read_exact`
/// retry `Interrupted` internally, so an error here is a broken connection,
/// not a signal.
fn read_request(stream: &TcpStream) -> Option<ServedRequest> {
    let mut reader = std::io::BufReader::new(stream);
    let mut content_length = 0usize;
    let mut head = String::new();
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => return None,
            Ok(_) => {
                if let Some(rest) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = rest.trim().parse().unwrap_or(0);
                }
                head.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            Err(_) => return None,
        }
    }
    let mut request_body = vec![0u8; content_length];
    reader.read_exact(&mut request_body).ok()?;
    Some(ServedRequest {
        head,
        body: String::from_utf8_lossy(&request_body).into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loopback::{exchange, exchange_bytes, split_response};
    use crate::loopback::{header_value, header_values};

    #[test]
    fn capturing_fixture_propagates_a_server_panic() {
        let fixture = CapturingFixture {
            thread: std::thread::spawn(|| panic!("server boom")),
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fixture.join()));
        let payload = result.unwrap_err();
        let msg = payload.downcast_ref::<String>();
        assert!(
            msg.is_some_and(|m| m.contains("scripted server thread")),
            "join must re-raise the server panic, got: {msg:?}"
        );
    }

    #[test]
    fn scripted_server_answers_one_request_per_body() {
        const REQUEST: &[u8] = b"POST /db/neo4j/query/v2 HTTP/1.1\r\nContent-Length: 2\r\n\r\n{}";
        let (h, port) = spawn_scripted_responses("200 OK", vec!["{\"a\":1}", "{\"b\":2}"]);
        for expected in ["{\"a\":1}", "{\"b\":2}"] {
            let (status, resp) = exchange(port, REQUEST);
            assert_eq!(status, 200, "{resp}");
            assert!(resp.ends_with(expected), "{resp}");
        }
        h.join().unwrap();
    }

    #[test]
    fn capture_server_records_bodies_until_stopped() {
        const REQUEST: &[u8] = b"POST /track HTTP/1.1\r\nContent-Length: 7\r\n\r\n[{\"a\":1";
        let (sink, port) = spawn_capture_server("200 OK", "1");
        for _ in 0..2 {
            let (status, resp) = exchange(port, REQUEST);
            assert!(
                status == 200 && resp == "1",
                "the fixed response is served: {resp}"
            );
        }
        assert_eq!(sink.request_count(), 2);
        assert_eq!(sink.bodies(), vec!["[{\"a\":1", "[{\"a\":1"]);
        sink.stop();
        // Stopping is idempotent and the sink still reads back; drop joins
        // the server thread.
        sink.stop();
        assert_eq!(sink.request_count(), 2);
    }

    #[test]
    fn capture_server_drop_stops_and_joins_the_thread() {
        let (sink, port) = spawn_capture_server("200 OK", "1");
        let (status, _) = exchange(port, b"POST / HTTP/1.1\r\nContent-Length: 2\r\n\r\n{}");
        assert_eq!(status, 200);
        assert_eq!(sink.bodies(), vec!["{}"]);
        drop(sink); // must not hang: the accept loop polls the stop flag
    }

    #[test]
    fn capturing_fixture_records_every_request_head() {
        const REQUEST: &[u8] = b"POST /db/neo4j/query/v2 HTTP/1.1\r\n\
                                 Accept: application/json\r\n\
                                 Content-Length: 2\r\n\r\n{}";
        let (fixture, port) = spawn_scripted_responses_capturing("200 OK", vec!["{}", "{}"]);
        for _ in 0..2 {
            let (status, _) = exchange(port, REQUEST);
            assert_eq!(status, 200);
        }
        let heads = fixture.join();
        assert_eq!(heads.len(), 2, "one head per connection: {heads:?}");
        for head in &heads {
            assert!(
                head.starts_with("POST /db/neo4j/query/v2 HTTP/1.1\r\n"),
                "{head:?}"
            );
            assert_eq!(
                header_values(head, "accept"),
                vec!["application/json"],
                "the captured head parses back to its single Accept value: {head:?}"
            );
            assert!(head.ends_with("\r\n\r\n"), "{head:?}");
        }
    }

    /// The binary fixture's response shape: the octet-stream content type,
    /// the body's exact length declared, and the bytes themselves arriving
    /// verbatim (bytes that are not UTF-8, so any string conversion would
    /// corrupt them) — the generalization [`serve_connection`] gives the
    /// bytes fixture, pinned as the tarball-serving contract it is.
    #[test]
    fn bytes_fixture_serves_the_exact_bytes_as_octet_stream() {
        const REQUEST: &[u8] = b"GET /greet-1.2.3.tar.gz HTTP/1.1\r\nhost: localhost\r\n\r\n";
        let body: Vec<u8> = vec![0x1f, 0x8b, 0x00, 0xff, 0x8b, 0x1f];
        let (fixture, port) = spawn_single_response_bytes_capturing("200 OK", body.clone());
        let (status, raw) = exchange_bytes(port, REQUEST);
        assert_eq!(status, 200);
        let (head, served_body) = split_response(&raw);
        assert_eq!(
            header_value(head, "content-type"),
            Some("application/octet-stream"),
            "the binary fixture's content type: {head:?}"
        );
        assert_eq!(header_value(head, "content-length"), Some("6"));
        assert_eq!(
            served_body,
            &body[..],
            "the body bytes must arrive verbatim"
        );
        assert_eq!(fixture.join().len(), 1, "one connection, one head");
    }
}
