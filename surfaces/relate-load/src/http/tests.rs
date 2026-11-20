use super::*;
use relate_test_support::loopback::{
    header_value, header_values, spawn_single_response, spawn_single_response_capturing,
};

#[test]
fn aura_query_url_strips_bolt_port() {
    for (uri, database, url) in [
        (
            "neo4j+s://42f122c0.databases.neo4j.io",
            "neo4j",
            "https://42f122c0.databases.neo4j.io/db/neo4j/query/v2",
        ),
        (
            "neo4j+s://42f122c0.databases.neo4j.io:7687",
            "neo4j",
            "https://42f122c0.databases.neo4j.io/db/neo4j/query/v2",
        ),
        (
            "neo4j+s://42f122c0.databases.neo4j",
            "movies",
            "https://42f122c0.databases.neo4j.io/db/movies/query/v2",
        ),
    ] {
        assert_eq!(query_api_url(uri, database).unwrap(), url, "uri {uri:?}");
    }
}

#[test]
fn aura_host_scheme_admission_is_uniform() {
    // The Aura host waives no boundary rule: a scheme-less or
    // unknown-scheme URI errors like any other host, an explicit
    // `http(s)` URI is a Query API origin used as-is (ADR-0080) instead of
    // being silently upgraded to https, and the bolt-family Aura arm
    // states its own port policy — any bolt port (default, explicit, or
    // unmapped) resolves to the bare https host.
    for uri in [
        "42f122c0.databases.neo4j.io",
        "garbage://42f122c0.databases.neo4j.io",
    ] {
        let err = query_api_url(uri, "neo4j").unwrap_err();
        assert!(matches!(err, LoadError::Env(_)), "uri {uri:?}");
        assert!(!err.to_string().contains(uri), "raw URI leaked: {err}");
    }
    assert_eq!(
        query_api_url("http://42f122c0.databases.neo4j.io:7474", "neo4j").unwrap(),
        "http://42f122c0.databases.neo4j.io:7474/db/neo4j/query/v2"
    );
    assert_eq!(
        query_api_url("neo4j+s://42f122c0.databases.neo4j.io:9999", "neo4j").unwrap(),
        "https://42f122c0.databases.neo4j.io/db/neo4j/query/v2"
    );
}

#[test]
fn local_neo4j_maps_7687_to_7474() {
    for (uri, url) in [
        (
            "neo4j://localhost:7687",
            "http://localhost:7474/db/neo4j/query/v2",
        ),
        (
            "neo4j://127.0.0.1",
            "http://127.0.0.1:7474/db/neo4j/query/v2",
        ),
    ] {
        assert_eq!(query_api_url(uri, "neo4j").unwrap(), url, "uri {uri:?}");
    }
}

#[test]
fn tls_bolt_maps_7687_to_7473() {
    // The +s/+ssc spellings are the TLS twins of 7687 → 7474: the default
    // bolt port (explicit or implied) maps to the HTTPS Query API port
    // 7473 as the canonical origin — never a bare `https://host`, which
    // lands on port 443 where a self-hosted server has no Query API (the
    // generic web port is the fallback candidate, ADR-0633).
    for uri in [
        "neo4j+s://db.example.com:7687",
        "neo4j+s://db.example.com",
        "neo4j+ssc://db.example.com",
        "bolt+s://db.example.com:7687",
        "bolt+ssc://db.example.com",
    ] {
        assert_eq!(
            query_api_url(uri, "neo4j").unwrap(),
            "https://db.example.com:7473/db/neo4j/query/v2",
            "uri {uri:?}"
        );
    }
}

#[test]
fn bolt_uris_carry_the_generic_web_port_as_fallback_candidate() {
    // ADR-0633: a Bolt-family URI on a non-Aura host resolves to the
    // server's own Query API port as the canonical candidate, then the
    // same scheme on the generic web port as the one fallback; http(s)
    // and Aura URIs carry no fallback.
    for (uri, expected) in [
        (
            "neo4j://db.example.com:7687",
            vec![
                "http://db.example.com:7474/db/neo4j/query/v2",
                "http://db.example.com/db/neo4j/query/v2",
            ],
        ),
        (
            "neo4j+ssc://db.example.com",
            vec![
                "https://db.example.com:7473/db/neo4j/query/v2",
                "https://db.example.com/db/neo4j/query/v2",
            ],
        ),
        (
            "https://db.example.com:7473",
            vec!["https://db.example.com:7473/db/neo4j/query/v2"],
        ),
        (
            "neo4j+s://42f122c0.databases.neo4j.io:7687",
            vec!["https://42f122c0.databases.neo4j.io/db/neo4j/query/v2"],
        ),
    ] {
        let endpoint = query_api(uri, "neo4j").unwrap();
        let urls: Vec<String> = endpoint
            .ports
            .variants()
            .map(|port| endpoint.url(port))
            .collect();
        assert_eq!(urls, expected, "uri {uri:?}");
    }
}

#[test]
fn first_responding_origin_walks_candidates_until_one_answers() {
    // ADR-0633: any HTTP answer selects the origin (a 401 proves it is
    // the Query API); an undelivered failure advances to the fallback;
    // an ambiguous failure — the request may have been delivered — never
    // advances (a repeat POST could duplicate a non-idempotent write);
    // every candidate failing undelivered is one error naming each
    // origin tried.
    let endpoint = QueryEndpoint {
        scheme: WebScheme::Https,
        host: "db.example.com".into(),
        ports: Ports::CanonicalThenGenericWeb(Some(7473)),
        path: "/db/neo4j/query/v2".into(),
    };
    let canonical = endpoint.canonical_url();
    let fallback = endpoint.url(None);
    // The canonical origin answers: selected, the fallback is never
    // attempted.
    let (url, value) = first_responding_origin(&endpoint, |url| {
        assert_eq!(
            url, canonical,
            "the fallback was attempted before the server port answered"
        );
        Ok::<_, WalkFailure>(7)
    })
    .unwrap();
    assert_eq!((url.as_str(), value), (canonical.as_str(), 7));
    // The canonical origin is undelivered (connection refused), the
    // fallback answers.
    let (url, ()) = first_responding_origin(&endpoint, |url| {
        if url == canonical {
            Err(WalkFailure::Undelivered("connection refused".to_string()))
        } else {
            Ok(())
        }
    })
    .unwrap();
    assert_eq!(url, fallback);
    // Every candidate undelivered: one error naming each origin tried.
    let err = first_responding_origin(&endpoint, |_| {
        Err::<(), _>(WalkFailure::Undelivered("connection refused".to_string()))
    })
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains(&canonical), "{msg}");
    assert!(msg.contains(&fallback), "{msg}");
    // An ambiguous failure never advances: the fallback is not attempted
    // and the error says the request was not retried.
    let err = first_responding_origin(&endpoint, |url| {
        assert_eq!(
            url, canonical,
            "the fallback was attempted past an ambiguous failure"
        );
        Err::<(), _>(WalkFailure::Ambiguous("operation timed out".to_string()))
    })
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains(&canonical), "{msg}");
    assert!(msg.contains("not retried on another origin"), "{msg}");
    assert!(
        !msg.contains(&fallback),
        "the fallback leaked into the error: {msg}"
    );
}

#[test]
fn walk_failure_classifies_delivery() {
    // ADR-0633: only failures that prove the request never left the
    // client advance the walk — a refused or unreachable connection, an
    // unresolved host, a connect (TLS handshake included) or resolver
    // timeout; timeouts past the connect, resets, and TLS or protocol
    // errors may mean the server applied the request, so they never do.
    use std::io::ErrorKind;
    let io = |kind| ureq::Error::Io(std::io::Error::from(kind));
    for (error, expected_undelivered) in [
        (io(ErrorKind::ConnectionRefused), true),
        (io(ErrorKind::HostUnreachable), true),
        (io(ErrorKind::NetworkUnreachable), true),
        (ureq::Error::HostNotFound, true),
        (ureq::Error::Timeout(ureq::Timeout::Resolve), true),
        (ureq::Error::Timeout(ureq::Timeout::Connect), true),
        (io(ErrorKind::TimedOut), false),
        (io(ErrorKind::ConnectionAborted), false),
        (io(ErrorKind::ConnectionReset), false),
        (ureq::Error::Timeout(ureq::Timeout::Global), false),
        (ureq::Error::ConnectionFailed, false),
    ] {
        let label = format!("{error:?}");
        let undelivered = matches!(walk_failure(error), WalkFailure::Undelivered(_));
        assert_eq!(undelivered, expected_undelivered, "{label}");
    }
}

#[test]
fn real_refused_connect_is_undelivered_and_names_the_origin() {
    // A real refused TCP connect through the real client classifies as
    // undelivered (ADR-0633) — the aggregate error names the tried
    // origin, never the ambiguous not-retried refusal a misclassification
    // would produce. A dead port comes from binding a listener and
    // dropping it: nothing relistens on that ephemeral port.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dead_port = listener.local_addr().unwrap().port();
    drop(listener);
    let creds = creds_with_uri(format!("http://127.0.0.1:{dead_port}"));
    let err = run_query(&creds, "MATCH (n) RETURN n", &serde_json::Map::new(), false).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("NEO4J_URI reached no Query API origin"),
        "a real refused connect must classify as undelivered: {msg}"
    );
    assert!(msg.contains(&creds.uri), "the tried origin is named: {msg}");
    assert!(
        !msg.contains("not retried on another origin"),
        "an undelivered refusal is never the ambiguous not-retried error: {msg}"
    );
}

#[test]
fn hosted_demo_tls_bolt_uri_maps_to_its_https_query_api() {
    // The hosted demo serves its Query API on https :7473; a user
    // pasting the demo's TLS Bolt connection URI pulls against it.
    assert_eq!(
        query_api_url("neo4j+s://demo.neo4jlabs.com:7687", "companies").unwrap(),
        "https://demo.neo4jlabs.com:7473/db/companies/query/v2"
    );
}

#[test]
fn non_default_bolt_port_is_error() {
    for (uri, mapping) in [
        ("neo4j://host:9999", "7687 → 7474"),
        ("neo4j+s://host:9999", "7687 → 7473"),
    ] {
        let err = query_api_url(uri, "neo4j").unwrap_err();
        assert!(matches!(err, LoadError::Env(_)), "uri {uri:?}");
        let msg = err.to_string();
        assert!(msg.contains(mapping), "uri {uri:?}: {msg}");
    }
}

#[test]
fn malformed_port_is_refused_not_defaulted() {
    // A port that is present but not a number is refused with fixed text
    // (ADR-0122), never folded into "absent" — the default-port mappings
    // would otherwise silently reinterpret `neo4j://host:abc` as :7474 and
    // drop the port of `http://host:abc`.
    for uri in [
        "neo4j://host:abc",
        "neo4j+s://host:99999",
        "http://host:abc",
        // Bracketed-host junk (a non-`:port` suffix after `]`) carries as
        // a malformed port too — never the missing-host text.
        "http://[::1]evil.com",
    ] {
        let err = query_api_url(uri, "neo4j").unwrap_err();
        assert!(matches!(err, LoadError::Env(_)), "uri {uri:?}");
        let msg = err.to_string();
        assert_eq!(
            msg, "NEO4J_URI has an invalid port (expected a number 0-65535)",
            "uri {uri:?}"
        );
        assert!(!msg.contains(uri), "raw URI leaked: {msg}");
    }
}

#[test]
fn database_name_is_validated_at_the_url_boundary() {
    // `query_api_url` is pub and re-exported: a caller may bypass
    // `Credentials`, whose construction already validated the name, so the
    // boundary enforces the shared database-name rule itself — the
    // database becomes a URL path segment, never an arbitrary path.
    let too_long = "x".repeat(64);
    for database in ["", "movies/extra", "movies?x=1", "has space", &too_long] {
        let err = query_api_url("neo4j://localhost:7687", database).unwrap_err();
        assert!(matches!(err, LoadError::Env(_)), "database {database:?}");
        let msg = err.to_string();
        assert!(msg.starts_with("NEO4J_DATABASE"), "{msg}");
        assert!(!msg.contains("localhost"), "URI leaked: {msg}");
    }
    assert_eq!(
        query_api_url("neo4j://localhost:7687", "movies_2.x").unwrap(),
        "http://localhost:7474/db/movies_2.x/query/v2"
    );
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
    let endpoint = query_api("http://db.example.com:7474", "neo4j").unwrap();
    let err = ensure_cleartext_basic_allowed(&endpoint, false).unwrap_err();
    assert!(matches!(err, LoadError::Env(_)));
    let msg = err.to_string();
    assert!(msg.contains("--allow-plaintext"), "{msg}");
    assert!(msg.contains("db.example.com"), "{msg}");
    assert!(msg.contains("http://"), "{msg}");
}

#[test]
fn non_loopback_http_with_opt_in_is_allowed() {
    let endpoint = query_api("http://db.example.com:7474", "neo4j").unwrap();
    ensure_cleartext_basic_allowed(&endpoint, true).unwrap();
}

#[test]
fn loopback_http_is_allowed_without_opt_in() {
    let endpoint = query_api("http://127.0.0.1:7474", "neo4j").unwrap();
    ensure_cleartext_basic_allowed(&endpoint, false).unwrap();
}

#[test]
fn https_is_never_refused() {
    let endpoint = query_api("https://db.example.com:7473", "neo4j").unwrap();
    ensure_cleartext_basic_allowed(&endpoint, false).unwrap();
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
    let creds = creds_with_uri("http://user%3Asecretpw%40host");
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
        let creds = creds_with_uri(uri);
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
    let creds = creds_with_uri("http://db.example.com:7474");
    // Refusal happens before any network call, so no listener is needed.
    let err = run_query(&creds, "MATCH (n) RETURN n", &serde_json::Map::new(), false).unwrap_err();
    assert!(matches!(err, LoadError::Env(_)));
    let msg = err.to_string();
    assert!(msg.contains("--allow-plaintext"), "{msg}");
    assert!(msg.contains("db.example.com"), "{msg}");
    assert!(!msg.contains("pw"), "credential leaked: {msg}");
}

#[test]
fn run_query_loopback_fixture_both_plaintext_policies() {
    // Loopback is allowed silently with and without --allow-plaintext
    // (ADR-0122): one driver, both flag values.
    for allow_plaintext in [false, true] {
        let (h, port) = spawn_single_response("200 OK", NODE_BODY);
        let creds = loopback_creds(port);
        let json = run_query(
            &creds,
            "MATCH (n) RETURN n",
            &serde_json::Map::new(),
            allow_plaintext,
        )
        .unwrap();
        assert!(json["data"]["values"].is_array());
        h.join().expect("scripted server thread");
    }
}

#[test]
fn run_query_http_error() {
    let (h, port) = spawn_single_response("401 Unauthorized", r#"{"errors":[{"message":"no"}]}"#);
    let creds = loopback_creds(port);
    let err = run_query(&creds, "MATCH (n) RETURN n", &serde_json::Map::new(), false).unwrap_err();
    assert!(matches!(err, LoadError::Http { status: 401, .. }));
    h.join().expect("scripted server thread");
}

#[test]
fn run_query_http_error_strips_control_chars() {
    let (h, port) = spawn_single_response(
        "401 Unauthorized",
        "{\"errors\":[{\"message\":\"denied\u{1b}[31m\"}]}",
    );
    let creds = loopback_creds(port);
    let err = run_query(&creds, "MATCH (n) RETURN n", &serde_json::Map::new(), false).unwrap_err();
    match err {
        LoadError::Http { status: 401, reply } => {
            assert!(!reply.contains('\u{1b}'), "raw ESC leaked: {reply:?}");
            assert!(!reply.chars().any(relate_core::is_unsafe_output_char));
        }
        other => panic!("expected LoadError::Http, got {other:?}"),
    }
    h.join().expect("scripted server thread");
}

/// Test credentials for a URI: the one construction the non-loopback
/// tests share (`loopback_creds` is its loopback spelling).
fn creds_with_uri(uri: impl Into<String>) -> Credentials {
    Credentials {
        uri: uri.into(),
        username: "neo4j".into(),
        password: "pw".into(),
        database: "neo4j".into(),
    }
}

fn loopback_creds(port: u16) -> Credentials {
    creds_with_uri(format!("http://127.0.0.1:{port}"))
}

const NODE_BODY: &str = r#"{"data":{"fields":["n"],"values":[[{"elementId":"4:aaa:0","labels":["Person"],"properties":{"name":"Ada"}}]]},"errors":[]}"#;

/// Drive `run` against a capturing loopback fixture and assert the wire
/// contract every Query API POST shares: exactly one request, exactly one
/// `Accept` header with `expected_accept` (ADR-0181), and a `Content-Type`
/// of `expected_content_type` — `application/json` for plain parameter
/// bodies, the typed request media type for ADR-0401 writes. Returns the
/// captured request head and whatever `run` produced, so an entry point
/// can add assertions of its own.
fn assert_request_headers<T>(
    mut run: impl FnMut(&Credentials) -> T,
    expected_accept: &str,
    expected_content_type: &str,
) -> (String, T) {
    let (fixture, port) = spawn_single_response_capturing("200 OK", NODE_BODY);
    let creds = loopback_creds(port);
    let result = run(&creds);
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
        [expected_content_type],
        "request bodies carry the expected media type: {head:?}"
    );
    (head, result)
}

#[test]
fn run_query_sends_plain_json_headers() {
    assert_request_headers(
        |creds| {
            run_query(creds, "MATCH (n) RETURN n", &serde_json::Map::new(), false).unwrap();
        },
        "application/json",
        "application/json",
    );
}

/// The typed-parameter write path (ADR-0401): request bodies carry
/// `Content-Type: TYPED_CONTENT_TYPE` (the v1.2 typed request media
/// type), responses stay plain JSON, and the returned byte count is the
/// serialized request body's exact length.
#[test]
fn run_query_typed_params_sends_the_typed_content_type() {
    let (head, sent_bytes) = assert_request_headers(
        |creds| {
            let mut parameters = serde_json::Map::new();
            parameters.insert(
                "rows".into(),
                serde_json::json!([
                    {"key": {"$type": "String", "_value": "Ada"}}
                ]),
            );
            let (json, body_bytes) = run_query_typed_params(
                creds,
                "UNWIND $rows AS row MERGE (n:Person {name: row.key})",
                &parameters,
                false,
            )
            .unwrap();
            assert!(json["data"]["values"].is_array());
            body_bytes
        },
        "application/json",
        TYPED_CONTENT_TYPE,
    );
    // The byte count is the exact serialized request body length: the
    // Content-Length header names it.
    let content_length: u64 = header_value(&head, "content-length")
        .expect("a captured head carries content-length")
        .parse()
        .expect("content-length is a number");
    assert_eq!(sent_bytes, content_length);
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
        "application/json",
    );
}

#[test]
fn run_query_streaming_typed_sends_the_typed_accept_ladder() {
    // The harness asserts the exact Accept value (one header, the typed
    // ladder); nothing about the raw header line is load-bearing —
    // header lookup is case-insensitive by construction.
    assert_request_headers(
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
        "application/json",
    );
}

#[test]
fn typed_accept_ladder_matches_adr_0181() {
    assert_eq!(
        TYPED_ACCEPT_HEADER,
        "application/vnd.neo4j.query.v1.2;q=1.0, application/vnd.neo4j.query.v1.1;q=0.9, application/vnd.neo4j.query.v1.0;q=0.8, application/vnd.neo4j.query;q=0.7"
    );
}
