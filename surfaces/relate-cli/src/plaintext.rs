//! The CLI's one URL-string vocabulary (LABS-872, REL8-2381): the
//! authority→host extraction, the loopback-host predicates, and the
//! generic URL splits (`scheme`, `is_http_scheme`, `path_suffix`,
//! `last_segment`) — the transport gates and the path rules (the remote
//! store's `.rl` requirement, the extension install's manifest rule) each
//! state their policy on top. Ungated, so the refusals read the same in
//! every build.
//!
//! Each gate keeps its own policy *on top* of this vocabulary — a per-ADR
//! narrowing is an explicit extra check at its own site, never a fresh copy
//! of the spelling:
//!
//! - the telemetry endpoint override ([`crate::telemetry`], ADR-0263)
//!   accepts the named loopback forms plus exactly `127.0.0.1`;
//! - the remote `--database` fetch ([`crate::remote_db`], ADR-0496) accepts
//!   the whole `127.0.0.0/8` range, like `pull`'s plaintext opt-in
//!   (ADR-0122, in `relate-load`, whose own helper predates this one).
//!
//! The network surfaces' predicate (`relate_serve::is_loopback_host`)
//! is the same-vocabulary twin across a crate boundary; it is not reachable
//! from this crate's fetch paths without the `serve` feature, so the
//! vocabulary is stated once here instead. One in-crate gate deliberately
//! stays on its own form: the release channel's
//! `plain_http_allowed` (ADR-0358's trust acts) predates this module and
//! refuses `::1` (its carve-out is `localhost` and `127.0.0.0/8` only,
//! plus a userinfo refusal of its own), so migrating it would widen a
//! security-gated trust rule — a change for its own decision, not a
//! ride-along.

/// The host of a URL authority (the part after `://`): drops any
/// `userinfo@`, the trailing `:port`, any path/query/fragment, and the
/// brackets around an IPv6 literal. The empty string is the honest answer
/// for an authority with no host (`http:///x`).
pub(crate) fn authority_host(authority: &str) -> &str {
    // Every `split`/`split_once` here yields at least one piece, so each
    // `unwrap_or_default` is the honest empty piece — never a fabricated
    // fallback over a whole input the split already covered.
    let host_port = authority.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = host_port
        .rsplit_once('@')
        .map_or(host_port, |(_, host)| host);
    if let Some(rest) = host_port.strip_prefix('[') {
        return rest.split(']').next().unwrap_or_default();
    }
    host_port.split(':').next().unwrap_or_default()
}

/// The scheme of a `scheme://…` URL, exactly as written; `None` when the
/// value carries no `://`. Schemes compare ASCII-case-insensitively
/// (RFC 3986), so every caller compares with `eq_ignore_ascii_case` —
/// this is the crate's one scheme extraction (the plaintext/loopback
/// gates, the remote `--load` grammar, and the extension install's
/// DIR-or-URL classifier all read it).
pub(crate) fn split_scheme(url: &str) -> Option<(&str, &str)> {
    url.split_once("://")
}

/// The scheme of a `scheme://…` URL, exactly as written; `None` when the
/// value carries no `://`.
pub(crate) fn scheme(url: &str) -> Option<&str> {
    split_scheme(url).map(|(scheme, _)| scheme)
}

/// Whether `url`'s scheme is `http` or `https` — the crate's one http(s)
/// test.
pub(crate) fn is_http_scheme(url: &str) -> bool {
    scheme(url).is_some_and(|scheme| {
        scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
    })
}

/// A URL's path part and its trailing query/fragment: the split every
/// last-path-segment rule makes before it looks at the segment (the
/// remote store's `.rl` requirement and the extension install's
/// manifest-or-directory rule). The suffix is returned rather than
/// dropped, so a caller that appends to the path can keep it.
pub(crate) fn path_suffix(url: &str) -> (&str, &str) {
    match url.find(['?', '#']) {
        Some(idx) => (&url[..idx], &url[idx..]),
        None => (url, ""),
    }
}

/// The last path segment of a URL, query and fragment stripped — the
/// segment every last-path-segment rule looks at ([`path_suffix`]'s path,
/// then its final `/`).
pub(crate) fn last_segment(url: &str) -> &str {
    let (path, _) = path_suffix(url);
    path.rsplit('/')
        .next()
        .expect("rsplit yields at least one item")
}

/// The **named** loopback hosts: `localhost` in any case, and the IPv6
/// literal `::1`. A gate that accepts exactly the well-known loopback names
/// (the telemetry sink, ADR-0263) states its policy as this plus its own
/// address.
pub(crate) fn is_loopback_name(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host == "::1"
}

/// The **wide** loopback rule: the named hosts plus any IPv4 address in
/// `127.0.0.0/8`. The std parser is strict (it rejects leading zeros, `+`,
/// octal/hex, and short forms), so `0127.0.0.1` never reads as loopback.
pub(crate) fn is_loopback_host(host: &str) -> bool {
    if is_loopback_name(host) {
        return true;
    }
    host.parse::<std::net::Ipv4Addr>()
        .map(|addr| addr.is_loopback())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authority_host_drops_userinfo_port_and_brackets() {
        for (authority, host) in [
            ("example.com/store.rl", "example.com"),
            ("example.com:8443/store.rl", "example.com"),
            ("user:pw@example.com/store.rl", "example.com"),
            ("example.com?q=1", "example.com"),
            ("example.com#frag", "example.com"),
            ("[::1]:8080/store.rl", "::1"),
            ("[fe80::1]/store.rl", "fe80::1"),
            ("/only-a-path", ""),
            ("", ""),
        ] {
            assert_eq!(authority_host(authority), host, "{authority:?}");
        }
    }

    #[test]
    fn the_named_and_wide_loopback_rules_differ_exactly_on_addresses() {
        for host in ["localhost", "LOCALHOST", "::1"] {
            assert!(is_loopback_name(host), "{host}");
            assert!(is_loopback_host(host), "{host}");
        }
        for host in ["127.0.0.1", "127.9.9.9"] {
            assert!(!is_loopback_name(host), "{host} is an address, not a name");
            assert!(is_loopback_host(host), "{host}");
        }
        for host in ["10.0.0.1", "0127.0.0.1", "127.0.0", "example.com", ""] {
            assert!(!is_loopback_name(host), "{host}");
            assert!(!is_loopback_host(host), "{host}");
        }
    }
}
