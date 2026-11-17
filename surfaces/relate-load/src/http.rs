use std::io::Read;

use base64::Engine;
use serde_json::Value as Json;

use crate::creds::{complete_aura_uri, uri_authority, Credentials};
use crate::response::{parse_v2_response, truncate_for_error, MAX_RESPONSE_BYTES};
use crate::LoadError;

const BOLT_PORT: u16 = 7687;
const HTTP_DEFAULT: u16 = 7474;
const QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Query API v2 URL for `uri` + `database` (ADR-0080).
///
/// Error text is always fixed and never interpolates the raw `NEO4J_URI`: a
/// malformed URI can carry credentials (literal or percent-encoded userinfo),
/// so validation failures say what is wrong without echoing the input
/// (ADR-0122, validate-don't-sanitize).
pub fn query_api_url(uri: &str, database: &str) -> Result<String, LoadError> {
    let uri = complete_aura_uri(uri.trim());
    reject_uri_userinfo(&uri)?;
    let Some(auth) = uri_authority(&uri) else {
        return Err(LoadError::Env("NEO4J_URI missing host".to_string()));
    };
    // Validate before anything else consumes, echoes, or persists the host:
    // an encoded-userinfo probe (`http://user%3Asecretpw%40host`) parses the
    // credential as the host. Refuse it here rather than echo it in an error
    // or persist it via `identity_source` (ADR-0122).
    validate_host(&auth.host)?;
    let host = auth.host.as_str();
    let origin = if host.ends_with(".databases.neo4j.io") {
        format!("https://{host}")
    } else {
        let Some(scheme) = auth.scheme.as_deref() else {
            return Err(LoadError::Env("NEO4J_URI missing scheme".to_string()));
        };
        let port = auth.port;
        match scheme {
            "http" | "https" => {
                let authority = match port {
                    Some(p) => format!("{host}:{p}"),
                    None => host.to_string(),
                };
                format!("{scheme}://{authority}")
            }
            "neo4j" | "bolt" => match port {
                None | Some(BOLT_PORT) => format!("http://{host}:{HTTP_DEFAULT}"),
                Some(p) => {
                    return Err(LoadError::Env(format!(
                        "NEO4J_URI bolt port {p} has no Query API mapping (7687 → 7474); use an explicit http(s) NEO4J_URI"
                    )));
                }
            },
            "neo4j+s" | "neo4j+ssc" | "bolt+s" | "bolt+ssc" => {
                let authority = match port {
                    Some(p) if p != BOLT_PORT => format!("{host}:{p}"),
                    _ => host.to_string(),
                };
                format!("https://{authority}")
            }
            _ => {
                // Fixed text (ADR-0122): `split_scheme` takes everything before
                // the first `://`, so a URI like `user:pw@host://x` (or its
                // percent-encoded twin `user%3Apw%40host://x`) yields a "scheme"
                // token carrying userinfo. No scheme token is ever echoed.
                return Err(LoadError::Env(
                    "unsupported NEO4J_URI scheme (use neo4j/bolt/http/https or +s/+ssc)"
                        .to_string(),
                ));
            }
        }
    };
    Ok(format!("{origin}/db/{database}/query/v2"))
}

/// Refuse any `NEO4J_URI` carrying userinfo (ADR-0122, refuse at the
/// boundary). Pull credentials come from `NEO4J_USERNAME`/`NEO4J_PASSWORD`;
/// userinfo in the URI is never legitimate. The check runs on the RAW URI
/// after the scheme is split off but before `split_authority` truncates at
/// the first `/`: a password containing `/` (`http://user:p/ss@host:7687`)
/// would otherwise cut the authority to `user:p`, silently parsing the
/// userinfo username as the host and dropping the real host. The rule is
/// deliberately simple — an `@` anywhere in the post-scheme remainder
/// refuses the URI — because pull URIs are origins (`scheme://host[:port]`)
/// and `query_api_url` rebuilds the path itself. The error is fixed text and
/// never echoes the URI.
fn reject_uri_userinfo(uri: &str) -> Result<(), LoadError> {
    let remainder = match uri.split_once("://") {
        Some((_, rest)) => rest,
        None => uri,
    };
    if remainder.contains('@') {
        return Err(LoadError::Env(
            "NEO4J_URI must not contain userinfo; pass credentials via NEO4J_USERNAME/NEO4J_PASSWORD"
                .to_string(),
        ));
    }
    Ok(())
}

/// Refuse hosts outside the DNS / IP-literal charset (ADR-0122,
/// validate-don't-sanitize). A real hostname never contains `%` or `@`, so
/// percent-encoded userinfo that `uri_authority` parses into the host slot —
/// `user%3Asecretpw%40host`, double-encoded `%2540` probes, and any other
/// encoding trick — is rejected here instead of sanitized. The error is
/// fixed text and names neither the URI nor the host.
fn validate_host(host: &str) -> Result<(), LoadError> {
    if is_valid_host(host) {
        return Ok(());
    }
    Err(LoadError::Env(
        "NEO4J_URI has an invalid host (expected a DNS name or IP literal)".to_string(),
    ))
}

/// DNS name, IPv4 address, or bracketed IPv6 literal. Anything containing
/// `%`, `@`, or a character outside those grammars is not a host.
fn is_valid_host(host: &str) -> bool {
    if let Some(inner) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        // Bracketed IPv6 literal: hex digits and colons, at least one colon.
        return inner.contains(':') && inner.chars().all(|c| c.is_ascii_hexdigit() || c == ':');
    }
    !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
}

/// Enforce the plaintext Basic-auth policy (ADR-0122). `http://` to a
/// non-loopback host is refused unless `allow_plaintext` is set; the
/// opt-in downgrades the refusal to a warning. Loopback is always allowed
/// silently, and `https://` is never refused.
fn ensure_cleartext_basic_allowed(url: &str, allow_plaintext: bool) -> Result<(), LoadError> {
    let Some(auth) = uri_authority(url) else {
        return Ok(());
    };
    if auth.scheme.as_deref() != Some("http") {
        return Ok(());
    }
    if is_loopback_host(&auth.host) {
        return Ok(());
    }
    if !allow_plaintext {
        return Err(LoadError::Env(format!(
            "refusing to send Basic auth credentials in the clear: Query API URL {url} is http:// to a non-loopback host; pass --allow-plaintext to accept the risk, or use an https:// NEO4J_URI"
        )));
    }
    eprintln!(
        "warning: Query API URL {url} is http://; Basic auth credentials will be sent in the clear (--allow-plaintext)"
    );
    Ok(())
}

/// Loopback hosts for the plaintext Basic-auth policy (ADR-0122). This
/// mirrors `is_loopback_host` in relate-cli's telemetry/handle.rs in
/// shape only; the policies intentionally differ — pull accepts the whole
/// `127.0.0.0/8` range (ADR-0122) while the telemetry endpoint override is
/// pinned to exactly `127.0.0.1` (ADR-0263).
fn is_loopback_host(host: &str) -> bool {
    let h = host.trim_matches(|c| c == '[' || c == ']');
    if h.eq_ignore_ascii_case("localhost") || h == "::1" {
        return true;
    }
    // Any IPv4 address in 127.0.0.0/8 is loopback. The std parser is strict:
    // it rejects leading zeros, `+`, octal/hex, and short forms, so
    // `0127.0.0.1` and `127.0.0` never reach the is_loopback check.
    match h.parse::<std::net::Ipv4Addr>() {
        Ok(addr) => addr.is_loopback(),
        Err(_) => false,
    }
}

/// Body deadline policy for the shared POST prelude.
enum BodyDeadline {
    /// Whole-body deadline (buffered queries).
    Global,
    /// Connect-only: a whole-database pull's body streams for minutes and
    /// ureq's recv_body budget is whole-body, not per-read, so the
    /// streaming pull takes no body deadline — hang protection is the
    /// caller's timebox (ADR-0158).
    ConnectOnly,
}

/// The response representation a Query API v2 request asks for, as the
/// `Accept` header the shared [`post_query`] prelude sends (ADR-0181).
#[derive(Clone, Copy)]
enum ResultFormat {
    /// `Accept: application/json`: the plain `data.values` JSON every caller
    /// except the pull path consumes (the graph-examples reference client and
    /// the docker/admin probes).
    PlainJson,
    /// The typed JSON media-type ladder (ADR-0181): property values arrive
    /// as `{"$type": …, "_value": …}` envelopes so the pull can store
    /// temporal, spatial, UUID and vector values as their engine types.
    /// Only the pull path ([`run_query_streaming_typed`]) requests it.
    TypedJson,
}

impl ResultFormat {
    fn accept_header(self) -> &'static str {
        match self {
            ResultFormat::PlainJson => "application/json",
            ResultFormat::TypedJson => TYPED_ACCEPT_HEADER,
        }
    }
}

/// The typed Query API media-type ladder (ADR-0181): `.v1.2` on Neo4j
/// 2026.09, `.v1.0` on 5.26, the unversioned type last. `application/json`
/// is deliberately absent — a source without typed JSON answers 406 and the
/// pull fails loudly instead of silently loading untyped strings (LABS-186).
pub const TYPED_ACCEPT_HEADER: &str = "application/vnd.neo4j.query.v1.2;q=1.0, application/vnd.neo4j.query.v1.1;q=0.9, application/vnd.neo4j.query.v1.0;q=0.8, application/vnd.neo4j.query;q=0.7";

/// The typed Query API v2 **request** media type: parameter values sent as
/// `{"$type": …, "_value": …}` envelopes, the inverse of the typed response
/// ladder above. The push client uses v1.2 for types added after the
/// unversioned v1.0 dialect, including vectors and UUIDs (ADR-0401).
pub const TYPED_CONTENT_TYPE: &str = "application/vnd.neo4j.query.v1.2";

/// The request `Content-Type` a Query API v2 POST sends: plain JSON
/// parameters (every existing caller) or the typed parameter envelopes (the
/// push write path, which sends engine values as their `$type` tags so the
/// remote keeps the Cypher type — ADR-0401).
#[derive(Clone, Copy)]
enum ContentFormat {
    /// `Content-Type: application/json`.
    PlainJson,
    /// The v1.2 typed request form used by push (ADR-0401).
    TypedJson,
}

impl ContentFormat {
    fn content_type(self) -> &'static str {
        match self {
            ContentFormat::PlainJson => "application/json",
            ContentFormat::TypedJson => TYPED_CONTENT_TYPE,
        }
    }
}

/// The request prelude every Query API v2 POST shares: URL + cleartext
/// policy, body serialization, auth POST, non-2xx mapped into
/// [`LoadError::Http`]. On 2xx, returns the URL, the serialized request
/// body's byte count, and the body reader. The `Accept` header comes from
/// `format` (ADR-0181) — the pull path asks for typed JSON, every other
/// caller for plain JSON — and the `Content-Type` from `content`: plain JSON
/// parameters everywhere except the push write path (ADR-0401).
fn post_query(
    creds: &Credentials,
    cypher: &str,
    parameters: &serde_json::Map<String, Json>,
    allow_plaintext: bool,
    content: ContentFormat,
    format: ResultFormat,
    deadline: BodyDeadline,
) -> Result<(String, u64, impl Read), LoadError> {
    let url = query_api_url(&creds.uri, &creds.database)?;
    ensure_cleartext_basic_allowed(&url, allow_plaintext)?;
    let body = serde_json::to_string(&serde_json::json!({
        "statement": cypher,
        "parameters": parameters,
    }))
    .map_err(|e| LoadError::Io(format!("serializing request body for {url} failed: {e}")))?;
    let body_bytes = body.len() as u64;
    let auth = basic_auth(&creds.username, &creds.password);
    let config = ureq::post(&url)
        .header("Authorization", &auth)
        .header("Content-Type", content.content_type())
        .header("Accept", format.accept_header())
        .config()
        .http_status_as_error(false);
    let config = match deadline {
        BodyDeadline::Global => config.timeout_global(Some(QUERY_TIMEOUT)),
        BodyDeadline::ConnectOnly => config.timeout_connect(Some(QUERY_TIMEOUT)),
    };
    let resp = config
        .build()
        .send(&body)
        .map_err(|e| LoadError::Io(format!("HTTP request to {url} failed: {e}")))?;
    let status = resp.status().as_u16();
    let reader = resp.into_body().into_reader();
    if !(200..300).contains(&status) {
        let text = read_response_body(reader, &url)?;
        return Err(LoadError::Http {
            status,
            reply: truncate_for_error(&text),
        });
    }
    Ok((url, body_bytes, reader))
}

/// The one streaming request path both [`run_query_streaming`] and
/// [`run_query_streaming_typed`] call: POST through the shared [`post_query`]
/// prelude with no body deadline (ADR-0158), then feed `data.values` rows to
/// `on_rows` as they arrive off the body — the response is never buffered
/// whole, so a whole-database pull is not bounded by the response cap.
/// `format` picks the `Accept` header (ADR-0181) — plain JSON for the
/// streaming entry point, the typed ladder for the pull path — while the
/// prelude and row scanner are shared, so the two paths cannot drift into
/// separate request builders.
fn stream_query(
    format: ResultFormat,
    creds: &Credentials,
    cypher: &str,
    parameters: &serde_json::Map<String, Json>,
    allow_plaintext: bool,
    on_rows: &mut dyn FnMut(&[Json]) -> Result<(), LoadError>,
) -> Result<(), LoadError> {
    let (url, _body_bytes, reader) = post_query(
        creds,
        cypher,
        parameters,
        allow_plaintext,
        ContentFormat::PlainJson,
        format,
        BodyDeadline::ConnectOnly,
    )?;
    crate::response::stream_v2_rows(reader, &url, on_rows)
}

/// Stream one Query API v2 query: POST, then feed `data.values` rows to
/// `on_rows` as they arrive off the body — the response is never buffered
/// whole (ADR-0158), so a whole-database pull is not bounded by the
/// response cap. Asks for plain JSON (`Accept: application/json`). Thin
/// wrapper over the shared streaming body.
pub fn run_query_streaming(
    creds: &Credentials,
    cypher: &str,
    parameters: &serde_json::Map<String, Json>,
    allow_plaintext: bool,
    on_rows: &mut dyn FnMut(&[Json]) -> Result<(), LoadError>,
) -> Result<(), LoadError> {
    stream_query(
        ResultFormat::PlainJson,
        creds,
        cypher,
        parameters,
        allow_plaintext,
        on_rows,
    )
}

/// Stream one Query API v2 query asking for the typed JSON wire format
/// (ADR-0181): `load_streaming`'s entry point, and the only caller that
/// requests it. Thin wrapper over [`stream_query`], the same body
/// `run_query_streaming` calls; only the `Accept` header differs, so the two
/// paths cannot drift into separate request builders.
pub(crate) fn run_query_streaming_typed(
    creds: &Credentials,
    cypher: &str,
    parameters: &serde_json::Map<String, Json>,
    allow_plaintext: bool,
    on_rows: &mut dyn FnMut(&[Json]) -> Result<(), LoadError>,
) -> Result<(), LoadError> {
    stream_query(
        ResultFormat::TypedJson,
        creds,
        cypher,
        parameters,
        allow_plaintext,
        on_rows,
    )
}

pub fn run_query(
    creds: &Credentials,
    cypher: &str,
    parameters: &serde_json::Map<String, Json>,
    allow_plaintext: bool,
) -> Result<Json, LoadError> {
    let (url, _body_bytes, reader) = post_query(
        creds,
        cypher,
        parameters,
        allow_plaintext,
        ContentFormat::PlainJson,
        ResultFormat::PlainJson,
        BodyDeadline::Global,
    )?;
    let text = read_response_body(reader, &url)?;
    parse_v2_response(&text)
}

/// Run one buffered Query API v2 write whose **parameters** are typed
/// envelopes (`Content-Type: application/vnd.neo4j.query.v1.2`, ADR-0401): the
/// push write path's entry point, and the only caller that sends typed
/// request bodies. Responses stay plain JSON (the write's own row data is
/// ignored; errors surface through the shared non-2xx mapping). Returns the
/// parsed response and the serialized request body's byte count, the number
/// the push report's byte total sums.
pub fn run_query_typed_params(
    creds: &Credentials,
    cypher: &str,
    parameters: &serde_json::Map<String, Json>,
    allow_plaintext: bool,
) -> Result<(Json, u64), LoadError> {
    let (url, body_bytes, reader) = post_query(
        creds,
        cypher,
        parameters,
        allow_plaintext,
        ContentFormat::TypedJson,
        ResultFormat::PlainJson,
        BodyDeadline::Global,
    )?;
    let text = read_response_body(reader, &url)?;
    let json = parse_v2_response(&text)?;
    Ok((json, body_bytes))
}

fn read_response_body(reader: impl Read, url: &str) -> Result<String, LoadError> {
    let mut limited = reader.take(MAX_RESPONSE_BYTES.saturating_add(1));
    let mut text = String::new();
    limited
        .read_to_string(&mut text)
        .map_err(|e| LoadError::Io(format!("reading response body from {url} failed: {e}")))?;
    if text.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(LoadError::Io(format!(
            "response body from {url} exceeded {MAX_RESPONSE_BYTES} bytes"
        )));
    }
    Ok(text)
}

/// Normalized identity source: Query API origin + database (no credentials).
pub fn identity_source(creds: &Credentials) -> Result<String, LoadError> {
    query_api_url(&creds.uri, &creds.database)
}

fn basic_auth(username: &str, password: &str) -> String {
    let credentials = format!("{username}:{password}");
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(credentials)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use relate_test_support::loopback::{
        header_values, spawn_single_response, spawn_single_response_capturing,
    };

    #[test]
    fn aura_query_url_strips_bolt_port() {
        assert_eq!(
            query_api_url("neo4j+s://42f122c0.databases.neo4j.io", "neo4j").unwrap(),
            "https://42f122c0.databases.neo4j.io/db/neo4j/query/v2"
        );
        assert_eq!(
            query_api_url("neo4j+s://42f122c0.databases.neo4j.io:7687", "neo4j").unwrap(),
            "https://42f122c0.databases.neo4j.io/db/neo4j/query/v2"
        );
        assert_eq!(
            query_api_url("neo4j+s://42f122c0.databases.neo4j", "movies").unwrap(),
            "https://42f122c0.databases.neo4j.io/db/movies/query/v2"
        );
    }

    #[test]
    fn local_neo4j_maps_7687_to_7474() {
        assert_eq!(
            query_api_url("neo4j://localhost:7687", "neo4j").unwrap(),
            "http://localhost:7474/db/neo4j/query/v2"
        );
        assert_eq!(
            query_api_url("neo4j://127.0.0.1", "neo4j").unwrap(),
            "http://127.0.0.1:7474/db/neo4j/query/v2"
        );
    }

    #[test]
    fn non_default_bolt_port_is_error() {
        let err = query_api_url("neo4j://host:9999", "neo4j").unwrap_err();
        assert!(matches!(err, LoadError::Env(_)));
        assert!(err.to_string().contains("7687"));
    }

    #[test]
    fn loopback_hosts_are_recognized() {
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("LOCALHOST"));
        assert!(is_loopback_host("::1"));
        assert!(is_loopback_host("[::1]"));
        // The whole 127.0.0.0/8 range is loopback.
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("127.0.0.2"));
        assert!(is_loopback_host("127.255.255.254"));
        assert!(!is_loopback_host("db.example.com"));
        assert!(!is_loopback_host("128.0.0.1"));
        // Non-numeric and short forms are not loopback.
        assert!(!is_loopback_host("127"));
        assert!(!is_loopback_host("127.0.0"));
        assert!(!is_loopback_host("127.0.0.256"));
        // Strict parsing: leading zeros, `+`, hex, and trailing junk are not
        // loopback (octal-IP SSRF shapes).
        assert!(!is_loopback_host("0127.0.0.1"));
        assert!(!is_loopback_host("+127.0.0.1"));
        assert!(!is_loopback_host("127.0.0.01"));
        assert!(!is_loopback_host("127.0.0.1.evil.com"));
        assert!(!is_loopback_host("0x7f.0.0.1"));
    }

    #[test]
    fn non_loopback_http_without_opt_in_is_refused() {
        let url = query_api_url("http://db.example.com:7474", "neo4j").unwrap();
        let err = ensure_cleartext_basic_allowed(&url, false).unwrap_err();
        assert!(matches!(err, LoadError::Env(_)));
        let msg = err.to_string();
        assert!(msg.contains("--allow-plaintext"), "{msg}");
        assert!(msg.contains("db.example.com"), "{msg}");
        assert!(msg.contains("http://"), "{msg}");
    }

    #[test]
    fn non_loopback_http_with_opt_in_is_allowed() {
        let url = query_api_url("http://db.example.com:7474", "neo4j").unwrap();
        ensure_cleartext_basic_allowed(&url, true).unwrap();
    }

    #[test]
    fn loopback_http_is_allowed_without_opt_in() {
        let url = query_api_url("http://127.0.0.1:7474", "neo4j").unwrap();
        ensure_cleartext_basic_allowed(&url, false).unwrap();
    }

    #[test]
    fn https_is_never_refused() {
        let url = query_api_url("https://db.example.com:7473", "neo4j").unwrap();
        ensure_cleartext_basic_allowed(&url, false).unwrap();
    }

    #[test]
    fn http_origin_is_query_api() {
        assert_eq!(
            query_api_url("http://127.0.0.1:9", "neo4j").unwrap(),
            "http://127.0.0.1:9/db/neo4j/query/v2"
        );
    }

    #[test]
    fn userinfo_uri_is_rejected_without_echo() {
        // Any `@` in the post-scheme remainder refuses the URI outright:
        // pull credentials come from NEO4J_USERNAME/NEO4J_PASSWORD, so
        // userinfo in NEO4J_URI is never legitimate (ADR-0122). The slash-in-
        // password shapes are the attempt-6 hole: without the raw-URI check,
        // `split_authority` cuts at the `/` and silently parses the userinfo
        // username as the host.
        for uri in [
            "http://user:secretpw@host:7687",
            "neo4j://user:secretpw@host:7687",
            "http://user@host",
            "user:secretpw@host:7687",
            "http://user:p/ss@host:7687",
            "neo4j://user:p/ss@host:7687",
            "user:p/ss@host:7687",
            "neo4j://user:secretpw@",
        ] {
            let err = query_api_url(uri, "neo4j").unwrap_err();
            assert!(matches!(err, LoadError::Env(_)), "uri {uri:?}");
            let msg = err.to_string();
            assert_eq!(
                msg,
                "NEO4J_URI must not contain userinfo; pass credentials via NEO4J_USERNAME/NEO4J_PASSWORD",
                "uri {uri:?}"
            );
            assert!(!msg.contains("secretpw"), "credential leaked: {msg}");
            assert!(!msg.contains("p/ss"), "credential leaked: {msg}");
            assert!(!msg.contains(uri), "raw URI leaked: {msg}");
        }
    }

    #[test]
    fn missing_host_error_is_fixed_text() {
        // No `@`, so this is not userinfo: empty authority after the scheme.
        let err = query_api_url("neo4j://", "neo4j").unwrap_err();
        assert!(matches!(err, LoadError::Env(_)));
        assert_eq!(err.to_string(), "NEO4J_URI missing host");
    }

    #[test]
    fn missing_scheme_error_is_fixed_text_with_encoded_userinfo() {
        // `@` percent-encoded, no `://`: `user:secretpw%40host` splits into
        // host `user` (valid charset) with no scheme, so the fixed
        // missing-scheme text applies; the raw URI must not appear.
        let err = query_api_url("user:secretpw%40host", "neo4j").unwrap_err();
        assert!(matches!(err, LoadError::Env(_)));
        let msg = err.to_string();
        assert_eq!(msg, "NEO4J_URI missing scheme");
        assert!(!msg.contains("secretpw"), "credential leaked: {msg}");
        assert!(!msg.contains("%40"), "encoded credential leaked: {msg}");
    }

    #[test]
    fn encoded_userinfo_parsed_as_host_is_rejected_without_echo() {
        // No literal `@` in these URIs, so the userinfo gate does not fire;
        // they parse the encoded credential as the HOST. The host-charset
        // gate rejects them with fixed text: no credential, no raw URI.
        for uri in [
            "http://user%3Asecretpw%40host",
            "https://user%3Asecretpw%40host",
            "neo4j://user%3Asecretpw%40host:7687",
            // Double-encoded probes.
            "http://user%3Asecretpw%2540host",
            "http://user%253Asecretpw%2540host",
            // No-scheme shape: host slot still carries the `%`.
            "user%3Asecretpw%40host",
        ] {
            let err = query_api_url(uri, "neo4j").unwrap_err();
            assert!(matches!(err, LoadError::Env(_)), "uri {uri:?}");
            let msg = err.to_string();
            assert!(msg.contains("invalid host"), "uri {uri:?}: {msg}");
            assert!(!msg.contains("secretpw"), "credential leaked: {msg}");
            assert!(!msg.contains('%'), "encoded credential leaked: {msg}");
            assert!(!msg.contains(uri), "raw URI leaked: {msg}");
        }
    }

    #[test]
    fn identity_source_rejects_encoded_userinfo_uri() {
        // The identity source is persisted to the on-disk pull-identity map;
        // an encoded-userinfo URI must error instead of being stored.
        let creds = Credentials {
            uri: "http://user%3Asecretpw%40host".into(),
            username: "neo4j".into(),
            password: "pw".into(),
            database: "neo4j".into(),
        };
        let err = identity_source(&creds).unwrap_err();
        assert!(matches!(err, LoadError::Env(_)));
        let msg = err.to_string();
        assert!(msg.contains("invalid host"), "{msg}");
        assert!(!msg.contains("secretpw"), "credential leaked: {msg}");
    }

    #[test]
    fn valid_dns_ip_and_ipv6_hosts_are_accepted() {
        for (uri, url) in [
            (
                "http://localhost:7474",
                "http://localhost:7474/db/neo4j/query/v2",
            ),
            (
                "http://127.0.0.1:7474",
                "http://127.0.0.1:7474/db/neo4j/query/v2",
            ),
            ("http://[::1]:7474", "http://[::1]:7474/db/neo4j/query/v2"),
            ("http://[fe80::1]", "http://[fe80::1]/db/neo4j/query/v2"),
            (
                "https://db.example.com",
                "https://db.example.com/db/neo4j/query/v2",
            ),
            (
                "neo4j+s://42f122c0.databases.neo4j.io",
                "https://42f122c0.databases.neo4j.io/db/neo4j/query/v2",
            ),
        ] {
            assert_eq!(query_api_url(uri, "neo4j").unwrap(), url, "uri {uri:?}");
        }
    }

    #[test]
    fn identity_source_rejects_userinfo_uri() {
        // The identity source is persisted to the on-disk pull-identity map;
        // any userinfo-carrying URI must error instead of being stored.
        // Covers scheme-present slash-in-password, plain literal userinfo,
        // and a percent-encoded variant (no literal `@`, so it fails the
        // host-charset gate rather than the userinfo gate).
        for uri in [
            "http://user:p/ss@host:7687",
            "neo4j://user:p/ss@host:7687",
            "neo4j://user:secretpw@host:7687",
            "http://user@host",
            "http://user%3Asecretpw%40host",
        ] {
            let creds = Credentials {
                uri: uri.into(),
                username: "neo4j".into(),
                password: "pw".into(),
                database: "neo4j".into(),
            };
            let err = identity_source(&creds).unwrap_err();
            assert!(matches!(err, LoadError::Env(_)), "uri {uri:?}");
            let msg = err.to_string();
            assert!(!msg.contains("secretpw"), "credential leaked: {msg}");
            assert!(!msg.contains("p/ss"), "credential leaked: {msg}");
            assert!(!msg.contains(uri), "raw URI leaked: {msg}");
        }
    }

    #[test]
    fn unknown_scheme_error_is_fixed_text() {
        // The unsupported-scheme error never echoes the scheme token: even a
        // legit unknown scheme that matches the RFC 3986 scheme charset is
        // not interpolated (ADR-0122, fixed text everywhere).
        let err = query_api_url("neo4j+ss://host", "neo4j").unwrap_err();
        assert!(matches!(err, LoadError::Env(_)));
        let msg = err.to_string();
        assert_eq!(
            msg,
            "unsupported NEO4J_URI scheme (use neo4j/bolt/http/https or +s/+ssc)"
        );
        assert!(!msg.contains("neo4j+ss"), "scheme token leaked: {msg}");
    }

    #[test]
    fn unknown_scheme_error_does_not_leak_uri_userinfo() {
        // `split_scheme` takes everything before the first `://`, so these
        // "scheme" tokens carry the credential; the fixed text must not
        // contain any of it.
        for uri in [
            "user:secretpw@host://evil",
            "user%3Asecretpw%40host://evil",
            "user:p@ss@host://evil",
        ] {
            let err = query_api_url(uri, "neo4j").unwrap_err();
            assert!(matches!(err, LoadError::Env(_)), "uri {uri:?}");
            let msg = err.to_string();
            assert_eq!(
                msg, "unsupported NEO4J_URI scheme (use neo4j/bolt/http/https or +s/+ssc)",
                "uri {uri:?}"
            );
            assert!(!msg.contains("secretpw"), "credential leaked: {msg}");
            assert!(!msg.contains("p@ss"), "credential leaked: {msg}");
            assert!(!msg.contains("%3A"), "encoded credential leaked: {msg}");
            assert!(!msg.contains(uri), "raw URI leaked: {msg}");
        }
    }

    #[test]
    fn run_query_refuses_cleartext_non_loopback_without_opt_in() {
        let creds = Credentials {
            uri: "http://db.example.com:7474".into(),
            username: "neo4j".into(),
            password: "pw".into(),
            database: "neo4j".into(),
        };
        // Refusal happens before any network call, so no listener is needed.
        let err =
            run_query(&creds, "MATCH (n) RETURN n", &serde_json::Map::new(), false).unwrap_err();
        assert!(matches!(err, LoadError::Env(_)));
        let msg = err.to_string();
        assert!(msg.contains("--allow-plaintext"), "{msg}");
        assert!(msg.contains("db.example.com"), "{msg}");
        assert!(!msg.contains("pw"), "credential leaked: {msg}");
    }

    #[test]
    fn run_query_loopback_fixture_with_opt_in() {
        let body = r#"{"data":{"fields":["n"],"values":[[{"elementId":"4:aaa:0","labels":["Person"],"properties":{"name":"Ada"}}]]}}"#;
        let (h, port) = spawn_single_response("200 OK", body);
        let creds = Credentials {
            uri: format!("http://127.0.0.1:{port}"),
            username: "neo4j".into(),
            password: "pw".into(),
            database: "neo4j".into(),
        };
        let json = run_query(&creds, "MATCH (n) RETURN n", &serde_json::Map::new(), true).unwrap();
        assert!(json["data"]["values"].is_array());
        h.join().ok();
    }

    #[test]
    fn run_query_loopback_fixture() {
        let body = r#"{"data":{"fields":["n"],"values":[[{"elementId":"4:aaa:0","labels":["Person"],"properties":{"name":"Ada"}}]]}}"#;
        let (h, port) = spawn_single_response("200 OK", body);
        let creds = Credentials {
            uri: format!("http://127.0.0.1:{port}"),
            username: "neo4j".into(),
            password: "pw".into(),
            database: "neo4j".into(),
        };
        let json = run_query(&creds, "MATCH (n) RETURN n", &serde_json::Map::new(), false).unwrap();
        assert!(json["data"]["values"].is_array());
        h.join().ok();
    }

    #[test]
    fn run_query_http_error() {
        let (h, port) =
            spawn_single_response("401 Unauthorized", r#"{"errors":[{"message":"no"}]}"#);
        let creds = Credentials {
            uri: format!("http://127.0.0.1:{port}"),
            username: "neo4j".into(),
            password: "pw".into(),
            database: "neo4j".into(),
        };
        let err =
            run_query(&creds, "MATCH (n) RETURN n", &serde_json::Map::new(), false).unwrap_err();
        assert!(matches!(err, LoadError::Http { status: 401, .. }));
        h.join().ok();
    }

    #[test]
    fn run_query_http_error_strips_control_chars() {
        let (h, port) = spawn_single_response(
            "401 Unauthorized",
            "{\"errors\":[{\"message\":\"denied\u{1b}[31m\"}]}",
        );
        let creds = Credentials {
            uri: format!("http://127.0.0.1:{port}"),
            username: "neo4j".into(),
            password: "pw".into(),
            database: "neo4j".into(),
        };
        let err =
            run_query(&creds, "MATCH (n) RETURN n", &serde_json::Map::new(), false).unwrap_err();
        match err {
            LoadError::Http { status: 401, reply } => {
                assert!(!reply.contains('\u{1b}'), "raw ESC leaked: {reply:?}");
                assert!(!reply.chars().any(relate_core::is_unsafe_output_char));
            }
            other => panic!("expected LoadError::Http, got {other:?}"),
        }
        h.join().ok();
    }

    fn loopback_creds(port: u16) -> Credentials {
        Credentials {
            uri: format!("http://127.0.0.1:{port}"),
            username: "neo4j".into(),
            password: "pw".into(),
            database: "neo4j".into(),
        }
    }

    const NODE_BODY: &str = r#"{"data":{"fields":["n"],"values":[[{"elementId":"4:aaa:0","labels":["Person"],"properties":{"name":"Ada"}}]]},"errors":[]}"#;

    /// Drive `run` against a capturing loopback fixture and assert the wire
    /// contract every Query API POST shares: exactly one request, exactly one
    /// `Accept` header with `expected_accept` (ADR-0181), and a plain-JSON
    /// `Content-Type`. Returns the captured request head so an entry point can
    /// add assertions of its own.
    fn assert_request_headers(run: impl Fn(&Credentials), expected_accept: &str) -> String {
        let (fixture, port) = spawn_single_response_capturing("200 OK", NODE_BODY);
        let creds = loopback_creds(port);
        run(&creds);
        let heads = fixture.join();
        assert_eq!(heads.len(), 1, "{heads:?}");
        let head = heads.into_iter().next().expect("one captured head");
        assert_eq!(
            header_values(&head, "accept"),
            [expected_accept],
            "exactly one Accept header, {expected_accept:?}: {head:?}"
        );
        assert_eq!(
            header_values(&head, "content-type"),
            ["application/json"],
            "request bodies stay plain JSON parameters: {head:?}"
        );
        head
    }

    #[test]
    fn run_query_sends_plain_json_headers() {
        assert_request_headers(
            |creds| {
                run_query(creds, "MATCH (n) RETURN n", &serde_json::Map::new(), false).unwrap();
            },
            "application/json",
        );
    }

    /// The typed-parameter write path (ADR-0401): request bodies carry
    /// `Content-Type: application/vnd.neo4j.query`, responses stay plain
    /// JSON, and the returned byte count is the serialized request body's
    /// exact length.
    #[test]
    fn run_query_typed_params_sends_the_typed_content_type() {
        let (fixture, port) = spawn_single_response_capturing("200 OK", NODE_BODY);
        let creds = loopback_creds(port);
        let mut parameters = serde_json::Map::new();
        parameters.insert(
            "rows".into(),
            serde_json::json!([
                {"key": {"$type": "String", "_value": "Ada"}}
            ]),
        );
        let (json, body_bytes) = run_query_typed_params(
            &creds,
            "UNWIND $rows AS row MERGE (n:Person {name: row.key})",
            &parameters,
            false,
        )
        .unwrap();
        assert!(json["data"]["values"].is_array());
        let heads = fixture.join();
        assert_eq!(heads.len(), 1, "{heads:?}");
        let head = heads.into_iter().next().expect("one captured head");
        assert_eq!(
            header_values(&head, "content-type"),
            [TYPED_CONTENT_TYPE],
            "typed parameter bodies carry the typed request media type: {head:?}"
        );
        assert_eq!(
            header_values(&head, "accept"),
            ["application/json"],
            "write responses stay plain JSON: {head:?}"
        );
        // The byte count is the exact serialized request body length: the
        // Content-Length line names it.
        let content_length: u64 = head
            .lines()
            .find_map(|line| line.strip_prefix("content-length: "))
            .and_then(|v| v.parse().ok())
            .expect("a captured head carries content-length");
        assert_eq!(body_bytes, content_length);
    }

    #[test]
    fn run_query_streaming_sends_plain_json_headers() {
        assert_request_headers(
            |creds| {
                let mut rows = 0usize;
                run_query_streaming(
                    creds,
                    "MATCH (n) RETURN n",
                    &serde_json::Map::new(),
                    false,
                    &mut |batch| {
                        rows += batch.len();
                        Ok(())
                    },
                )
                .unwrap();
                assert_eq!(rows, 1);
            },
            "application/json",
        );
    }

    #[test]
    fn run_query_streaming_typed_sends_the_typed_accept_ladder() {
        let head = assert_request_headers(
            |creds| {
                let mut rows = 0usize;
                run_query_streaming_typed(
                    creds,
                    "MATCH (n) RETURN n",
                    &serde_json::Map::new(),
                    false,
                    &mut |batch| {
                        rows += batch.len();
                        Ok(())
                    },
                )
                .unwrap();
                assert_eq!(rows, 1);
            },
            TYPED_ACCEPT_HEADER,
        );
        // The exact header line on the wire: ureq writes standard header
        // names lowercase (HTTP names are case-insensitive).
        let expected_line = format!("accept: {TYPED_ACCEPT_HEADER}");
        assert!(
            head.lines().any(|line| line == expected_line),
            "request head must carry the exact line {expected_line:?}: {head:?}"
        );
    }

    #[test]
    fn typed_accept_ladder_matches_adr_0181() {
        assert_eq!(
            TYPED_ACCEPT_HEADER,
            "application/vnd.neo4j.query.v1.2;q=1.0, application/vnd.neo4j.query.v1.1;q=0.9, application/vnd.neo4j.query.v1.0;q=0.8, application/vnd.neo4j.query;q=0.7"
        );
    }
}
