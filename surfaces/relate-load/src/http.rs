use std::io::Read;

use base64::Engine;
use serde_json::Value as Json;

use crate::creds::{database_name_error, uri_authority, Credentials, UriAuthority, UriPort};
use crate::response::{parse_v2_response, truncate_for_error, MAX_RESPONSE_BYTES};
use crate::LoadError;

const BOLT_PORT: u16 = 7687;
const HTTP_DEFAULT: u16 = 7474;
const HTTPS_DEFAULT: u16 = 7473;
const QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Query API v2 URL for `uri` + `database` (ADR-0080): the **canonical**
/// URL of the endpoint [`query_api`] resolves — the origin a pull's
/// identity normalizes to (ADR-0633).
///
/// Error text is always fixed and never interpolates the raw `NEO4J_URI`: a
/// malformed URI can carry credentials (literal or percent-encoded userinfo),
/// so validation failures say what is wrong without echoing the input
/// (ADR-0122, validate-don't-sanitize).
pub fn query_api_url(uri: &str, database: &str) -> Result<String, LoadError> {
    Ok(query_api(uri, database)?.canonical_url())
}

/// Resolve the Query API v2 endpoint a `NEO4J_URI` + `database` names
/// (ADR-0080, ADR-0633): the parsed origin plus the ordered port variants
/// the request prelude walks. The canonical port is always present; a
/// Bolt-family URI on a non-Aura host additionally carries the generic
/// web port as the refused-connect fallback.
///
/// Error text is always fixed and never interpolates the raw `NEO4J_URI`
/// (ADR-0122, validate-don't-sanitize).
pub(crate) fn query_api(uri: &str, database: &str) -> Result<QueryEndpoint, LoadError> {
    let uri = uri.trim();
    reject_uri_userinfo(uri)?;
    let Some(auth) = uri_authority(uri) else {
        return Err(LoadError::Env("NEO4J_URI missing host".to_string()));
    };
    // Complete the truncated Aura spelling on the parsed authority — the
    // one place the host is normalized, so the endpoint and every
    // classification after it read the same value.
    let auth = complete_aura(auth);
    // Validate before anything else consumes, echoes, or persists the host:
    // an encoded-userinfo probe (`http://user%3Asecretpw%40host`) parses the
    // credential as the host. Refuse it here rather than echo it in an error
    // or persist it via `identity_source` (ADR-0122).
    validate_host(&auth.host)?;
    resolve_query_endpoint(&auth, database)
}

/// The resolved Query API endpoint a `NEO4J_URI` names (ADR-0080,
/// ADR-0633): one origin — the typed scheme and the (Aura-completed)
/// host, parsed once — plus the ordered port variants the request
/// prelude walks. Every candidate names the same scheme and host by
/// construction — only the port varies — and the cleartext policy
/// consumes exactly this origin, never a re-parsed URL.
pub(crate) struct QueryEndpoint {
    scheme: WebScheme,
    host: String,
    /// The ordered port variants the walk tries (ADR-0633): the canonical
    /// port, then — for a Bolt family on a non-Aura host — the scheme's
    /// generic web port (no port spelled). The enum makes the pairing
    /// unrepresentable wrong: it names exactly which walk applies.
    ports: Ports,
    /// The validated `/db/{database}/query/v2` path.
    path: String,
}

impl QueryEndpoint {
    /// The origin every candidate shares — scheme and host, no port, no
    /// path; the cleartext policy's one input.
    fn origin(&self) -> String {
        format!("{}://{}", self.scheme.as_str(), self.host)
    }

    /// The URL of one port variant of this endpoint.
    fn url(&self, port: Option<u16>) -> String {
        let port = port.map(|p| format!(":{p}")).unwrap_or_default();
        format!("{}{port}{path}", self.origin(), path = self.path)
    }

    /// The canonical Query API URL — the first variant the walk tries,
    /// and the origin a pull's identity normalizes to (ADR-0633).
    fn canonical_url(&self) -> String {
        self.url(self.ports.canonical())
    }
}

/// The ordered port variants of a [`QueryEndpoint`] (ADR-0633).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ports {
    /// One candidate: the canonical port spelling — `Some(p)` spells
    /// `:{p}`, `None` spells the bare host (a portless `http(s)` URI, or
    /// the bare `https` Aura host).
    Canonical(Option<u16>),
    /// The canonical port, then the scheme's generic web port (the bare
    /// host) as the refused-connect fallback — a Bolt family on a
    /// non-Aura host.
    CanonicalThenGenericWeb(Option<u16>),
}

impl Ports {
    /// The canonical port, whichever walk applies.
    fn canonical(self) -> Option<u16> {
        match self {
            Ports::Canonical(p) | Ports::CanonicalThenGenericWeb(p) => p,
        }
    }

    /// The ordered port variants the walk tries.
    fn variants(self) -> impl Iterator<Item = Option<u16>> {
        let fallback = match self {
            Ports::Canonical(_) => None,
            Ports::CanonicalThenGenericWeb(_) => Some(None),
        };
        std::iter::once(self.canonical()).chain(fallback)
    }
}

/// The web scheme of a Query API origin — typed, so the plaintext
/// Basic-auth gate matches the variant, never a re-derived string
/// (ADR-0122).
#[derive(Clone, Copy, PartialEq, Eq)]
enum WebScheme {
    Http,
    Https,
}

impl WebScheme {
    fn as_str(self) -> &'static str {
        match self {
            WebScheme::Http => "http",
            WebScheme::Https => "https",
        }
    }

    /// The Query API server port this scheme's Bolt family maps its
    /// default bolt port to (ADR-0633): http → 7474, https → 7473.
    fn server_port(self) -> u16 {
        match self {
            WebScheme::Http => HTTP_DEFAULT,
            WebScheme::Https => HTTPS_DEFAULT,
        }
    }
}

/// Resolve the endpoint's origin and ports from the parsed authority —
/// scheme presence, admission, and port validity uniform for every host,
/// an Aura host waiving nothing (ADR-0080, ADR-0633).
fn resolve_query_endpoint(auth: &UriAuthority, database: &str) -> Result<QueryEndpoint, LoadError> {
    let Some(scheme) = auth.scheme.as_deref() else {
        return Err(LoadError::Env("NEO4J_URI missing scheme".to_string()));
    };
    // Port validity is uniform for every host and scheme: a port that is
    // present but not a number is refused outright (ADR-0122), never folded
    // into "absent" — the default-port mappings below would otherwise
    // silently reinterpret `neo4j://host:abc` as :7474 and drop the port of
    // `http://host:abc`.
    let port = match auth.port {
        UriPort::Malformed => {
            return Err(LoadError::Env(
                "NEO4J_URI has an invalid port (expected a number 0-65535)".to_string(),
            ));
        }
        UriPort::Absent => None,
        UriPort::Port(p) => Some(p),
    };
    let Some(family) = SchemeFamily::classify(scheme) else {
        // Fixed text (ADR-0122): `split_scheme` takes everything before
        // the first `://`, so a URI like `user:pw@host://x` (or its
        // percent-encoded twin `user%3Apw%40host://x`) yields a "scheme"
        // token carrying userinfo. No scheme token is ever echoed.
        return Err(LoadError::Env(UNSUPPORTED_SCHEME_TEXT.to_string()));
    };
    // The database becomes the URL path segment composed below: the
    // shared name rule (ADR-0080) is owned here, where the segment is
    // built — the charset keeps `/`, `?`, and spaces out of the path.
    // Callers may bypass `Credentials`, whose construction already
    // validated the name.
    if let Some(err) = database_name_error(database) {
        return Err(LoadError::Env(format!("NEO4J_DATABASE {err}")));
    }
    let path = format!("/db/{database}/query/v2");
    let (scheme, ports) = match family {
        SchemeFamily::Origin(scheme) => (scheme, Ports::Canonical(port)),
        SchemeFamily::Bolt(scheme) => bolt_origin(scheme, &auth.host, port)?,
    };
    Ok(QueryEndpoint {
        scheme,
        host: auth.host.clone(),
        ports,
        path,
    })
}

/// The Query API endpoint a Bolt-family URI's default port (7687,
/// explicit or implied) resolves to — the one policy for both spellings
/// (ADR-0080, ADR-0633): an Aura host serves the Query API on the bare
/// `https` host, any bolt port stripped; any other host maps the default
/// bolt port to the sibling's own server port, with the generic web port
/// as the refused-connect fallback — the server's own port first,
/// because any HTTP answer wins the walk and the generic web port is
/// the one likely to host an unrelated service. An explicit non-default
/// bolt port has no mapping: the Query API may listen anywhere.
fn bolt_origin(
    scheme: WebScheme,
    host: &str,
    port: Option<u16>,
) -> Result<(WebScheme, Ports), LoadError> {
    if is_aura_host(host) {
        return Ok((WebScheme::Https, Ports::Canonical(None)));
    }
    match port {
        None | Some(BOLT_PORT) => Ok((
            scheme,
            Ports::CanonicalThenGenericWeb(Some(scheme.server_port())),
        )),
        Some(p) => Err(LoadError::Env(format!(
            "NEO4J_URI bolt port {p} has no Query API mapping ({} → {}); use an explicit http(s) NEO4J_URI",
            BOLT_PORT,
            scheme.server_port()
        ))),
    }
}

/// The Aura database domain, in its two spellings — the one home of the
/// Aura-host fact, beside the Query API origin policy that consumes it:
/// [`is_aura_host`] matches the completed domain;
/// [`complete_aura`] completes the truncated stem to it. A domain
/// change edits exactly these two constants.
const AURA_DOMAIN_STEM: &str = ".databases.neo4j";
const AURA_DOMAIN: &str = ".databases.neo4j.io";

/// An Aura database host (`<id>.databases.neo4j.io`).
fn is_aura_host(host: &str) -> bool {
    host.ends_with(AURA_DOMAIN)
}

/// Complete a truncated Aura host (`*.databases.neo4j` →
/// `*.databases.neo4j.io`) on the parsed authority — the host with its
/// stem suffix replaced by the completed domain. A host already carrying
/// the domain does not end with the stem, so completion is idempotent.
fn complete_aura(mut auth: UriAuthority) -> UriAuthority {
    if let Some(prefix) = auth.host.strip_suffix(AURA_DOMAIN_STEM) {
        auth.host = format!("{prefix}{AURA_DOMAIN}");
    }
    auth
}

/// The `NEO4J_URI` scheme family, classified once — the single home of
/// the scheme vocabulary (ADR-0633): no other site lists the spellings.
#[derive(Clone, Copy)]
enum SchemeFamily {
    /// An `http(s)` URI: a Query API origin used as-is.
    Origin(WebScheme),
    /// A Bolt-family URI: the default bolt port (7687, explicit or
    /// implied) reinterprets as the Query API scheme it maps to.
    Bolt(WebScheme),
}

/// The fixed unsupported-scheme refusal text — the one sanctioned
/// re-listing of the scheme spellings beside [`SchemeFamily::classify`],
/// the vocabulary's single home; the two are edited together (ADR-0122
/// pins the wording, so it cannot be derived at runtime).
const UNSUPPORTED_SCHEME_TEXT: &str =
    "unsupported NEO4J_URI scheme (use neo4j/bolt/http/https or +s/+ssc)";

impl SchemeFamily {
    /// The one exhaustive match over the accepted scheme spellings —
    /// besides [`UNSUPPORTED_SCHEME_TEXT`], the one sanctioned echo.
    fn classify(scheme: &str) -> Option<Self> {
        match scheme {
            "http" => Some(Self::Origin(WebScheme::Http)),
            "https" => Some(Self::Origin(WebScheme::Https)),
            "neo4j" | "bolt" => Some(Self::Bolt(WebScheme::Http)),
            "neo4j+s" | "neo4j+ssc" | "bolt+s" | "bolt+ssc" => Some(Self::Bolt(WebScheme::Https)),
            _ => None,
        }
    }
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

/// Enforce the plaintext Basic-auth policy (ADR-0122) on the resolved
/// endpoint's origin — the parsed scheme and host, never a re-parsed
/// URL: every port variant of one endpoint shares them, so one verdict
/// covers the whole walk. `http://` to a non-loopback host is refused
/// unless `allow_plaintext` is set; the opt-in downgrades the refusal to
/// a warning. Loopback is always allowed silently, and `https://` is
/// never refused.
fn ensure_cleartext_basic_allowed(
    endpoint: &QueryEndpoint,
    allow_plaintext: bool,
) -> Result<(), LoadError> {
    if endpoint.scheme != WebScheme::Http {
        return Ok(());
    }
    if is_loopback_host(&endpoint.host) {
        return Ok(());
    }
    let origin = endpoint.origin();
    if !allow_plaintext {
        return Err(LoadError::Env(format!(
            "refusing to send Basic auth credentials in the clear: Query API URL {origin} is http:// to a non-loopback host; pass --allow-plaintext to accept the risk, or use an https:// NEO4J_URI"
        )));
    }
    eprintln!(
        "warning: Query API URL {origin} is http://; Basic auth credentials will be sent in the clear (--allow-plaintext)"
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

/// Why an attempt on one candidate origin failed ([`first_responding_origin`],
/// ADR-0633).
#[derive(Debug)]
enum WalkFailure {
    /// The request provably never left the client — the connection was
    /// refused or unreachable, the host never resolved, or the connect
    /// (TLS handshake included) timed out: the walk advances to the next
    /// candidate, which repeats nothing the server might have applied.
    Undelivered(String),
    /// Every other failure — a global or body timeout, a reset or aborted
    /// connection, a TLS or protocol error after the connect: the request
    /// may have reached the server, so re-POSTing it on another origin
    /// could duplicate a non-idempotent write (the push path, ADR-0401).
    /// The walk stops and the error surfaces.
    Ambiguous(String),
}

/// Walk the endpoint's port variants until one answers (ADR-0633): `Ok` —
/// any HTTP status, a `401` included, proves the origin — selects it; a
/// [`WalkFailure::Undelivered`] advances; a [`WalkFailure::Ambiguous`]
/// stops the walk — a repeat POST could duplicate a non-idempotent
/// write. The canonical port is tried first, then the generic web port;
/// every candidate failing undelivered is one error naming each origin
/// tried with its failure.
fn first_responding_origin<R>(
    endpoint: &QueryEndpoint,
    attempt: impl Fn(&str) -> Result<R, WalkFailure>,
) -> Result<(String, R), LoadError> {
    let mut failures = Vec::new();
    for port in endpoint.ports.variants() {
        let url = endpoint.url(port);
        match attempt(&url) {
            Ok(response) => return Ok((url, response)),
            Err(WalkFailure::Undelivered(e)) => failures.push(format!("{url} ({e})")),
            Err(WalkFailure::Ambiguous(e)) => {
                return Err(LoadError::Io(format!(
                    "Query API request to {url} failed ({e}); not retried on another origin: the failure does not prove the request was undelivered, and a repeat POST could duplicate a non-idempotent write"
                )));
            }
        }
    }
    Err(LoadError::Io(format!(
        "NEO4J_URI reached no Query API origin (tried {})",
        failures.join("; ")
    )))
}

/// Classify a transport failure from the Query API POST (ADR-0633):
/// [`WalkFailure::Undelivered`] — the walk may advance — only when the
/// failure proves the request never left the client (a refused or
/// unreachable connection, an unresolved host, a connect or
/// TLS-handshake timeout); everything else may mean the server applied
/// the request, so the walk never repeats it.
fn walk_failure(error: ureq::Error) -> WalkFailure {
    let text = error.to_string();
    match &error {
        ureq::Error::Io(io)
            if matches!(
                io.kind(),
                std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::HostUnreachable
                    | std::io::ErrorKind::NetworkUnreachable
            ) =>
        {
            WalkFailure::Undelivered(text)
        }
        ureq::Error::HostNotFound
        | ureq::Error::Timeout(ureq::Timeout::Resolve | ureq::Timeout::Connect) => {
            WalkFailure::Undelivered(text)
        }
        _ => WalkFailure::Ambiguous(text),
    }
}

/// The request one candidate origin is POSTed (ADR-0181, ADR-0401): the
/// serialized body, the Basic-auth header, the request and response
/// media types, and the deadline policy — one unit the walk sends.
struct RequestSpec {
    body: String,
    body_bytes: u64,
    auth: String,
    content: ContentFormat,
    format: ResultFormat,
    deadline: BodyDeadline,
}

impl RequestSpec {
    /// Serialize the statement + parameters and the auth header. The
    /// `Accept` header comes from `format` (ADR-0181) — the pull path
    /// asks for typed JSON, every other caller for plain JSON — and the
    /// `Content-Type` from `content`: plain JSON parameters everywhere
    /// except the push write path (ADR-0401).
    fn new(
        creds: &Credentials,
        cypher: &str,
        parameters: &serde_json::Map<String, Json>,
        content: ContentFormat,
        format: ResultFormat,
        deadline: BodyDeadline,
    ) -> Result<Self, LoadError> {
        let body = serde_json::to_string(&serde_json::json!({
            "statement": cypher,
            "parameters": parameters,
        }))
        .map_err(|e| LoadError::Io(format!("serializing the request body failed: {e}")))?;
        Ok(Self {
            body_bytes: body.len() as u64,
            body,
            auth: basic_auth(&creds.username, &creds.password),
            content,
            format,
            deadline,
        })
    }

    /// POST this spec to one candidate URL (ADR-0633): any HTTP response
    /// proves the origin; a transport failure carries its delivery
    /// verdict via [`walk_failure`].
    fn send(&self, url: &str) -> Result<ureq::http::Response<ureq::Body>, WalkFailure> {
        let config = ureq::post(url)
            .header("Authorization", &self.auth)
            .header("Content-Type", self.content.content_type())
            .header("Accept", self.format.accept_header())
            .config()
            .http_status_as_error(false);
        let config = match self.deadline {
            BodyDeadline::Global => config.timeout_global(Some(QUERY_TIMEOUT)),
            BodyDeadline::ConnectOnly => config.timeout_connect(Some(QUERY_TIMEOUT)),
        };
        config.build().send(&self.body).map_err(walk_failure)
    }
}

/// The request prelude every Query API v2 POST shares — orchestration
/// only: build the request spec, resolve the endpoint, run the
/// cleartext verdict once on the parsed origin, walk the port variants
/// (ADR-0633), and map non-2xx into [`LoadError::Http`]. On 2xx,
/// returns the answering URL, the serialized request body's byte count,
/// and the body reader.
fn post_query(
    creds: &Credentials,
    cypher: &str,
    parameters: &serde_json::Map<String, Json>,
    allow_plaintext: bool,
    content: ContentFormat,
    format: ResultFormat,
    deadline: BodyDeadline,
) -> Result<(String, u64, impl Read), LoadError> {
    let spec = RequestSpec::new(creds, cypher, parameters, content, format, deadline)?;
    let endpoint = query_api(&creds.uri, &creds.database)?;
    // The cleartext policy (ADR-0122) reads only the endpoint's scheme
    // and host, which every port variant shares: one verdict before the
    // walk, on the already-parsed origin.
    ensure_cleartext_basic_allowed(&endpoint, allow_plaintext)?;
    let (url, resp) = first_responding_origin(&endpoint, |url| spec.send(url))?;
    let status = resp.status().as_u16();
    let reader = resp.into_body().into_reader();
    if !(200..300).contains(&status) {
        let text = read_response_body(reader, &url)?;
        return Err(LoadError::Http {
            status,
            reply: truncate_for_error(&text),
        });
    }
    Ok((url, spec.body_bytes, reader))
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
mod tests;
