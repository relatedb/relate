//! The browser-origin policy both HTTP surfaces run (ADR-0266): the
//! `Origin` allowlist and its `--origin` flag grammar, and the
//! browser-attack guard (CSRF / DNS rebinding) that applies it to every
//! state-changing request. The classification beneath the policy — the
//! origin-shape parse, the loopback set, and the wildcard/exact matcher —
//! is the shared network-policy one (`relate_serve::origin`),
//! consumed here and by bolt's WebSocket upgrade guard alike, so the
//! surfaces cannot drift. The transport ([`crate::http`]) runs the guard
//! inside its pre-body pipeline ([`crate::http::read_admitted_request`]); the policy
//! lives here, so the values the flag accepts and the origins the guard
//! classifies are one grammar, not two.

use crate::http::RequestHead;
use relate_serve::origin::Origin;
// The typed flag product and the matcher are the shared classification's:
// both surfaces' origin flags parse through them, so this crate re-exports
// them beside the policy that stores them.
pub use relate_serve::origin::{OriginFlagValue, OriginMatch};

/// The Origin allowlist for the browser guard and the CORS echo (the
/// browser-facing HTTP Query API surface; the agent-facing MCP transport
/// pins [`OriginAllowlist::LoopbackOnly`]).
pub enum OriginAllowlist {
    /// The MCP transport's fixed policy: loopback origins only (ADR-0266's
    /// set, any port, http/https).
    LoopbackOnly,
    /// The HTTP surface's built-in default: the loopback set **plus** the
    /// hosted Neo4j web apps — the Browser and the (Aura) Console, the
    /// shared `relate_serve::origin::HOSTED_WEB_APP_ORIGINS` list
    /// bolt's authenticated WebSocket default also consults (ADR-0459) —
    /// so they drive a local Relate out of the box.
    Default,
    /// `--origin *` or an explicit `--origin` list: the shared matcher's
    /// resolved verdict, stored owned (round 11, F-11-2). The wildcard
    /// carries the documented risk: with `--auth none`, any website the
    /// operator visits can drive the loopback server. An explicit list
    /// **replaces** the default: exactly these origin URLs match (loopback
    /// origins are no longer implied).
    Matched(OriginMatch),
}

impl OriginAllowlist {
    /// Build the policy from `relate serve --origin` values: none is
    /// [`OriginAllowlist::Default`]; otherwise the shared matcher's
    /// resolution — a wildcard anywhere is [`OriginMatch::Any`], any other
    /// list is exact and replaces the default (loopback origins are no
    /// longer implied). The precedence lives once, in
    /// [`OriginMatch::resolve`]; this builder folds only the HTTP
    /// surface's default over its `None`.
    pub fn from_flag_values(values: &[OriginFlagValue]) -> Self {
        match OriginMatch::resolve(values) {
            Some(matched) => Self::Matched(matched),
            None => Self::Default,
        }
    }

    /// The `--origin` flag value's validity grammar — the CLI's
    /// `serve --origin` parse delegates to it, so the accepted values and
    /// the allowlist they build cannot drift (round 3, F-1-9; round 4,
    /// F-4-2). The grammar itself is the shared one
    /// ([`OriginFlagValue::parse`]): one value is an exact origin URL
    /// (`http://…` / `https://…`) or the wildcard `*`, a URL value must
    /// conform to the origin shape ([`Origin::parse`]), and the stored
    /// `Url` payload is the canonical serialization the exact matcher
    /// compares (round 11, F-11-1/F-11-3). `Ok` is the typed value every
    /// builder downstream consumes, `Err` the reason, phrased to read
    /// after the caller names its flag (`serve --origin {reason}`).
    ///
    /// ```
    /// use relate_http::http::{OriginAllowlist, OriginFlagValue};
    ///
    /// assert_eq!(
    ///     OriginAllowlist::parse_flag_value("*").unwrap(),
    ///     OriginFlagValue::Wildcard
    /// );
    /// assert_eq!(
    ///     OriginAllowlist::parse_flag_value("https://tool.example").unwrap(),
    ///     OriginFlagValue::Url("https://tool.example".to_string())
    /// );
    /// assert_eq!(
    ///     OriginAllowlist::parse_flag_value("http://localhost:7474").unwrap(),
    ///     OriginFlagValue::Url("http://localhost:7474".to_string())
    /// );
    /// // The scheme and host are canonicalized — lowercased, IPv6
    /// // brackets kept — so the exact matcher byte-compares one spelling:
    /// assert_eq!(
    ///     OriginAllowlist::parse_flag_value("HTTP://LOCALhost:7474").unwrap(),
    ///     OriginFlagValue::Url("http://localhost:7474".to_string())
    /// );
    /// assert_eq!(
    ///     OriginAllowlist::parse_flag_value("https://[::1]:8080").unwrap(),
    ///     OriginFlagValue::Url("https://[::1]:8080".to_string())
    /// );
    /// // A bare host or a lookalike scheme is rejected:
    /// assert!(OriginAllowlist::parse_flag_value("browser.neo4j.io").is_err());
    /// assert!(OriginAllowlist::parse_flag_value("ftp://tool.example").is_err());
    /// // So is a URL outside the origin shape: an empty host, a path,
    /// // userinfo, a query, a fragment, a non-numeric port.
    /// assert!(OriginAllowlist::parse_flag_value("https://").is_err());
    /// assert!(OriginAllowlist::parse_flag_value("http://host.example/path").is_err());
    /// assert!(OriginAllowlist::parse_flag_value("http://user@host.example").is_err());
    /// assert!(OriginAllowlist::parse_flag_value("http://host.example?query").is_err());
    /// assert!(OriginAllowlist::parse_flag_value("http://host.example#frag").is_err());
    /// assert!(OriginAllowlist::parse_flag_value("http://host.example:x").is_err());
    /// ```
    pub fn parse_flag_value(value: &str) -> Result<OriginFlagValue, String> {
        OriginFlagValue::parse(value)
    }

    /// Whether the policy allows `origin`.
    pub fn allows(&self, origin: &str) -> bool {
        // One equivalence relation over the incoming Origin (round 9,
        // F-9-4): the shared origin-shape parse ([`Origin::parse`]) runs
        // once at the top, and every arm compares canonical-to-canonical
        // — the loopback arms on the shared ADR-0266 classification
        // ([`Origin::is_loopback`]), the stored matcher's entries against
        // the canonical rebuild ([`Origin::canonical`], the spelling
        // [`OriginFlagValue::parse`] stores). An Origin outside the
        // shape is invalid — never "allowed anyway" — except under `*`,
        // whose shape-agnostic verdict the matcher itself states (its
        // documented risk).
        let parsed = Origin::parse(origin);
        match self {
            Self::LoopbackOnly => parsed.is_some_and(|parsed| parsed.is_loopback()),
            Self::Default => {
                parsed.is_some_and(|parsed| parsed.is_loopback() || parsed.is_hosted_web_app())
            }
            Self::Matched(matched) => matched.allows(parsed.as_ref()),
        }
    }
}

/// Why [`browser_guard`] rejected a request: the status and message for the
/// error body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuardRejection {
    /// The HTTP status (403 or 415).
    pub status: u16,
    /// The human-readable reason.
    pub message: &'static str,
}

/// The browser-attack guard both HTTP surfaces apply identically, after the
/// headers parse and before auth or the body read (CSRF / DNS rebinding
/// against a loopback server). It classifies the method once
/// ([`method_scope`]) and applies each scope's rules: the surface's `POST`
/// statement routes and its `DELETE` rollback route are both state-changing
/// and origin-guarded (ADR-0455). A safe method is exempt: a `GET` carries
/// no body and is CSRF-safe, and `GET /` (the public discovery document)
/// must stay reachable from browser tooling on loopback.
///
/// - Every state-changing method ([`origin_guard`]): a present `Origin`
///   must be allowed by the surface's [`OriginAllowlist`] (the HTTP surface
///   defaults to loopback — any port, http/https — plus the hosted Browser
///   and Console origins; the MCP transport pins loopback only). A missing
///   Origin is a non-browser client and is allowed. Anything else is a
///   `403` — and carries no CORS header, so the browser cannot read the
///   response either.
/// - A `POST` additionally ([`post_media_type_guard`]) must not carry a
///   browser-"simple" content type (`text/plain`, the form types, or none
///   at all): those are the types a cross-origin form can send without a
///   preflight, which is the CSRF property this guard owns (ADR-0425). The
///   rule is deliberately **POST-only**: a form can only submit as `POST`,
///   and a `DELETE` (the rollback route) carries no body. The **media-type
///   vocabulary** — which types are valid statement request types and what
///   each decodes — is the statement route's
///   (`typed_params::request_dialect`): the guard passes every non-simple
///   type through to the route, which rejects unsupported request types
///   with a 400 naming the accepted spellings. Only the simple set is a
///   `415` here.
pub fn browser_guard(head: &RequestHead, origins: &OriginAllowlist) -> Result<(), GuardRejection> {
    match method_scope(&head.method) {
        MethodScope::Safe => Ok(()),
        MethodScope::StateChanging => origin_guard(head, origins),
        MethodScope::Post => origin_guard(head, origins).and_then(|()| post_media_type_guard(head)),
    }
}

/// The guard's method scope, classified once from the method name so each
/// rule reads as belonging to the scope that owns it: safe methods carry
/// none, every state-changing method carries the Origin rule, and `POST`
/// additionally carries the browser-"simple" content-type rule.
enum MethodScope {
    /// RFC 9110 §9.2.1's safe methods: `GET`, `HEAD`, `OPTIONS`, `TRACE`.
    Safe,
    /// A state-changing method that is not `POST` — the rollback `DELETE`
    /// today, plus any future or extension unsafe method.
    StateChanging,
    /// `POST`: state-changing and the only method a cross-origin HTML form
    /// can submit.
    Post,
}

/// Classify `method` once (RFC 9110 §9.1: names are case-sensitive, so the
/// match is exact). Anything outside the safe set that is not `POST` is
/// state-changing and origin-checked, so a lowercase or unknown spelling
/// fails closed, never open; a new state-changing route or extension-host
/// method is guarded by construction instead of by joining a list.
fn method_scope(method: &str) -> MethodScope {
    match method {
        "GET" | "HEAD" | "OPTIONS" | "TRACE" => MethodScope::Safe,
        "POST" => MethodScope::Post,
        _ => MethodScope::StateChanging,
    }
}

/// The origin rule every state-changing method carries: a present `Origin`
/// must be allowed by the surface's allowlist. A missing Origin is a
/// non-browser client and passes; anything else is a `403`.
fn origin_guard(head: &RequestHead, origins: &OriginAllowlist) -> Result<(), GuardRejection> {
    let Some(origin) = head.header("origin") else {
        return Ok(());
    };
    if origins.allows(origin) {
        return Ok(());
    }
    Err(GuardRejection {
        status: 403,
        message: "origin is not allowed",
    })
}

/// `POST`'s browser-"simple" content-type rule (ADR-0425): the types a
/// cross-origin form can send without a preflight are the CSRF property
/// this guard owns. The valid-request-type vocabulary stays the statement
/// route's.
fn post_media_type_guard(head: &RequestHead) -> Result<(), GuardRejection> {
    if !is_browser_simple_media_type(head.header("content-type")) {
        return Ok(());
    }
    Err(GuardRejection {
        status: 415,
        message: "the request content type is browser-\"simple\" — a cross-origin form cannot send a non-simple type without a preflight",
    })
}

/// Whether a Content-Type header value is a browser-"simple" media type
/// (or absent, which a form-defaulted POST is): `text/plain`,
/// `application/x-www-form-urlencoded`, `multipart/form-data` — the set a
/// cross-origin form can send without a preflight. The CSRF property,
/// and only it (ADR-0425): the valid-request-type vocabulary lives in the
/// statement route's dialect resolver, not here.
fn is_browser_simple_media_type(content_type: Option<&str>) -> bool {
    match content_type {
        None => true,
        Some(value) => {
            let media_type = crate::http::lowercased_media_type(value);
            matches!(
                media_type.as_str(),
                "text/plain" | "application/x-www-form-urlencoded" | "multipart/form-data"
            )
        }
    }
}

// The origin-shape parse, the loopback set, and the canonical
// serialization live in the shared network-policy layer
// (`relate_serve::origin`) — bolt's WebSocket guard consults the
// same classification, so this module states only the HTTP surfaces'
// policy over it.

// The allowlist and guard tests live in this module beside the policy
// they pin (the content_encoding.rs pattern); request heads come from the
// one in-crate fixture (`crate::http::tests::head_at`).
#[cfg(test)]
mod tests {
    use super::*;

    use crate::http::tests::head_at;

    /// RFC 9110's unsafe methods — the vocabulary the guard's
    /// state-changing policies are pinned over. One list, so the origin rule
    /// and the POST-only content-type rule are exercised over the same set.
    const UNSAFE_METHODS: [&str; 5] = ["POST", "PUT", "PATCH", "DELETE", "CONNECT"];

    /// [`head_at`] at the Query API v2 statement route: [`browser_guard`]
    /// reads only the method and the headers, so the path is fixed and
    /// never inspected by these tests.
    fn head(method: &str, headers: &[(&str, &str)]) -> RequestHead {
        head_at(method, "/db/neo4j/query/v2", headers)
    }

    #[test]
    fn browser_guard_rejects_only_browser_simple_content_types_on_post() {
        // A browser form POST carries no JSON content type.
        assert_eq!(
            browser_guard(&head("POST", &[]), &OriginAllowlist::Default,)
                .unwrap_err()
                .status,
            415,
            "a browser form POST carries no JSON content type"
        );
        assert_eq!(
            browser_guard(
                &head("POST", &[("content-type", "text/plain")]),
                &OriginAllowlist::Default,
            )
            .unwrap_err()
            .status,
            415
        );
        // The form types are simple too.
        for simple in ["application/x-www-form-urlencoded", "multipart/form-data"] {
            assert_eq!(
                browser_guard(
                    &head("POST", &[("content-type", simple)]),
                    &OriginAllowlist::Default,
                )
                .unwrap_err()
                .status,
                415,
                "{simple} is browser-simple"
            );
        }
        // Every non-simple type passes the guard (ADR-0425: the guard owns
        // only the CSRF property; the statement route's dialect resolver
        // owns the vocabulary and its 400s) — the JSON types, the
        // Query API family, and unknown types the route will reject alike.
        for passes in [
            "application/json",
            "application/json; charset=utf-8",
            "application/vnd.neo4j.query.v1.2; charset=utf-8",
            "application/vnd.neo4j.query",
            "application/xml",
        ] {
            assert!(
                browser_guard(
                    &head("POST", &[("content-type", passes)]),
                    &OriginAllowlist::Default,
                )
                .is_ok(),
                "{passes} is not browser-simple: the route owns its vocabulary"
            );
        }
        // The rule is POST-specific: none of the other unsafe methods is
        // held to it — a form cannot submit them, and the DELETE carries no
        // body. POST's own pass case is covered above.
        for method in UNSAFE_METHODS {
            if method == "POST" {
                continue;
            }
            assert!(
                browser_guard(
                    &head(method, &[("content-type", "text/plain")]),
                    &OriginAllowlist::Default
                )
                .is_ok(),
                "{method} is not held to the POST content-type rule"
            );
        }
        assert!(browser_guard(&head("GET", &[]), &OriginAllowlist::Default).is_ok());
    }

    #[test]
    fn origin_allowlist_matrix() {
        let default = OriginAllowlist::Default;
        assert!(default.allows("http://localhost:3000"));
        assert!(default.allows("http://127.0.0.1"));
        assert!(default.allows("https://[::1]:8080"));
        // The hosted Neo4j web apps work out of the box.
        assert!(default.allows("https://browser.neo4j.io"));
        assert!(default.allows("https://console.neo4j.io"));
        assert!(!default.allows("https://evil.example"));
        // An explicit list replaces the default: loopback is no longer
        // implied, and the hosted Browser is dropped.
        let exact = OriginAllowlist::from_flag_values(&[OriginFlagValue::Url(
            "https://tool.example".to_string(),
        )]);
        assert!(exact.allows("https://tool.example"));
        assert!(!exact.allows("https://browser.neo4j.io"));
        assert!(!exact.allows("http://localhost:3000"));
        // '*' anywhere allows everything; no flags is the default.
        assert!(
            OriginAllowlist::from_flag_values(&[OriginFlagValue::Wildcard])
                .allows("https://anything")
        );
        assert!(matches!(
            OriginAllowlist::from_flag_values(&[]),
            OriginAllowlist::Default
        ));
        // The MCP transport's fixed policy: loopback only.
        assert!(OriginAllowlist::LoopbackOnly.allows("http://localhost"));
        assert!(!OriginAllowlist::LoopbackOnly.allows("https://browser.neo4j.io"));
    }

    #[test]
    fn browser_guard_exempts_every_safe_method() {
        // RFC 9110 §9.2.1's safe methods are CSRF-safe: even a foreign
        // Origin on one passes (the public discovery document must stay
        // reachable from browser tooling, and a preflight is the browser's
        // own gate).
        for method in ["GET", "HEAD", "OPTIONS", "TRACE"] {
            let h = head(method, &[("origin", "https://evil.example")]);
            assert!(
                browser_guard(&h, &OriginAllowlist::Default).is_ok(),
                "{method} is safe"
            );
        }
        // The classification is case-sensitive (RFC 9110 §9.1): a lowercase
        // spelling is not `GET`, reaches the surface as an unknown method,
        // and is checked like any unsafe method — the verdict fails closed.
        let h = head("get", &[("origin", "https://evil.example")]);
        assert_eq!(
            browser_guard(&h, &OriginAllowlist::Default)
                .unwrap_err()
                .status,
            403
        );
    }

    #[test]
    fn browser_guard_origin_checks_every_state_changing_method() {
        // The server answers POST (statement/tx routes) and DELETE (rollback)
        // today; the guard covers those and any future unsafe method, not a
        // hand-maintained list (ADR-0455).
        for method in UNSAFE_METHODS {
            // A POST also carries a non-simple type, so the content-type
            // rule does not mask the origin verdict under test.
            let headers = |origin: Option<&'static str>| {
                let mut headers = Vec::new();
                if method == "POST" {
                    headers.push(("content-type", "application/json"));
                }
                if let Some(origin) = origin {
                    headers.push(("origin", origin));
                }
                headers
            };
            // A missing Origin is a non-browser client and passes.
            assert!(
                browser_guard(&head(method, &headers(None)), &OriginAllowlist::Default).is_ok(),
                "{method} without an Origin is a non-browser client"
            );
            // A foreign Origin is rejected.
            let foreign = head(method, &headers(Some("https://evil.example")));
            assert_eq!(
                browser_guard(&foreign, &OriginAllowlist::Default)
                    .unwrap_err()
                    .status,
                403,
                "{method} with a foreign Origin"
            );
            // An allowlisted loopback Origin passes.
            let allowed = head(method, &headers(Some("http://localhost:3000")));
            assert!(
                browser_guard(&allowed, &OriginAllowlist::Default).is_ok(),
                "{method} with an allowlisted Origin"
            );
        }
    }

    #[test]
    fn browser_guard_allows_only_loopback_origins() {
        // A missing Origin is a non-browser client and passes.
        assert!(browser_guard(
            &head("POST", &[("content-type", "application/json")]),
            &OriginAllowlist::Default,
        )
        .is_ok());
        for allowed in [
            "http://localhost",
            "https://localhost:3000",
            "http://127.0.0.1:7474",
            "https://127.0.0.1",
            "http://[::1]:8080",
            "https://[::1]",
            "http://LOCALHOST/",
            // The scheme is case-insensitive (RFC 3986 §3.1):
            "HTTP://localhost:3000",
        ] {
            let h = head(
                "POST",
                &[("content-type", "application/json"), ("origin", allowed)],
            );
            assert!(
                browser_guard(&h, &OriginAllowlist::Default).is_ok(),
                "{allowed}"
            );
        }
        for foreign in [
            "http://evil.example",
            "https://localhost.evil.example",
            "http://127.0.0.1.evil.example",
            "http://user@localhost",
            "http://localhost:abc",
            "http://localhost/evil",
            // A fragment delimiter in a slash-less authority is outside
            // the shape, never a host character (round 11, F-11-3):
            "http://localhost#evil",
            "file://localhost",
            "http://[::2]",
            "null",
        ] {
            let h = head(
                "POST",
                &[("content-type", "application/json"), ("origin", foreign)],
            );
            let rejection = browser_guard(&h, &OriginAllowlist::Default).unwrap_err();
            assert_eq!(rejection.status, 403, "{foreign}");
        }
    }
}
