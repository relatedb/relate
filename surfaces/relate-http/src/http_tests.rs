use std::io::BufReader;
use std::time::Duration;

use super::*;

use crate::content_encoding::ReplyEncodingPolicy;

/// The admission limits these tests run under: no deadlines (nothing to
/// trickle), a caller-chosen body cap, and the ordinary read timeout.
fn limits(max_body_bytes: usize) -> ReadLimits {
    ReadLimits::new(
        Duration::ZERO,
        Duration::ZERO,
        max_body_bytes,
        Duration::from_secs(30),
    )
    .expect("valid test limits")
}

/// Drive the one public admission path over an in-memory request.
fn admit(raw: &[u8], auth: &relate_auth::AuthConfig, max_body_bytes: usize) -> AdmissionOutcome {
    let mut reader = BufReader::new(raw);
    read_admitted_request(
        &mut reader,
        auth,
        &OriginAllowlist::Default,
        Cors::Browser,
        crate::query_api::is_discovery_probe,
        &limits(max_body_bytes),
        |_| Ok(()),
    )
    .expect("the in-memory admission read cannot hit a transport failure")
}

#[test]
fn parses_request_line_headers_and_body() {
    let raw = b"POST /db/neo4j/query/v2 HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: 2\r\nAuthorization: Basic QQ==\r\n\r\n{}";
    let AdmissionOutcome::Request { request, public } =
        admit(raw, &relate_auth::AuthConfig::None, 1024)
    else {
        panic!("expected a complete request");
    };
    assert!(!public);
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/db/neo4j/query/v2");
    assert_eq!(request.header("authorization"), Some("Basic QQ=="));
    assert_eq!(request.header("CONTENT-LENGTH"), Some("2"));
    assert_eq!(request.body, b"{}");
    // Request derefs to its head.
    assert_eq!(request.headers.len(), 4);
}

#[test]
fn clean_eof_is_closed() {
    assert!(matches!(
        admit(b"", &relate_auth::AuthConfig::None, 1024),
        AdmissionOutcome::Closed
    ));
}

#[test]
fn body_over_the_cap_is_a_400_rejection() {
    let raw = b"POST /db/neo4j/query/v2 HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n";
    let AdmissionOutcome::Rejected { rejection, drain } =
        admit(raw, &relate_auth::AuthConfig::None, 16)
    else {
        panic!("expected a 400 rejection");
    };
    assert_eq!(rejection.status, 400);
    assert!(
        rejection.message.contains("body too large"),
        "{}",
        rejection.message
    );
    // The remaining declared body is drained after the answer.
    assert!(matches!(drain, DrainPlan::Declared(16)), "{drain:?}");
}

#[test]
fn a_failed_auth_check_is_a_401_rejection_with_the_challenge() {
    let raw = b"POST /db/neo4j/query/v2 HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}";
    let auth = relate_auth::AuthConfig::basic("neo4j", "s3cret");
    let AdmissionOutcome::Rejected { rejection, .. } = admit(raw, &auth, 1024) else {
        panic!("expected a 401 rejection");
    };
    assert_eq!(rejection.status, 401);
    assert!(rejection
        .extra_headers
        .iter()
        .any(|(n, v)| n == "WWW-Authenticate" && v.contains("Basic realm=")));
}

#[test]
fn an_unterminated_header_line_is_bounded_by_the_cap() {
    // One giant header line with no newline at all: the read must fail
    // at the cap instead of buffering the whole line (the peer never
    // sends the terminator the old cap check waited for).
    let mut raw = b"POST /db/neo4j/query/v2 HTTP/1.1\r\nX-Filler: ".to_vec();
    raw.extend(std::iter::repeat_n(b'x', MAX_HEADER_BYTES * 2));
    let AdmissionOutcome::BadRequest { message, drain } =
        admit(&raw, &relate_auth::AuthConfig::None, 1024)
    else {
        panic!("an over-cap unterminated line is a malformed head");
    };
    assert_eq!(message, "malformed request: headers too large", "{message}");
    assert!(matches!(drain, DrainPlan::Unparsed), "{drain:?}");
}

/// The one in-crate request-head fixture: `method` at `path`, HTTP/1.1,
/// `headers` in wire order, no leftover bytes — every test module's request
/// head comes from here, paired with [`RequestHead::into_request`] when it
/// carries a body.
pub(crate) fn head_at(method: &str, path: &str, headers: &[(&str, &str)]) -> RequestHead {
    RequestHead {
        method: method.to_string(),
        path: path.to_string(),
        version: HttpVersion::Http11,
        headers: headers
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect(),
        leftover: Vec::new(),
    }
}

#[test]
fn created_is_a_phrase_and_the_fallback_is_not_reached_by_201() {
    // The tx routes answer 201: an unmapped status renders the fallback
    // phrase, so a 201 without its own row would say "Internal Server
    // Error" on a successful BEGIN.
    assert_eq!(reason(201), "Created");
    // The preflight's body-less 204 carries its own phrase too.
    assert_eq!(reason(204), "No Content");
    // 500 is a status the surface emits (an internal engine error's
    // envelope), so it owns a row instead of reaching the fallback.
    assert_eq!(reason(500), "Internal Server Error");
    // A status this surface never emits keeps the fallback phrase.
    assert_eq!(reason(599), "Internal Server Error");
}

#[test]
fn the_one_head_writer_frames_by_length_or_chunked() {
    let extra = [("x-extra".to_string(), "1".to_string())];
    let mut length = Vec::new();
    write_head(
        &mut length,
        201,
        "application/json",
        Framing::Length(2),
        &extra,
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(length).unwrap(),
        concat!(
            "HTTP/1.1 201 Created\r\n",
            "content-type: application/json\r\n",
            "content-length: 2\r\n",
            "connection: close\r\n",
            "x-extra: 1\r\n",
            "\r\n"
        )
    );
    let mut chunked = Vec::new();
    write_head(&mut chunked, 200, "application/json", Framing::Chunked, &[]).unwrap();
    assert_eq!(
        String::from_utf8(chunked).unwrap(),
        concat!(
            "HTTP/1.1 200 OK\r\n",
            "content-type: application/json\r\n",
            "transfer-encoding: chunked\r\n",
            "connection: close\r\n",
            "\r\n"
        )
    );
}

#[test]
fn an_http_1_1_head_records_its_version() {
    let raw = b"POST /db/neo4j/query/v2 HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}";
    let AdmissionOutcome::Request { request, .. } =
        admit(raw, &relate_auth::AuthConfig::None, 1024)
    else {
        panic!("expected a complete request");
    };
    assert_eq!(request.version, HttpVersion::Http11);
}

#[test]
fn an_http_1_0_request_is_a_parsed_400_rejection() {
    // httparse parses HTTP/1.0 as version 0, but this surface answers
    // HTTP/1.1 only: the head is a parsed rejection (400) before any gate, so
    // a 1.0 peer can never be answered with the HTTP/1.1-only chunked framing
    // (RFC 9112 §7.1). The real head rides on the rejection, so the reply
    // seam reads the client's Origin and Accept-Encoding and the drain uses
    // the declared body — not the malformed head's unparsed fallbacks.
    let raw = b"POST /db/neo4j/query/v2 HTTP/1.0\r\nContent-Type: application/json\r\nContent-Length: 2\r\nOrigin: https://browser.neo4j.io\r\nAccept-Encoding: gzip\r\n\r\n{}";
    let AdmissionOutcome::Rejected { rejection, .. } =
        admit(raw, &relate_auth::AuthConfig::None, 1024)
    else {
        panic!("an HTTP/1.0 request is refused, not answered");
    };
    let Rejection {
        head,
        public,
        status,
        code,
        message,
        extra_headers,
    } = rejection;
    assert_eq!(status, 400);
    assert_eq!(code, relate_error::GQLSTATUS_GENERAL);
    assert!(message.contains("HTTP/1.0"), "{message}");
    assert!(message.contains("HTTP/1.1"), "{message}");
    assert!(
        extra_headers.is_empty(),
        "the version floor adds no headers"
    );
    // The parsed head carries what the reply seam reads, and the
    // gate's probe classification rides as data.
    assert!(!public, "a 1.0 POST is not the discovery probe");
    assert_eq!(head.header("origin"), Some("https://browser.neo4j.io"));
    assert_eq!(head.header("content-length"), Some("2"));
    match ReplyEncodingPolicy::for_head(&head, public) {
        ReplyEncodingPolicy::Negotiated(Some(header)) => assert_eq!(header, "gzip"),
        _ => panic!("the 400 negotiates from the client's real Accept-Encoding"),
    }
}
