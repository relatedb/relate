//! The ADR-0266 browser-Origin classification the network surfaces share:
//! the origin-shape parse, the loopback-host set, and the wildcard/exact
//! allowlist matcher, stated once here — `relate-serve` owns exactly
//! this class of cross-surface policy, beside the bind-posture
//! `is_loopback_host` (ADR-0248) — and consumed by bolt's WebSocket
//! upgrade guard (`OriginPolicy`) and the HTTP surfaces' browser guard
//! (`OriginAllowlist`) alike, so the classification cannot drift between
//! the surfaces (round 10, F-10-1).

/// The ADR-0266 loopback set: the hosts that name the local machine, each
/// with the spelling an Origin serializes it to (a reg-name or IPv4
/// literal as itself, the IPv6 literal bracketed). The loopback
/// classification compares one of the two projections — the bare host
/// ([`Origin::is_loopback`]) or the serialized spelling
/// ([`Origin::is_loopback_spelling`]) — never its own copy of the set.
const LOOPBACK_HOSTS: [(&str, &str); 3] = [
    ("localhost", "localhost"),
    ("127.0.0.1", "127.0.0.1"),
    ("::1", "[::1]"),
];

/// The hosted Neo4j web applications both browser-facing surfaces allow
/// beside the loopback set: the Neo4j Browser web application
/// (<https://browser.neo4j.io>, documented in the Neo4j Browser manual)
/// and the Neo4j (Aura) Console. The HTTP surface's `OriginAllowlist::
/// Default` and bolt's authenticated WebSocket default both consult this
/// one list, so the two surfaces cannot drift on which hosted apps work
/// out of the box. Extend it here — never with a per-surface copy.
pub const HOSTED_WEB_APP_ORIGINS: [&str; 2] =
    ["https://browser.neo4j.io", "https://console.neo4j.io"];

/// One parsed browser Origin (the ADR-0266 shape): `http(s)://host[:port]`
/// with no userinfo, no query, no fragment, at most an empty path, a
/// non-empty host, and a numeric-or-absent port. The parse yields the
/// pieces every consumer projects — the scheme, carried in its canonical
/// lowercase spelling, the host as written (the canonical serialization
/// lowercases it), the bracketed-host decision carried as data, and the
/// port (a `u16`, parsed once; an out-of-range port is outside the shape,
/// and a leading-zero spelling canonicalizes to the plain number) — so the loopback classification, the canonical
/// serialization, and the origin flags' grammars all read the same
/// parse, not one per surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Origin<'a> {
    scheme: Scheme,
    host: OriginHost<'a>,
    port: Option<u16>,
}

/// The origin shape's scheme: a two-element domain, decided once in
/// [`Origin::parse`] and rendered back by [`Origin::canonical`] — never
/// re-derived from text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scheme {
    Http,
    Https,
}

impl Scheme {
    /// The scheme named by `text`, case-insensitively (RFC 3986 §3.1).
    fn parse(text: &str) -> Option<Self> {
        [Self::Http, Self::Https]
            .into_iter()
            .find(|scheme| text.eq_ignore_ascii_case(scheme.as_str()))
    }

    /// The canonical lowercase spelling.
    fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }
}

/// The origin-shape parse's host token: whether the host is bracketed is
/// decided once, in [`Origin::parse`]'s
/// `match authority.strip_prefix('[')`, and carried as data instead of
/// one spelling of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OriginHost<'a> {
    /// A reg-name or IPv4 literal, as written (case preserved; the
    /// canonical rebuild lowercases it).
    Name(&'a str),
    /// A bracketed host — in practice an IPv6 literal, but any non-empty
    /// bracketed host reaches this arm, and the parse does not validate
    /// the literal as IPv6 (round 11, F-11-4). The token is the bracketed
    /// spelling the canonical serialization keeps, the one datum the
    /// parse carries: the bare host is the same slice between brackets.
    Bracketed(&'a str),
}

impl OriginHost<'_> {
    /// The host with brackets stripped — the projection the loopback set
    /// restricts.
    fn bare(&self) -> &str {
        match self {
            Self::Name(host) => host,
            Self::Bracketed(token) => token
                .strip_prefix('[')
                .and_then(|bare| bare.strip_suffix(']'))
                .expect("the parse spans at least the brackets"),
        }
    }

    /// The host spelling the canonical serialization keeps: as written
    /// for a name, brackets kept for a bracketed host.
    fn token(&self) -> &str {
        match self {
            Self::Name(host) => host,
            Self::Bracketed(token) => token,
        }
    }
}

impl<'a> Origin<'a> {
    /// The origin-shape parse — the one structural parse every surface
    /// consults, so the values the origin flags accept and the origins
    /// the guards classify are one grammar, not two: an `http(s)://`
    /// scheme, then the authority — `host[:port]`, no userinfo, no query,
    /// no fragment, at most an empty path — with a non-empty host and a
    /// `u16`-or-absent port (`parse_port_suffix`). The host arms own their
    /// emptiness (round 10, F-10-2): the name arm rejects an empty name
    /// (`http://`), the bracketed arm an empty bare host (`http://[]` —
    /// the brackets always span at least `[]`, so only the bare host
    /// inside them can be empty). `None` for a value outside the shape.
    pub fn parse(origin: &'a str) -> Option<Self> {
        // The scheme is read case-insensitively and carried in its
        // canonical lowercase spelling (RFC 3986 §3.1), exactly as the
        // host is: the scheme's case is not significant, so
        // `HTTP://localhost` and `http://localhost` are one Origin
        // (round 11, F-11-1).
        let (scheme, rest) = origin.split_once("://")?;
        let scheme = Scheme::parse(scheme)?;
        // An Origin serializes as scheme://host[:port] with at most an
        // empty path; refuse anything beyond that rather than guessing at
        // its meaning.
        let authority = match rest.split_once('/') {
            Some((authority, "")) => authority,
            None => rest,
            _ => return None,
        };
        // Userinfo, a query and a fragment are outside the shape (a query
        // or fragment with no path would surface here): refuse rather than
        // guess at its meaning (round 11, F-11-3).
        if authority.contains(['@', '?', '#']) {
            return None;
        }
        let (host, port_suffix) = match authority.strip_prefix('[') {
            // A bracketed host: the token spans the brackets (so the
            // canonical rebuild, [`Origin::canonical`], keeps them) and
            // the port suffix is whatever follows the closing bracket.
            Some(bracketed) => {
                // `close` indexes the `]` inside `bracketed`, so it is
                // also the bare host's length — zero is an empty host
                // (`http://[]`), which the shape refuses (round 10,
                // F-10-2).
                let close = bracketed.find(']')?;
                if close == 0 {
                    return None;
                }
                (
                    OriginHost::Bracketed(&authority[..close + 2]),
                    &bracketed[close + 1..],
                )
            }
            None => {
                let name = authority
                    .split(':')
                    .next()
                    .expect("str::split always yields one item");
                if name.is_empty() {
                    return None;
                }
                (OriginHost::Name(name), &authority[name.len()..])
            }
        };
        // After the host: a colon plus a `u16`'s digits only (a non-numeric
        // or out-of-range port makes the whole Origin invalid, not "any
        // port"). The arms above own the host's emptiness, so only the port
        // is checked here.
        let port = parse_port_suffix(port_suffix)?;
        Some(Self { scheme, host, port })
    }

    /// The canonical serialization (round 7, F-7-4): the parsed pieces
    /// rebuilt with the host lowercased, IPv6 brackets kept — the one
    /// spelling the exact matchers byte-compare, so a stored origin and
    /// the incoming Origin agree on one spelling.
    pub fn canonical(&self) -> String {
        let host = self.host.token().to_ascii_lowercase();
        let scheme = self.scheme.as_str();
        match self.port {
            Some(port) => format!("{scheme}://{host}:{port}"),
            None => format!("{scheme}://{host}"),
        }
    }

    /// True for the loopback hosts of the ADR-0266 set, on the bare-host
    /// projection (brackets stripped), case-insensitive on the name (RFC
    /// 3986 §3.2.2; the IP literals carry no letters to case-fold). The
    /// HTTP surfaces' form of the classification: a bracketed non-IPv6
    /// host (`[localhost]`) reaches this check as its bare host, so the
    /// set — not the brackets — decides.
    pub fn is_loopback(&self) -> bool {
        LOOPBACK_HOSTS
            .iter()
            .any(|(bare_host, _)| self.host.bare().eq_ignore_ascii_case(bare_host))
    }

    /// True when the Origin's serialized host spelling — brackets kept on
    /// IPv6 literals — is exactly one of the ADR-0266 loopback spellings.
    /// The WebSocket guard's form of the classification: a bracketed
    /// non-IPv6 host (`[localhost]`) and a non-lowercase name
    /// (`LOCALHOST`) are not spellings any loopback host serializes to,
    /// so they are rejected though their bare form names loopback. The
    /// callers parse once and project both this and
    /// [`Self::is_hosted_web_app`] from the same parse.
    pub fn is_loopback_spelling(&self) -> bool {
        LOOPBACK_HOSTS
            .iter()
            .any(|(_, spelling)| self.host.token() == *spelling)
    }

    /// True when the canonical serialization is one of the hosted Neo4j
    /// web-app origins ([`HOSTED_WEB_APP_ORIGINS`]), compared
    /// canonical-to-canonical so scheme and host case is insignificant.
    pub fn is_hosted_web_app(&self) -> bool {
        HOSTED_WEB_APP_ORIGINS.contains(&self.canonical().as_str())
    }
}

/// The port after the host: `Some(None)` for no port, `Some(Some(port))` for
/// a colon plus the digits of a `u16`, `None` for anything else (a
/// non-numeric, empty or out-of-range port invalidates the whole Origin).
fn parse_port_suffix(rest: &str) -> Option<Option<u16>> {
    if rest.is_empty() {
        return Some(None);
    }
    let digits = rest.strip_prefix(':')?;
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u16>().ok().map(Some)
}

/// The wildcard origin spelling: every Origin, stated once here — both
/// surfaces' origin flags resolve their `*` through this matcher.
pub const ORIGIN_WILDCARD: &str = "*";

/// One parsed origin-flag value — the wildcard or an exact origin URL —
/// the typed product both surfaces' origin flags parse through
/// ([`OriginFlagValue::parse`]), so their acceptance grammars and the
/// spellings they store cannot drift (round 11 follow-up, F-1-1/F-1-2).
/// The wildcard spelling is stated once, in [`ORIGIN_WILDCARD`]; no
/// builder re-tests strings for `*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OriginFlagValue {
    /// `*`: every Origin.
    Wildcard,
    /// An exact origin URL (`http://…` / `https://…`), stored in its
    /// canonical serialization.
    Url(String),
}

impl OriginFlagValue {
    /// The origin flags' validity grammar, owned beside the matcher the
    /// values feed: one value is an exact origin URL (`http://…` /
    /// `https://…`) or the wildcard `*`; a URL value must conform to the
    /// origin shape ([`Origin::parse`]) and is stored in its canonical
    /// serialization — scheme and host lowercased (RFC 3986 §3.1,
    /// §3.2.2), IPv6 brackets kept — so the exact matcher byte-compares
    /// one spelling. `Ok` is the typed value, `Err` the reason, phrased
    /// to read after the caller names its flag (`--origin {reason}`).
    pub fn parse(value: &str) -> Result<Self, String> {
        if value == ORIGIN_WILDCARD {
            return Ok(Self::Wildcard);
        }
        match Origin::parse(value) {
            Some(parsed) => Ok(Self::Url(parsed.canonical())),
            None => Err(format!(
                "expects an origin URL (http(s)://…) or *, got {value:?}"
            )),
        }
    }
}

/// The wildcard/exact matcher both surfaces resolve their origin flags
/// through: the wildcard allows every Origin; an exact list allows the
/// origins that canonicalize to one of its members. The matcher owns its
/// list (round 11, F-11-2): both surfaces store the resolved verdict, so
/// a borrowed form would only push each of them to restate the
/// wildcard/exact vocabulary in its own owned shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OriginMatch {
    /// Every Origin — the wildcard, resolved once by [`Self::resolve`].
    Any,
    /// Exactly the listed origins, in their canonical serialization.
    Exact(Vec<String>),
}

impl OriginMatch {
    /// Resolve a parsed origin-flag list — the one statement of the
    /// precedence both surfaces fold their default over: the wildcard
    /// anywhere in the list selects [`Self::Any`]; an empty list is
    /// `None` (the surface's own default applies); any other list is
    /// exact membership.
    pub fn resolve(values: &[OriginFlagValue]) -> Option<Self> {
        let mut urls = Vec::new();
        for value in values {
            match value {
                OriginFlagValue::Wildcard => return Some(Self::Any),
                OriginFlagValue::Url(url) => urls.push(url.clone()),
            }
        }
        if urls.is_empty() {
            None
        } else {
            Some(Self::Exact(urls))
        }
    }

    /// Whether `origin` is allowed, compared canonical-to-canonical.
    /// `None` is an Origin outside the origin shape: [`Self::Any`]
    /// allows it anyway (its documented risk), an exact list does not.
    pub fn allows(&self, origin: Option<&Origin<'_>>) -> bool {
        match self {
            Self::Any => true,
            Self::Exact(list) => origin.is_some_and(|origin| list.contains(&origin.canonical())),
        }
    }
}

// The classification's tests live beside it: the parse's shape matrix and
// the two loopback projections' decisions, the facts both surfaces pin
// through their own policy tests.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_yields_the_origin_shape_pieces() {
        let parsed = Origin::parse("http://localhost:7474").unwrap();
        assert_eq!(parsed.scheme, Scheme::Http);
        assert_eq!(parsed.host, OriginHost::Name("localhost"));
        assert_eq!(parsed.port, Some(7474));
        assert_eq!(parsed.canonical(), "http://localhost:7474");
        let parsed = Origin::parse("https://[::1]:8080").unwrap();
        assert_eq!(parsed.scheme, Scheme::Https);
        assert_eq!(parsed.host, OriginHost::Bracketed("[::1]"));
        assert_eq!(parsed.host.bare(), "::1");
        assert_eq!(parsed.port, Some(8080));
        assert_eq!(parsed.canonical(), "https://[::1]:8080");
        // The canonical rebuild lowercases the host, brackets kept:
        let parsed = Origin::parse("http://LOCALhost").unwrap();
        assert_eq!(parsed.canonical(), "http://localhost");
        // The scheme is case-insensitive (RFC 3986 §3.1) and canonical:
        let parsed = Origin::parse("HTTP://LOCALHOST:7474").unwrap();
        assert_eq!(parsed.scheme, Scheme::Http);
        assert_eq!(parsed.canonical(), "http://localhost:7474");
        let parsed = Origin::parse("hTTps://[::1]").unwrap();
        assert_eq!(parsed.scheme, Scheme::Https);
        assert_eq!(parsed.canonical(), "https://[::1]");
        // An empty path is the shape's one allowed path spelling:
        assert!(Origin::parse("https://host.example/").is_some());
    }

    #[test]
    fn the_port_is_a_u16_parsed_once() {
        // A leading-zero spelling canonicalizes to the number a browser
        // would emit; an out-of-range port is outside the shape.
        let parsed = Origin::parse("http://localhost:0080").unwrap();
        assert_eq!(parsed.port, Some(80));
        assert_eq!(parsed.canonical(), "http://localhost:80");
        assert_eq!(
            Origin::parse("http://localhost").unwrap().port,
            None,
            "no port is no port"
        );
        assert_eq!(
            Origin::parse("http://localhost:65535").unwrap().port,
            Some(65535)
        );
        for outside in [
            "http://localhost:65536",
            "http://localhost:99999",
            "http://[::1]:100000",
        ] {
            assert!(Origin::parse(outside).is_none(), "{outside}");
        }
    }

    #[test]
    fn parse_rejects_outside_the_shape() {
        for outside in [
            // Bare hosts and lookalike schemes:
            "localhost",
            "null",
            "browser.neo4j.io",
            "ftp://localhost",
            "https:/localhost",
            // Empty hosts — the name arm's emptiness (round 10, F-10-2):
            "http://",
            "https://:80",
            // The bracketed arm's emptiness (round 10, F-10-2):
            "http://[]",
            "http://[]:8080",
            // Paths, queries, fragments, userinfo:
            "http://host.example/path",
            "http://host.example/evil",
            "http://user@localhost",
            "http://host.example?query",
            // A fragment delimiter in a slash-less authority is a
            // rejected spelling, never a host character (round 11,
            // F-11-3):
            "http://localhost#evil",
            "http://host.example#frag",
            // Non-numeric ports:
            "http://localhost:abc",
            "http://localhost:",
            // An unclosed bracket:
            "http://[::1",
        ] {
            assert!(Origin::parse(outside).is_none(), "{outside}");
        }
    }

    #[test]
    fn loopback_classification_bare_projection() {
        // The HTTP surfaces' form: the bare host against the ADR-0266
        // set, case-insensitive on the name, brackets stripped.
        for loopback in [
            "http://localhost",
            "https://localhost:3000",
            "http://LOCALHOST/",
            "http://127.0.0.1:7474",
            "https://127.0.0.1",
            "http://[::1]:8080",
            "https://[::1]",
            // The bare projection strips the brackets, so a bracketed
            // spelling of a loopback IPv4 literal classifies on the set:
            "http://[127.0.0.1]",
        ] {
            let parsed = Origin::parse(loopback).unwrap();
            assert!(parsed.is_loopback(), "{loopback}");
        }
        for foreign in [
            "http://evil.example",
            "https://localhost.evil.example",
            "http://127.0.0.1.evil.example",
            "http://[::2]",
            "http://localhost.",
        ] {
            let parsed = Origin::parse(foreign).unwrap();
            assert!(!parsed.is_loopback(), "{foreign}");
        }
    }

    #[test]
    fn loopback_classification_serialized_spelling() {
        // The WebSocket guard's form: the serialized host spelling must
        // be exactly one of the set's spellings — brackets kept on the
        // IPv6 literal, the name as-written.
        for loopback in [
            "http://localhost",
            "https://localhost:3000",
            "http://127.0.0.1:8080",
            "https://127.0.0.1",
            "http://[::1]:9000",
            "https://[::1]",
        ] {
            let parsed = Origin::parse(loopback).unwrap();
            assert!(parsed.is_loopback_spelling(), "{loopback}");
        }
        for foreign in [
            // A non-lowercase name is not a loopback spelling:
            "http://LOCALHOST",
            // Bracketed non-IPv6 hosts serialize to no loopback spelling:
            "http://[localhost]",
            "http://[127.0.0.1]",
            "http://[::2]",
        ] {
            let parsed = Origin::parse(foreign).unwrap();
            assert!(!parsed.is_loopback_spelling(), "{foreign}");
        }
    }

    #[test]
    fn serialized_verdict_reads_the_scheme_case_insensitively() {
        // The WebSocket guard's verdict: the scheme is case-insensitive
        // (RFC 3986 §3.1), read by the shared parse itself — a
        // non-canonical scheme spelling classifies the same as its
        // canonical spelling — while everything else stays exact.
        for loopback in [
            "HTTP://localhost",
            "HTTPS://[::1]:8080",
            "hTTps://localhost",
        ] {
            assert!(
                Origin::parse(loopback).is_some_and(|parsed| parsed.is_loopback_spelling()),
                "{loopback}"
            );
        }
        for foreign in [
            // The scheme's case is forgiven; nothing else is:
            "HTTP://LOCALHOST",
            "HTTP://[localhost]",
            // Outside the shape entirely:
            "null",
            "localhost",
            "ftp://localhost",
            "HTTP://localhost/evil",
            "HTTP://user@localhost",
            "HTTP://localhost#evil",
            "HTTP://localhost?query",
        ] {
            assert!(
                !Origin::parse(foreign).is_some_and(|parsed| parsed.is_loopback_spelling()),
                "{foreign}"
            );
        }
    }

    #[test]
    fn hosted_web_app_classification_is_canonical() {
        for hosted in [
            "https://browser.neo4j.io",
            "https://console.neo4j.io",
            // The canonical rebuild lowercases scheme and host, so a
            // non-canonical spelling of the same origin matches, and an
            // empty path is the shape's only path spelling.
            "HTTPS://BROWSER.neo4j.io",
            "https://browser.neo4j.io/",
        ] {
            assert!(
                Origin::parse(hosted).is_some_and(|parsed| parsed.is_hosted_web_app()),
                "{hosted}"
            );
        }
        for foreign in [
            // A port makes it a different origin.
            "https://browser.neo4j.io:443",
            // A lookalike host is not the hosted app.
            "https://browser.neo4j.io.evil.example",
            "http://browser.neo4j.io",
            "https://evil.example",
            // A real path is outside the origin shape; an empty path is
            // the shape's one allowed path spelling and canonicalizes to
            // the same origin (asserted above).
            "https://browser.neo4j.io/app",
            // Outside the origin shape entirely.
            "browser.neo4j.io",
        ] {
            assert!(
                !Origin::parse(foreign).is_some_and(|parsed| parsed.is_hosted_web_app()),
                "{foreign}"
            );
        }
    }

    #[test]
    fn origin_match_resolves_wildcard_and_exact() {
        // An empty list is None — the surface's own default applies.
        assert_eq!(OriginMatch::resolve(&[]), None);
        // The wildcard anywhere in the list selects Any.
        assert_eq!(
            OriginMatch::resolve(&[
                OriginFlagValue::Url("https://a.example".to_string()),
                OriginFlagValue::Wildcard,
            ]),
            Some(OriginMatch::Any)
        );
        // Exact membership compares the canonical serialization: the
        // matcher owns the list it resolved, so a surface can store the
        // verdict, and a non-canonical spelling of the same origin
        // matches.
        let values = [OriginFlagValue::Url("https://a.example".to_string())];
        let exact = OriginMatch::resolve(&values).unwrap();
        assert_eq!(
            exact,
            OriginMatch::Exact(vec!["https://a.example".to_string()])
        );
        assert!(exact.allows(Origin::parse("https://a.example").as_ref()));
        assert!(exact.allows(Origin::parse("HTTPS://A.example").as_ref()));
        assert!(!exact.allows(Origin::parse("https://b.example").as_ref()));
        // Any allows every spelling, shape-valid or not.
        assert!(OriginMatch::Any.allows(Origin::parse("https://a.example").as_ref()));
        assert!(OriginMatch::Any.allows(None));
    }

    #[test]
    fn origin_flag_values_parse_canonically() {
        assert_eq!(
            OriginFlagValue::parse("*").unwrap(),
            OriginFlagValue::Wildcard
        );
        assert_eq!(
            OriginFlagValue::parse("HTTP://LOCALhost:7474").unwrap(),
            OriginFlagValue::Url("http://localhost:7474".to_string())
        );
        for outside in ["browser.neo4j.io", "ftp://x", "http://host.example#frag"] {
            assert!(OriginFlagValue::parse(outside).is_err(), "{outside}");
        }
    }
}
