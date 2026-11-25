//! The statement streamer's tests (ADR-0682): a result larger than the
//! stream threshold is written as the engine's batches arrive, a failure
//! after the chunked head is the in-band `errors` member, and a peer that
//! stops reading ends the statement instead of pinning it.
//!
//! Reached through `server::tests` (`#[path]`) for the loopback fixtures.

use std::time::Instant;

use relate_test_support::loopback::{dechunk, header_value, split_response};

use super::*;
use crate::reply::{PendingStatement, ResponseMediaType};

/// The workspace budget the two-million-row answer runs under: generous for
/// a few batches, impossible for the collected answer.
const POOL_BYTES: u64 = 16 << 20;

fn small_pool_db() -> Database {
    Database::open_with(
        ":memory:",
        relate::OpenOptions {
            concurrent_logical_workspace_limit: Some(POOL_BYTES),
            ..Default::default()
        },
    )
    .unwrap()
}

/// The de-chunked body of a raw response, with its head.
fn chunked_body(raw: &[u8]) -> (String, String) {
    let (head, frames) = split_response(raw);
    let (body, _) = dechunk(frames);
    (
        head.to_string(),
        String::from_utf8(body).expect("the body is UTF-8"),
    )
}

#[test]
fn two_million_rows_stream_under_a_small_pool() {
    let port = start(small_pool_db(), HttpConfig::new(AuthConfig::None));
    let request = post_db(
        "neo4j",
        r#"{"statement": "UNWIND range(1, 2000000) AS n RETURN n"}"#,
        None,
    );
    let (status, raw) = exchange_bytes(port, request.as_bytes());
    assert_eq!(status, 200);
    let (head, body) = chunked_body(&raw);
    assert_eq!(header_value(&head, "transfer-encoding"), Some("chunked"));
    assert!(
        body.starts_with(r#"{"data":{"fields":["n"],"values":[[1],[2],[3],"#),
        "{}",
        &body[..60]
    );
    assert_eq!(body.matches("],[").count() + 1, 2_000_000);
    assert!(
        body.contains(r#"]]},"bookmarks":[]"#),
        "the tail follows the rows"
    );
}

#[test]
fn an_error_after_the_head_is_the_in_band_errors_member() {
    let port = start(
        Database::open(":memory:").unwrap(),
        HttpConfig::new(AuthConfig::None),
    );
    // Row 150000 divides by zero: the head went out at the threshold, long
    // before, so the failure rides the tail of the 200.
    let request = post_db(
        "neo4j",
        r#"{"statement": "UNWIND range(1, 200000) AS n RETURN n / (n - 150000) AS q"}"#,
        None,
    );
    let (status, raw) = exchange_bytes(port, request.as_bytes());
    assert_eq!(status, 200);
    let (head, body) = chunked_body(&raw);
    assert_eq!(header_value(&head, "transfer-encoding"), Some("chunked"));
    assert!(
        body.starts_with(r#"{"data":{"fields":["q"],"values":[["#),
        "{}",
        &body[..60]
    );
    let tail_at = body.find(r#"]},"errors":[{"code":""#).unwrap_or_else(|| {
        panic!(
            "the errors member follows the data: {}",
            &body[body.len() - 200..]
        )
    });
    assert!(
        body.ends_with(r#""}]}"#),
        "the body closes after the error: {}",
        &body[body.len() - 80..]
    );
    assert!(
        !body[tail_at..].contains("bookmarks"),
        "a failed statement carries no success tail"
    );
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("the body is JSON");
    assert!(parsed["data"]["values"].as_array().unwrap().len() > 1000);
    assert_eq!(parsed["errors"].as_array().unwrap().len(), 1);
}

#[test]
fn an_error_before_any_row_keeps_its_status() {
    let port = start(
        Database::open(":memory:").unwrap(),
        HttpConfig::new(AuthConfig::None),
    );
    let request = post_db("neo4j", r#"{"statement": "RETURN 1 / 0 AS q"}"#, None);
    let (status, body) = exchange_json(port, request);
    assert_eq!(status, 400, "{body}");
    assert!(body["errors"][0]["code"].is_string());
}

/// A sink whose first write fails like a socket past its write timeout.
struct StalledSink {
    writes: usize,
}

impl Write for StalledSink {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        self.writes += 1;
        Err(io::Error::from(io::ErrorKind::TimedOut))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_failed_write_cancels_the_statement() {
    let db = Database::open(":memory:").unwrap();
    let pending = PendingStatement {
        conn: db.connect().unwrap(),
        // A hundred million rows: collected or run to the end this would
        // not return, so returning at all is the cancellation.
        statement: "UNWIND range(1, 100000000) AS n RETURN n".to_string(),
        params: Default::default(),
        include_counters: false,
        media_type: ResponseMediaType::Json,
    };
    let mut sink = StalledSink { writes: 0 };
    let started = Instant::now();
    let error = match crate::streamed::serve(&mut sink, pending, Vec::new(), None) {
        Err(error) => error,
        Ok(_) => panic!("a stalled sink fails the reply"),
    };
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(sink.writes, 1, "nothing is written after the failure");
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "the statement stopped at the failed write"
    );
}

#[test]
fn a_peer_that_stops_reading_ends_the_statement_at_the_write_timeout() {
    let db = Database::open(":memory:").unwrap();
    let mut config = HttpConfig::new(AuthConfig::None);
    config.write_timeout = Duration::from_millis(300);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        serve_handler(&db, stream, &config)
    });
    // Ask for far more than the socket buffers hold, then never read.
    let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
    client
        .write_all(
            post_db(
                "neo4j",
                r#"{"statement": "UNWIND range(1, 50000000) AS n RETURN n"}"#,
                None,
            )
            .as_bytes(),
        )
        .unwrap();
    let started = Instant::now();
    let outcome = server.join().expect("the connection thread returns");
    let error = outcome.expect_err("the stalled write fails the connection");
    assert!(
        matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ),
        "{error}"
    );
    assert!(started.elapsed() < Duration::from_secs(30));
    drop(client);
}
