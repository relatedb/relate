//! The chunked-framing tests of the Query API reply seam (ADR-0386): a small
//! reply keeps its one-shot `Content-Length` body, a large one streams RFC
//! 9112 chunk frames that reassemble to the identity envelope, gzip composes
//! above the frame writer, and the pull client's own HTTP stack (ureq)
//! decodes both directions transparently.
//!
//! Reached through `server::tests` (`#[path]`), so the loopback exchange and
//! response-splitting fixtures have one home.

use relate_test_support::loopback::{
    dechunk, gunzip, header_value, max_frame_size, post_with_version, split_response,
    JSON_CONTENT_TYPE,
};

use super::*;
use crate::v2_envelope::tests::body_json;

/// A statement whose answer is 100_000 rows — far above the stream threshold,
/// so its reply takes the chunked framing (ADR-0386 §1).
const STREAM_BODY: &str = r#"{"statement": "UNWIND range(1, 100000) AS n RETURN n"}"#;

/// LABS-1120: the envelope's per-request tail — everything from the
/// `"timings"` member to the closing brace — cut from the raw bytes, so a
/// byte-for-byte comparison of two requests for the same statement holds
/// for the deterministic prefix. The one spelling: the parent module's
/// gzip roundtrip test calls it too (`pub(super)`).
pub(super) fn strip_timing_tail(body: &[u8]) -> &[u8] {
    let text = std::str::from_utf8(body).expect("the reply body is UTF-8");
    match text.find(",\"timings\":") {
        Some(index) => &body[..index],
        None => body,
    }
}

/// LABS-1120: the `timings`/`planCache` members carry per-request numbers,
/// so cross-request envelope comparisons drop them — everything else is
/// deterministic for the same statement and stays compared.
fn without_timing_members(mut envelope: serde_json::Value) -> serde_json::Value {
    if let Some(object) = envelope.as_object_mut() {
        object.remove("timings");
        object.remove("planCache");
    }
    envelope
}

/// The success envelope for `statement` as the reply seam constructs it: the
/// identity envelope a chunked (and gzip-wrapped) streaming body must
/// reassemble to (ADR-0386 §2/§4). Built through the seam's own handler so the
/// comparison is the envelope, not a re-encoding.
fn identity_envelope(statement: &str) -> serde_json::Value {
    let db = Database::open(":memory:").unwrap();
    let request = crate::http::tests::head_at(
        "POST",
        "/db/neo4j/query/v2",
        &[("content-type", JSON_CONTENT_TYPE)],
    )
    .into_request(statement.as_bytes().to_vec());
    let reply = query_api::handle(
        &db,
        &request,
        &AuthConfig::None,
        None,
        &crate::tx_registry::TxRegistry::new(),
        query_api::CredentialGate::Unchecked {
            public: query_api::is_discovery_probe(&request),
        },
        None,
    );
    let body = body_json(&reply);
    assert_eq!(reply.status, 200, "{body}");
    body
}

#[test]
fn small_results_keep_content_length_and_no_transfer_encoding() {
    let port = start(
        Database::open(":memory:").unwrap(),
        HttpConfig::new(AuthConfig::None),
    );
    // A plain small answer: one-shot identity with its declared length.
    let (status, raw) = exchange_bytes(
        port,
        post_db("neo4j", r#"{"statement": "RETURN 1 AS n"}"#, None).as_bytes(),
    );
    assert_eq!(status, 200, "{raw:?}");
    let (head, body) = split_response(&raw);
    assert_eq!(header_value(head, "transfer-encoding"), None, "{head}");
    assert!(body.len() < crate::stream::STREAM_THRESHOLD_BYTES);
    assert_eq!(one_shot_content_length(head), body.len(), "{head}");
    // A sub-threshold gzip answer stays one-shot too: the declared length is
    // the compressed body's.
    let request = post_db_with_headers("neo4j", BIG_BODY, &[("accept-encoding", "gzip")]);
    let (status, raw) = exchange_bytes(port, request.as_bytes());
    assert_eq!(status, 200, "{raw:?}");
    let (head, payload) = split_response(&raw);
    assert_eq!(header_value(head, "transfer-encoding"), None, "{head}");
    assert_eq!(
        header_value(head, "content-encoding"),
        Some("gzip"),
        "{head}"
    );
    assert!(payload.len() < crate::stream::STREAM_THRESHOLD_BYTES);
    assert_eq!(one_shot_content_length(head), payload.len(), "{head}");
}

#[test]
fn a_large_result_streams_chunk_frames_that_reassemble_to_the_envelope() {
    let port = start(
        Database::open(":memory:").unwrap(),
        HttpConfig::new(AuthConfig::None),
    );
    // No Accept-Encoding: identity, and above the stream threshold that is
    // still chunked — no content-length, one head, then frames.
    let (status, raw) = exchange_bytes(port, post_db("neo4j", STREAM_BODY, None).as_bytes());
    assert_eq!(status, 200, "{raw:?}");
    let (head, frames) = split_response(&raw);
    assert_eq!(
        header_value(head, "transfer-encoding"),
        Some("chunked"),
        "{head}"
    );
    assert_eq!(header_value(head, "content-length"), None, "{head}");
    assert_eq!(header_value(head, "content-encoding"), None, "{head}");
    assert_eq!(
        header_value(head, "vary"),
        Some("Accept-Encoding"),
        "{head}"
    );
    assert_eq!(header_value(head, "connection"), Some("close"), "{head}");
    // The frames are the raw chunked body: every one of them — not only the
    // first — is bounded by the frame writer's buffer (ADR-0386 §3).
    assert!(
        max_frame_size(frames) <= crate::stream::FRAME_BUFFER_BYTES,
        "every streamed frame is bounded by the frame buffer"
    );
    // Parse them, reassemble the envelope, and compare it to the identity
    // serialization one-shot replies write.
    let (body, frame_count) = dechunk(frames);
    assert!(
        frame_count > 1 && frame_count < 1_000,
        "a streamed body arrives in a bounded number of frames, got {frame_count}"
    );
    let envelope: serde_json::Value =
        serde_json::from_slice(&body).expect("the frames reassemble to one JSON envelope");
    let values = envelope["data"]["values"]
        .as_array()
        .expect("data.values rows");
    assert_eq!(values.len(), 100_000);
    assert_eq!(values[0], serde_json::json!([1]));
    assert_eq!(values[99_999], serde_json::json!([100_000]));
    assert_eq!(envelope["bookmarks"], serde_json::json!([]));
    assert_eq!(
        without_timing_members(envelope.clone()),
        without_timing_members(identity_envelope(STREAM_BODY)),
        "the reassembled chunked body is the identity envelope"
    );

    // The identity-fetched envelope is the framing control: the same
    // statement asking for identity explicitly answers the same bytes, so the
    // framing changes the transfer only, never the envelope.
    let request = post_db_with_headers("neo4j", STREAM_BODY, &[("accept-encoding", "identity")]);
    let (status, identity_raw) = exchange_bytes(port, request.as_bytes());
    assert_eq!(status, 200, "{identity_raw:?}");
    let (identity_head, identity_frames) = split_response(&identity_raw);
    assert_eq!(
        header_value(identity_head, "content-encoding"),
        None,
        "{identity_head}"
    );
    assert_eq!(
        header_value(identity_head, "transfer-encoding"),
        Some("chunked"),
        "{identity_head}"
    );
    let (identity_body, _) = dechunk(identity_frames);
    // LABS-1120: the byte-for-byte contract holds for the deterministic
    // body — the timing members are the per-request tail.
    assert_eq!(
        strip_timing_tail(&body),
        strip_timing_tail(&identity_body),
        "the chunked body is the identity-fetched envelope, byte for byte"
    );
}

#[test]
fn a_large_gzip_result_streams_chunk_frames_that_inflate_to_the_envelope() {
    let port = start(
        Database::open(":memory:").unwrap(),
        HttpConfig::new(AuthConfig::None),
    );
    let request = post_db_with_headers("neo4j", STREAM_BODY, &[("accept-encoding", "gzip")]);
    let (status, raw) = exchange_bytes(port, request.as_bytes());
    assert_eq!(status, 200, "{raw:?}");
    let (head, frames) = split_response(&raw);
    assert_eq!(
        header_value(head, "transfer-encoding"),
        Some("chunked"),
        "{head}"
    );
    assert_eq!(header_value(head, "content-length"), None, "{head}");
    assert_eq!(
        header_value(head, "content-encoding"),
        Some("gzip"),
        "{head}"
    );
    assert_eq!(
        header_value(head, "vary"),
        Some("Accept-Encoding"),
        "{head}"
    );
    // gzip composes above the frame writer (ADR-0386 §3): de-chunk the
    // frames, then inflate one gzip member to the identity envelope. The
    // compressor's writes are framed too, so every frame stays bounded.
    assert!(
        max_frame_size(frames) <= crate::stream::FRAME_BUFFER_BYTES,
        "the compressor's frames are bounded by the frame buffer"
    );
    let (compressed, frame_count) = dechunk(frames);
    assert!(
        frame_count > 1,
        "the compressor's writes are several frames"
    );
    assert_eq!(
        without_timing_members(
            serde_json::from_slice::<serde_json::Value>(&gunzip(&compressed)).unwrap()
        ),
        without_timing_members(identity_envelope(STREAM_BODY)),
        "gzip over chunked inflates to the exact identity envelope"
    );
}

#[test]
fn an_http_1_0_request_is_refused_and_never_gets_chunked_framing() {
    let port = start(
        Database::open(":memory:").unwrap(),
        HttpConfig::new(AuthConfig::None),
    );
    // The statement an HTTP/1.1 client gets chunked above; a 1.0 peer is a
    // parsed 400 rejection at the head, so its answer is one-shot identity —
    // `Transfer-Encoding: chunked` is HTTP/1.1-only framing (RFC 9112 §7.1)
    // and never reaches a client that does not speak it.
    let request = post_with_version("HTTP/1.0", "/db/neo4j/query/v2", STREAM_BODY);
    let (status, raw) = exchange_bytes(port, request.as_bytes());
    assert_eq!(status, 400, "{raw:?}");
    let (head, body) = split_response(&raw);
    assert_eq!(header_value(head, "transfer-encoding"), None, "{head}");
    assert_eq!(one_shot_content_length(head), body.len(), "{head}");
    assert_eq!(header_value(head, "vary"), Some("Accept-Encoding"));
    let envelope: serde_json::Value = serde_json::from_slice(body).unwrap();
    let message = envelope["errors"][0]["message"]
        .as_str()
        .expect("the 400 is an error envelope");
    assert!(
        message.contains("HTTP/1.0") && message.contains("HTTP/1.1"),
        "the refusal names the version: {message}"
    );
}

#[test]
fn ureq_decodes_a_large_gzip_chunked_result_transparently() {
    let port = start(
        Database::open(":memory:").unwrap(),
        HttpConfig::new(AuthConfig::None),
    );
    let url = format!("http://127.0.0.1:{port}/db/neo4j/query/v2");
    // The pull client's stack (relate-load's ureq): it unchunks the
    // transfer coding and inflates the gzip representation inside its body
    // reader (ADR-0386 §Context). Accept-Encoding is explicit so the request
    // negotiates gzip whatever ureq's default feature set is — its decode
    // trigger is the response header, not the request one.
    let mut response = ureq::post(&url)
        .header("content-type", "application/json")
        .header("accept-encoding", "gzip")
        .config()
        .http_status_as_error(false)
        .build()
        .send(STREAM_BODY)
        .expect("the live test server answers the loopback request");
    assert_eq!(response.status().as_u16(), 200);
    let body = response
        .body_mut()
        .read_to_string()
        .expect("the body decodes transparently (chunked, then gzip)");
    let envelope: serde_json::Value =
        serde_json::from_str(&body).expect("the decoded body is one JSON envelope");
    let values = envelope["data"]["values"]
        .as_array()
        .expect("data.values rows");
    assert_eq!(values.len(), 100_000);
    assert_eq!(values[0], serde_json::json!([1]));
    assert_eq!(values[99_999], serde_json::json!([100_000]));
}
