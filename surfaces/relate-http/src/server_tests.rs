use relate_test_support::loopback::{
    exchange, exchange_bytes, exchange_raw, gunzip, header_value, post, post_head_only,
    post_with_content_type, post_with_headers, request, split_response, LOOPBACK_HOST,
};

use super::*;
use crate::http::OriginFlagValue;
use crate::query_api::tests::{parse_expires, tx_id_from, SLOW_STATEMENT};

// The chunked-framing tests (ADR-0386) live in their own file, reached
// through this module so they share its loopback fixtures.
#[path = "streaming_tests.rs"]
mod streaming;

/// Start the server on an ephemeral port in the background; return the
/// port. The server runs until the test process ends.
fn start(db: Database, config: HttpConfig) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || serve(Arc::new(db), listener, config));
    port
}

/// The shared raw client with the body parsed as the JSON envelope.
fn exchange_json(port: u16, request: String) -> (u16, serde_json::Value) {
    let (status, body) = exchange(port, request.as_bytes());
    (status, serde_json::from_str(&body).unwrap())
}

/// The shared POST builder, addressed at a database's query route.
fn post_db(database: &str, body: &str, auth: Option<&str>) -> String {
    post(&format!("/db/{database}/query/v2"), body, auth)
}

/// Begin a `{}` transaction on the neo4j namespace over the wire and return
/// its route path and id — the one open-and-extract setup the expiry and
/// rollback wire tests share.
fn open_tx(port: u16) -> (String, String) {
    let (status, opened) = exchange_json(port, post("/db/neo4j/query/v2/tx", "{}", None));
    assert_eq!(status, 201, "{opened}");
    let id = tx_id_from(&opened);
    (format!("/db/neo4j/query/v2/tx/{id}"), id)
}

/// A POST at a database's query route carrying extra request headers
/// (the shared `post` sends no `Accept-Encoding`, which is the identity
/// default the compression tests vary from).
fn post_db_with_headers(database: &str, body: &str, extra_headers: &[(&str, &str)]) -> String {
    post_with_headers(&format!("/db/{database}/query/v2"), body, extra_headers)
}

/// A deterministic query envelope well above the gzip threshold: 500
/// integers serialize to roughly 1.9 KB.
const BIG_BODY: &str = r#"{"statement": "RETURN range(1, 500) AS r"}"#;

/// The `content-length` a one-shot reply declares (a chunked reply has none —
/// the framing tests assert that header's absence directly). The one-shot
/// contract, named, so the invariant is the caller's to assert.
fn one_shot_content_length(head: &str) -> usize {
    header_value(head, "content-length")
        .expect("a one-shot reply declares content-length")
        .parse()
        .expect("content-length is a number")
}

#[test]
fn serves_rows_parameters_errors_and_counters() {
    let port = start(
        Database::open(":memory:").unwrap(),
        HttpConfig::new(AuthConfig::None),
    );
    // Success with rows.
    let (status, body) = exchange_json(
        port,
        post_db("neo4j", r#"{"statement": "RETURN 1 AS n"}"#, None),
    );
    assert_eq!(status, 200);
    assert_eq!(body["data"]["fields"], serde_json::json!(["n"]));
    assert_eq!(body["data"]["values"], serde_json::json!([[1]]));
    assert_eq!(body["bookmarks"], serde_json::json!([]));
    // Parameters.
    let (status, body) = exchange_json(
        port,
        post_db(
            "neo4j",
            r#"{"statement": "RETURN $x AS x", "parameters": {"x": 7}}"#,
            None,
        ),
    );
    assert_eq!(status, 200);
    assert_eq!(body["data"]["values"], serde_json::json!([[7]]));
    // A Cypher error is the error envelope.
    let (status, body) = exchange_json(port, post_db("neo4j", r#"{"statement": "RETURN"}"#, None));
    assert_eq!(status, 400);
    assert_eq!(body["errors"][0]["code"], "42N06");
    // A write carries counters.
    let (status, body) = exchange_json(
        port,
        post_db("neo4j", r#"{"statement": "CREATE (:C {v: 1})"}"#, None),
    );
    assert_eq!(status, 200);
    assert_eq!(body["counters"]["nodes_created"], 1);
    // An unknown database is 42N00.
    let (status, body) = exchange_json(
        port,
        post_db("movies", r#"{"statement": "RETURN 1"}"#, None),
    );
    assert_eq!(status, 404);
    assert_eq!(body["errors"][0]["code"], "42N00");
}

#[test]
fn malformed_request_gets_a_400_not_a_silent_close() {
    let port = start(
        Database::open(":memory:").unwrap(),
        HttpConfig::new(AuthConfig::None),
    );
    // An over-cap Content-Length: the reader errors before allocating,
    // and the handler answers 400 instead of just closing.
    let request = post_head_only("/db/neo4j/query/v2", DEFAULT_MAX_BODY_BYTES + 1);
    let (status, body) = exchange(port, request.as_bytes());
    assert_eq!(status, 400, "{body:?}");
    let body: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(body["errors"][0]["message"]
        .as_str()
        .unwrap()
        .contains("malformed request"));
}

#[test]
fn browser_media_types_negotiate_on_the_wire() {
    let port = start(
        Database::open(":memory:").unwrap(),
        HttpConfig::new(AuthConfig::None),
    );
    // The request content type is the versioned Query API form
    // (ADR-0425: the unversioned spelling is rejected by every real
    // server, ours now too); the Browser Accept list negotiates the
    // response to it — Browser's client requires that content type and
    // decodes the typed body (LABS-376).
    let body = r#"{"statement": "RETURN 1 AS n"}"#;
    let request = post_with_content_type(
        "/db/neo4j/query/v2",
        "application/vnd.neo4j.query.v1.2",
        body,
        &[("accept", crate::query_api::tests::BROWSER_ACCEPT)],
    );
    let (status, raw) = exchange_raw(port, request.as_bytes());
    assert_eq!(status, 200, "{raw:?}");
    let (head, _) = split_response(raw.as_bytes());
    assert_eq!(
        header_value(head, "content-type"),
        Some("application/vnd.neo4j.query"),
        "{head}"
    );
    // … and /db/system/query/v2 with CALL db.ping() (Browser's
    // connectivity probe) routes and answers.
    let probe = r#"{"statement": "CALL db.ping()"}"#;
    let request = post_with_content_type(
        "/db/system/query/v2",
        "application/vnd.neo4j.query.v1.2",
        probe,
        &[],
    );
    let (status, body) = exchange(port, request.as_bytes());
    assert_eq!(status, 200, "{body}");
    assert!(
        body.contains("\"fields\":[\"success\"]") && body.contains("\"values\":[[true]]"),
        "{body}"
    );
}

#[test]
fn wired_unversioned_request_type_is_the_routes_400() {
    let port = start(
        Database::open(":memory:").unwrap(),
        HttpConfig::new(AuthConfig::None),
    );
    // ADR-0425's wired-path pin: the unversioned request type is not
    // browser-simple, so the guard passes it — and the statement
    // route's fail-closed 400 (naming the accepted spellings) is
    // what the wire answers, the same 400 every real server gives.
    // The pre-PR guard would have pre-empted it with a 415.
    let request = post_with_content_type(
        "/db/neo4j/query/v2",
        "application/vnd.neo4j.query",
        r#"{"statement": "RETURN 1"}"#,
        &[],
    );
    let (status, body) = exchange(port, request.as_bytes());
    assert_eq!(status, 400, "{body:?}");
    assert!(
        body.contains("vnd.neo4j.query.v1.2"),
        "the 400 names the versioned spelling: {body:?}"
    );
}

#[test]
fn discovery_document_is_served_publicly_at_the_root() {
    let port = start(
        Database::open(":memory:").unwrap(),
        HttpConfig::new(AuthConfig::basic("neo4j", "s3cret")),
    );
    // No credential (discovery is public), and even a foreign Origin on
    // the CSRF-safe GET passes the browser guard. The Host is the one a
    // client reached the server at, echoed into the advertised URL below.
    let (status, body) = exchange(
        port,
        request(
            "GET",
            "/",
            "127.0.0.1:7474",
            &[("origin", "https://browser.example")],
        )
        .as_bytes(),
    );
    assert_eq!(status, 200, "{body}");
    let doc: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        doc["query"],
        serde_json::json!("http://127.0.0.1:7474/db/{databaseName}/query/v2")
    );
    assert_eq!(doc["neo4j_edition"], serde_json::json!("community"));
    assert!(doc.get("bolt_direct").is_none());
}

/// A POST with `origin` through the shared client path; returns the
/// status and the raw response (for CORS header assertions).
fn post_with_origin(port: u16, origin: &str) -> (u16, String) {
    let body = r#"{"statement": "RETURN 1"}"#;
    let request = post_with_headers("/db/neo4j/query/v2", body, &[("origin", origin)]);
    exchange_raw(port, request.as_bytes())
}

#[test]
fn cors_allowlist_echo_preflight_and_overrides() {
    let port = start(
        Database::open(":memory:").unwrap(),
        HttpConfig::new(AuthConfig::None),
    );
    // Default allowlist: the hosted Neo4j Browser POSTs and gets its
    // Origin echoed back (so credentialed browser requests work).
    let (status, raw) = post_with_origin(port, "https://browser.neo4j.io");
    assert_eq!(status, 200, "{raw}");
    let (head, _) = split_response(raw.as_bytes());
    assert_eq!(
        header_value(head, "access-control-allow-origin"),
        Some("https://browser.neo4j.io"),
        "{head}"
    );
    // A foreign Origin: 403 and no ACAO, so the browser cannot read it.
    let (status, raw) = post_with_origin(port, "https://evil.example");
    assert_eq!(status, 403, "{raw}");
    let (head, _) = split_response(raw.as_bytes());
    assert_eq!(
        header_value(head, "access-control-allow-origin"),
        None,
        "{head}"
    );

    // An unauthenticated preflight answers 204 with the allow headers
    // and the echo — and with auth REQUIRED, proving preflights skip
    // the challenge.
    let auth_port = start(
        Database::open(":memory:").unwrap(),
        HttpConfig::new(AuthConfig::basic("neo4j", "s3cret")),
    );
    let preflight = request(
        "OPTIONS",
        "/db/neo4j/query/v2",
        LOOPBACK_HOST,
        &[("origin", "https://browser.neo4j.io")],
    );
    let (status, raw) = exchange_raw(auth_port, preflight.as_bytes());
    assert_eq!(status, 204, "{raw:?}");
    let (head, body) = split_response(raw.as_bytes());
    assert!(body.is_empty(), "the preflight is body-less: {body:?}");
    assert_eq!(
        header_value(head, "access-control-allow-origin"),
        Some("https://browser.neo4j.io"),
        "{head}"
    );
    assert_eq!(
        header_value(head, "access-control-allow-methods"),
        Some("GET, POST, OPTIONS"),
        "{head}"
    );
    assert_eq!(
        header_value(head, "access-control-allow-headers"),
        Some("Authorization, Content-Type, sentry-trace, baggage"),
        "{head}"
    );

    // A 401 carries the echo for an allowlisted Origin (the browser
    // must be able to read the challenge).
    let body = r#"{"statement": "RETURN 1"}"#;
    let request = post_with_headers(
        "/db/neo4j/query/v2",
        body,
        &[("origin", "https://browser.neo4j.io")],
    );
    let (status, raw) = exchange_raw(auth_port, request.as_bytes());
    assert_eq!(status, 401, "{raw:?}");
    let (head, _) = split_response(raw.as_bytes());
    assert_eq!(
        header_value(head, "access-control-allow-origin"),
        Some("https://browser.neo4j.io"),
        "{head}"
    );

    // `--origin https://tool.example` replaces the default list:
    // browser.neo4j.io is rejected, the override is allowed.
    let mut config = HttpConfig::new(AuthConfig::None);
    config.allowed_origins = OriginAllowlist::from_flag_values(&[OriginFlagValue::Url(
        "https://tool.example".to_string(),
    )]);
    let exact_port = start(Database::open(":memory:").unwrap(), config);
    let (status, _) = post_with_origin(exact_port, "https://browser.neo4j.io");
    assert_eq!(status, 403);
    let (status, raw) = post_with_origin(exact_port, "https://tool.example");
    assert_eq!(status, 200, "{raw}");
    let (head, _) = split_response(raw.as_bytes());
    assert_eq!(
        header_value(head, "access-control-allow-origin"),
        Some("https://tool.example"),
        "{head}"
    );

    // `--origin *` allows any Origin.
    let mut config = HttpConfig::new(AuthConfig::None);
    config.allowed_origins = OriginAllowlist::from_flag_values(&[OriginFlagValue::Wildcard]);
    let any_port = start(Database::open(":memory:").unwrap(), config);
    let (status, raw) = post_with_origin(any_port, "https://anything.example");
    assert_eq!(status, 200, "{raw}");
    let (other_head, _) = split_response(raw.as_bytes());
    assert_eq!(
        header_value(other_head, "access-control-allow-origin"),
        Some("https://anything.example"),
        "{other_head}"
    );
}

#[test]
fn a_configured_bolt_endpoint_is_advertised_in_discovery() {
    // The field's one consumer today: a config that carries a Bolt
    // endpoint advertises it (the product wiring for a future
    // `serve --bolt`).
    let mut config = HttpConfig::new(AuthConfig::None);
    config.bolt_direct = Some("bolt://127.0.0.1:7687".into());
    let port = start(Database::open(":memory:").unwrap(), config);
    let (status, body) = exchange(port, request("GET", "/", LOOPBACK_HOST, &[]).as_bytes());
    assert_eq!(status, 200, "{body}");
    let doc: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        doc["bolt_direct"],
        serde_json::json!("bolt://127.0.0.1:7687")
    );
}

#[test]
fn browser_guard_rejects_foreign_origins_and_non_json_posts() {
    let port = start(
        Database::open(":memory:").unwrap(),
        HttpConfig::new(AuthConfig::None),
    );
    let body = r#"{"statement": "RETURN 1"}"#;
    // A POST with caller-chosen extra headers (the shared `post` pins
    // the JSON content type; these cases need to vary it).
    let post_json = |extra_headers: &[(&str, &str)]| {
        post_with_headers("/db/neo4j/query/v2", body, extra_headers)
    };
    let post_text = |extra_headers: &[(&str, &str)]| {
        post_with_content_type("/db/neo4j/query/v2", "text/plain", body, extra_headers)
    };
    // A cross-origin form POST cannot set application/json: 415.
    let (status, _) = exchange(port, post_text(&[]).as_bytes());
    assert_eq!(status, 415);
    // A present foreign Origin: 403.
    let (status, _) = exchange(
        port,
        post_json(&[("origin", "https://evil.example")]).as_bytes(),
    );
    assert_eq!(status, 403);
    // A loopback Origin passes.
    let (status, body) = exchange(
        port,
        post_json(&[("origin", "http://localhost:3000")]).as_bytes(),
    );
    assert_eq!(status, 200);
    let body: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["data"]["values"], serde_json::json!([[1]]));
}

#[test]
fn foreign_origin_and_dns_rebinding_deletes_are_rejected_under_auth_none() {
    let port = start(
        Database::open(":memory:").unwrap(),
        HttpConfig::new(AuthConfig::None),
    );
    let (tx_path, _) = open_tx(port);

    // A foreign Origin on the rollback is refused (the guard now covers
    // every state-changing method, ADR-0455).
    let foreign = request(
        "DELETE",
        &tx_path,
        LOOPBACK_HOST,
        &[("origin", "https://evil.example")],
    );
    let (status, body) = exchange(port, foreign.as_bytes());
    assert_eq!(status, 403, "{body}");

    // The transaction is still open: the refused rollback never ran.
    let (status, body) = exchange_json(
        port,
        post(&tx_path, r#"{"statement":"RETURN 1 AS n"}"#, None),
    );
    assert_eq!(status, 200, "{body}");

    // The DNS-rebinding shape: the browser addressed the attacker's
    // hostname (rebound to loopback, so `Host` names it) and the page's
    // Origin is that same foreign origin. The Origin verdict refuses it.
    let rebound = request(
        "DELETE",
        &tx_path,
        "evil.example:7474",
        &[("origin", "http://evil.example:7474")],
    );
    let (status, body) = exchange(port, rebound.as_bytes());
    assert_eq!(status, 403, "{body}");

    // An allowlisted (loopback) Origin is accepted. The body-less DELETE
    // carries no content type and is not held to POST's JSON rule.
    let allowed = request(
        "DELETE",
        &tx_path,
        LOOPBACK_HOST,
        &[("origin", "http://localhost:3000")],
    );
    let (status, body) = exchange(port, allowed.as_bytes());
    assert_eq!(status, 202, "{body}");
}

#[test]
fn unauthenticated_over_cap_post_is_401_before_the_body_is_read() {
    let port = start(
        Database::open(":memory:").unwrap(),
        HttpConfig::new(AuthConfig::basic("neo4j", "s3cret")),
    );
    // Headers only: an over-cap Content-Length and no body bytes at all.
    // A reader that buffered the body before the auth check would block
    // here (or 400/413 on the cap); the prompt 401 proves auth ran first.
    let request = post_head_only("/db/neo4j/query/v2", DEFAULT_MAX_BODY_BYTES + 1);
    let started = std::time::Instant::now();
    let (status, raw) = exchange_raw(port, request.as_bytes());
    assert_eq!(status, 401, "{raw:?}");
    let (head, _) = split_response(raw.as_bytes());
    assert!(
        header_value(head, "www-authenticate").is_some_and(|value| value.contains("Basic realm=")),
        "{head}"
    );
    // The ADR-0139 failed-auth delay applies to the 401.
    assert!(
        started.elapsed() >= crate::http::FAILED_AUTH_DELAY,
        "401 without the failed-auth delay"
    );
}

#[test]
fn unauthenticated_request_is_rejected() {
    let port = start(
        Database::open(":memory:").unwrap(),
        HttpConfig::new(AuthConfig::basic("neo4j", "s3cret")),
    );
    // No header at all.
    let (status, body) =
        exchange_json(port, post_db("neo4j", r#"{"statement": "RETURN 1"}"#, None));
    assert_eq!(status, 401);
    assert_eq!(body["errors"][0]["code"], "42NFF");
    // A wrong credential.
    let (status, _) = exchange_json(
        port,
        post_db(
            "neo4j",
            r#"{"statement": "RETURN 1"}"#,
            Some("Basic bmVvNGo6d3Jvbmc="),
        ),
    );
    assert_eq!(status, 401);
    // The right credential passes. `bmVvNGo6czNjcmV0` = "neo4j:s3cret".
    let (status, body) = exchange_json(
        port,
        post_db(
            "neo4j",
            r#"{"statement": "RETURN 1 AS n"}"#,
            Some("Basic bmVvNGo6czNjcmV0"),
        ),
    );
    assert_eq!(status, 200);
    assert_eq!(body["data"]["values"], serde_json::json!([[1]]));
}

#[test]
fn no_accept_encoding_keeps_the_query_body_identity() {
    let port = start(
        Database::open(":memory:").unwrap(),
        HttpConfig::new(AuthConfig::None),
    );
    let (status, raw) = exchange_bytes(port, post_db("neo4j", BIG_BODY, None).as_bytes());
    assert_eq!(status, 200, "{raw:?}");
    let (head, body) = split_response(&raw);
    // A client that sent no preference gets the identity bytes…
    assert_eq!(header_value(head, "content-encoding"), None, "{head}");
    assert_eq!(one_shot_content_length(head), body.len(), "{head}");
    assert!(body.len() >= content_encoding::GZIP_THRESHOLD_BYTES);
    // … and still keys shared caches on the encoding it omitted.
    assert_eq!(
        header_value(head, "vary"),
        Some("Accept-Encoding"),
        "{head}"
    );
    let envelope: serde_json::Value = serde_json::from_slice(body).unwrap();
    assert_eq!(
        envelope["data"]["values"][0][0].as_array().unwrap().len(),
        500
    );
}

#[test]
fn gzip_q_zero_keeps_identity() {
    let port = start(
        Database::open(":memory:").unwrap(),
        HttpConfig::new(AuthConfig::None),
    );
    let request = post_db_with_headers("neo4j", BIG_BODY, &[("accept-encoding", "gzip;q=0")]);
    let (status, raw) = exchange_bytes(port, request.as_bytes());
    assert_eq!(status, 200, "{raw:?}");
    let (head, body) = split_response(&raw);
    assert_eq!(
        header_value(head, "content-encoding"),
        None,
        "an explicit q=0 disables gzip even when a wildcard allows it: {head}"
    );
    assert_eq!(header_value(head, "vary"), Some("Accept-Encoding"));
    serde_json::from_slice::<serde_json::Value>(body).unwrap();
}

#[test]
fn a_body_below_the_threshold_stays_identity_despite_gzip() {
    let port = start(
        Database::open(":memory:").unwrap(),
        HttpConfig::new(AuthConfig::None),
    );
    let body = r#"{"statement": "RETURN 1 AS n"}"#;
    let request = post_db_with_headers("neo4j", body, &[("accept-encoding", "gzip")]);
    let (status, raw) = exchange_bytes(port, request.as_bytes());
    assert_eq!(status, 200, "{raw:?}");
    let (head, payload) = split_response(&raw);
    assert!(
        payload.len() < content_encoding::GZIP_THRESHOLD_BYTES,
        "the fixture is a sub-threshold reply"
    );
    assert_eq!(
        header_value(head, "content-encoding"),
        None,
        "a sub-threshold body stays identity: {head}"
    );
    assert_eq!(one_shot_content_length(head), payload.len());
    assert_eq!(header_value(head, "vary"), Some("Accept-Encoding"));
}

#[test]
fn accept_encoding_split_across_header_lines_still_negotiates_gzip() {
    // RFC 9110 §5.2: a list header split across lines is one list — a
    // client sending `br` and `gzip` separately negotiates gzip, not
    // identity (the negotiation reads the joined value).
    let port = start(
        Database::open(":memory:").unwrap(),
        HttpConfig::new(AuthConfig::None),
    );
    let request = post_db_with_headers(
        "neo4j",
        BIG_BODY,
        &[("accept-encoding", "br"), ("accept-encoding", "gzip")],
    );
    let (status, raw) = exchange_bytes(port, request.as_bytes());
    assert_eq!(status, 200, "{raw:?}");
    let (head, _) = split_response(&raw);
    assert_eq!(
        header_value(head, "content-encoding"),
        Some("gzip"),
        "{head}"
    );
}

#[test]
fn gzip_roundtrips_to_the_exact_identity_envelope() {
    let port = start(
        Database::open(":memory:").unwrap(),
        HttpConfig::new(AuthConfig::None),
    );
    // The same statement answers identical envelopes, so the identity
    // bytes are the control the gzip payload must inflate to.
    let (status, identity_raw) = exchange_bytes(port, post_db("neo4j", BIG_BODY, None).as_bytes());
    assert_eq!(status, 200, "{identity_raw:?}");
    let (identity_head, identity_body) = split_response(&identity_raw);
    assert_eq!(header_value(identity_head, "content-encoding"), None);

    let request = post_db_with_headers("neo4j", BIG_BODY, &[("accept-encoding", "gzip")]);
    let (status, raw) = exchange_bytes(port, request.as_bytes());
    assert_eq!(status, 200, "{raw:?}");
    let (head, compressed) = split_response(&raw);
    assert_eq!(
        header_value(head, "content-encoding"),
        Some("gzip"),
        "{head}"
    );
    assert_eq!(header_value(head, "vary"), Some("Accept-Encoding"));
    // The declared length is the compressed body's length, not the JSON
    // envelope's.
    assert_eq!(one_shot_content_length(head), compressed.len(), "{head}");
    assert!(
        compressed.len() < identity_body.len(),
        "the gzip body must shrink the envelope"
    );
    // LABS-1120: the timing members carry per-request numbers, so the
    // byte-for-byte roundtrip holds for the deterministic prefix — the
    // tail from `"timings"` on is cut from both sides (the one helper,
    // `streaming::strip_timing_tail`).
    assert_eq!(
        streaming::strip_timing_tail(&gunzip(compressed)),
        streaming::strip_timing_tail(identity_body),
        "the gzip payload inflates to the exact identity envelope"
    );
}

#[test]
fn query_route_errors_carry_vary_and_stay_identity() {
    let port = start(
        Database::open(":memory:").unwrap(),
        HttpConfig::new(AuthConfig::None),
    );
    // An unknown database is a small 404 error envelope: it is on the
    // seam, so it advertises `Vary`, but never gzips below the bar.
    let request = post_db_with_headers(
        "movies",
        r#"{"statement": "RETURN 1"}"#,
        &[("accept-encoding", "gzip")],
    );
    let (status, raw) = exchange_bytes(port, request.as_bytes());
    assert_eq!(status, 404, "{raw:?}");
    let (head, body) = split_response(&raw);
    assert_eq!(header_value(head, "content-encoding"), None, "{head}");
    assert_eq!(header_value(head, "vary"), Some("Accept-Encoding"));
    let envelope: serde_json::Value = serde_json::from_slice(body).unwrap();
    assert_eq!(envelope["errors"][0]["code"], "42N00");
}

#[test]
fn discovery_and_preflight_are_outside_the_compression_seam() {
    let port = start(
        Database::open(":memory:").unwrap(),
        HttpConfig::new(AuthConfig::None),
    );
    // The public discovery document is never gzip and carries no
    // `Vary`, even when the client advertises gzip (ADR-0385 §2).
    let discovery = request("GET", "/", LOOPBACK_HOST, &[("accept-encoding", "gzip")]);
    let (status, raw) = exchange_bytes(port, discovery.as_bytes());
    assert_eq!(status, 200, "{raw:?}");
    let (head, _) = split_response(&raw);
    assert_eq!(header_value(head, "content-encoding"), None, "{head}");
    assert_eq!(header_value(head, "vary"), None, "{head}");
    // The body-less 204 preflight is outside the seam too.
    let preflight = request(
        "OPTIONS",
        "/db/neo4j/query/v2",
        LOOPBACK_HOST,
        &[
            ("origin", "https://browser.neo4j.io"),
            ("accept-encoding", "gzip"),
        ],
    );
    let (status, raw) = exchange_bytes(port, preflight.as_bytes());
    assert_eq!(status, 204, "{raw:?}");
    let (head, _) = split_response(&raw);
    assert_eq!(header_value(head, "content-encoding"), None, "{head}");
    assert_eq!(header_value(head, "vary"), None, "{head}");
}

#[test]
fn a_tx_run_replies_expires_that_advances_with_activity_on_the_wire() {
    // ADR-0435 §1 end to end: the deadline a tx reply renders is the
    // entry's activity stamp plus the configured timeout, refreshed after
    // each statement — pinned through the real server so the wire
    // rendering (RFC 3339, milliseconds) is what a client sees.
    let port = start(
        Database::open(":memory:").unwrap(),
        HttpConfig::new(AuthConfig::None),
    );
    let (tx_path, id) = open_tx(port);

    // A keep-alive run, then a statement that takes measurable time: the
    // slow run's deadline is the refreshed stamp plus the same 60 s
    // default window, strictly after the earlier rendering.
    let (status, keep_alive) = exchange_json(port, post(&tx_path, "", None));
    assert_eq!(status, 200, "{keep_alive}");
    assert_eq!(tx_id_from(&keep_alive), id);
    let keep_alive_deadline = parse_expires(&keep_alive);
    let (status, run) = exchange_json(port, post(&tx_path, SLOW_STATEMENT, None));
    assert_eq!(status, 200, "{run}");
    assert_eq!(
        tx_id_from(&run),
        id,
        "the run reply names the opened transaction"
    );
    let second = parse_expires(&run);
    assert!(
        second > keep_alive_deadline,
        "the deadline advanced: {keep_alive_deadline} -> {second}"
    );
    let remaining = second - chrono::Utc::now();
    assert!(
        (59_000..=60_100).contains(&remaining.num_milliseconds()),
        "the wire deadline is the configured 60 s window: {remaining}"
    );

    // Close through the wire so the transaction's snapshot pin is released.
    let close = request("DELETE", &tx_path, LOOPBACK_HOST, &[]);
    let (status, _) = exchange(port, close.as_bytes());
    assert_eq!(status, 202);
}
