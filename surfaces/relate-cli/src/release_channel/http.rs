//! The one HTTP client the channel's download routes share (ADR-0250 §5,
//! ADR-0358 §6): the asset size cap, the typed [`DownloadError`] the route
//! probe matches on, the named-field [`AssetRequest`], and
//! [`download_asset`] itself. Every request is issued through an agent
//! built by [`http_agent`], which pins `RedirectAuthHeaders::Never` in
//! this module's own configuration: the bearer token never follows a
//! redirect to the asset CDN (ADR-0358 §6). The direct probe in
//! [`super::download_route`] and the API route in [`super::api_assets`]
//! download through this module; neither builds a request of its own.
//! `extension install --url` (ADR-0431 §6) fetches its tarball here too —
//! the same cap and the same client, no token involved — as do the two
//! website metadata reads: the `releases.json` document (`upgrade`,
//! ADR-0250 §1) and the `extensions.json` channel index (ADR-0431 §4),
//! both public origins that take no token (ADR-0250 §5).

use ureq::config::RedirectAuthHeaders;

use super::ChannelError;

/// Download size cap for a release asset: ureq's default body limit is
/// 10MB and the release binary is larger than that. The extension
/// tarball rides the same cap (ADR-0431).
const MAX_ASSET_BYTES: u64 = 512 * 1024 * 1024;

/// A failed asset download, typed so the route probe can recognize the one
/// status that selects the API route (ADR-0358 §5) without string matching
/// on an error message.
#[derive(Debug)]
pub(crate) enum DownloadError {
    /// The server answered a non-2xx status to the request.
    Status {
        /// The request URL, part of the pinned user-facing text.
        url: String,
        /// The HTTP status code.
        code: u16,
    },
    /// Any other failure — transport, protocol, or body read — already
    /// labeled with its user-facing text.
    Other(ChannelError),
}

impl DownloadError {
    /// The transport-failure arm of [`download_asset`] (the request itself
    /// failed: connection refused, DNS, TLS). Labeled with the request URL,
    /// like every arm — the premise a consumer's one-line renderer rests on.
    /// An associated constructor, not an inline closure, so a test produces
    /// the error the client produces instead of copying its text.
    pub(crate) fn transport(url: &str, e: impl std::fmt::Display) -> DownloadError {
        DownloadError::Other(ChannelError::new(format!("GET {url}: {e}")))
    }

    /// The body-read failure arm of [`download_asset`] (the response body
    /// failed mid-read). Same URL-labeling contract as [`Self::transport`].
    pub(crate) fn body_read(url: &str, name: &str, e: impl std::fmt::Display) -> DownloadError {
        DownloadError::Other(ChannelError::new(format!(
            "GET {url}: downloading {name}: {e}"
        )))
    }

    /// The user-facing [`ChannelError`]: a status keeps the exact
    /// `GET {url}: http status: {code}` text the direct route always
    /// printed. This is the one source of that pinned line; callers append
    /// advice to the [`std::fmt::Display`] text rather than re-deriving
    /// the status line.
    pub(crate) fn into_channel_error(self) -> ChannelError {
        match self {
            DownloadError::Status { url, code } => {
                ChannelError::new(format!("GET {url}: http status: {code}"))
            }
            DownloadError::Other(err) => err,
        }
    }
}

/// One asset request over the shared HTTP client. Named fields, so the
/// optional bearer `token` and the optional `accept` media type — both
/// `Option<&str>` — cannot be transposed at a call site.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AssetRequest<'a> {
    /// The absolute URL to GET.
    pub(crate) url: &'a str,
    /// The asset name, labeling a body-read failure.
    pub(crate) name: &'a str,
    /// `Authorization: Bearer` header value, when set (ADR-0250 §5).
    pub(crate) token: Option<&'a str>,
    /// `Accept` header override, when set.
    pub(crate) accept: Option<&'a str>,
}

/// The plain-http carve-out: a `http://` URL is transport-acceptable only
/// when its host is the loopback interface (`127.0.0.1`, `[::1]`,
/// `localhost`) — there is no on-path attacker between a process and its
/// own loopback, and the test fixtures' scripted servers live there. Every
/// other plain-http URL (an https origin redirecting to plaintext, a
/// shortener, a misconfigured host) is refused rather than downgraded,
/// which is the one integrity leg the unsigned third-party tier
/// (`extension install --url`, ADR-0431 §6) has (LABS-470 security
/// review). An authority carrying userinfo (`user:pass@host`) is a
/// refusal, never a parse: the tier's URLs never carry userinfo, and a
/// spelling like `http://localhost:80@evil.com/` must not reach the host
/// split at all. The `--url` gate in `extensions::install` consults
/// [`approved_transport`], the one predicate over this carve-out, so the
/// two spellings of the rule cannot drift.
pub(crate) fn plain_http_allowed(url: &str) -> bool {
    let rest = match url.strip_prefix("http://") {
        Some(rest) => rest,
        None => return false,
    };
    let authority = rest.split_once(['/', '?', '#']).map_or(rest, |(a, _)| a);
    if authority.contains('@') {
        return false;
    }
    // A bracketed IPv6 literal is the host whole; every other authority is
    // host-up-to-colon (the port).
    let host = match authority.strip_prefix('[') {
        Some(bracketed) => {
            return bracketed
                .split_once(']')
                .is_some_and(|(literal, _)| literal == "::1")
        }
        None => authority.split_once(':').map_or(authority, |(h, _)| h),
    };
    matches!(host, "127.0.0.1" | "localhost")
        || host.strip_prefix("127.").is_some_and(|octets| {
            octets.split('.').count() == 3 && octets.split('.').all(|o| o.parse::<u8>().is_ok())
        })
}

/// The one transport-acceptance predicate: an https URL, or the loopback
/// plain-http carve-out — consulted by the `--url` gate in
/// `extensions::install` and by [`http_agent`]'s `https_only` pin, so the
/// rule exists once.
pub(crate) fn approved_transport(url: &str) -> bool {
    url.starts_with("https://") || plain_http_allowed(url)
}

/// The module's one HTTP client: every request [`download_asset`] issues
/// goes through an agent built here, with `RedirectAuthHeaders::Never`
/// pinned in this module's own configuration — the `Authorization` header
/// is never attached to a request that follows a redirect, so the bearer
/// token cannot reach the asset CDN the API asset endpoint redirects to
/// (ADR-0358 §6) — and, for every URL that is not a loopback plain-http
/// carve-out ([`plain_http_allowed`]), `https_only` pins every hop of the
/// redirect chain to https. Built per call, as the use-once client was
/// before it, so the pins are the only change to the request path.
fn http_agent(url: &str) -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            // https_only is the redirect-downgrade pin, not the gate: an
            // https request keeps it (a redirect to plaintext refuses),
            // and only the loopback plain-http carve-out releases it.
            .https_only(!plain_http_allowed(url))
            .redirect_auth_headers(RedirectAuthHeaders::Never)
            .build(),
    )
}

/// Download one asset as raw bytes over the one HTTP client every route
/// shares. `accept`, when set, overrides the `Accept` header; `token`, when
/// set, is sent as `Authorization: Bearer` (ADR-0250 §5) on this request
/// only — this module pins `RedirectAuthHeaders::Never` on its client, so
/// the token is never forwarded to the asset CDN the API route redirects
/// to (ADR-0358 §6). The status is kept typed so the probe can match on it
/// (ADR-0358 §5).
pub(crate) fn download_asset(request: AssetRequest<'_>) -> Result<Vec<u8>, DownloadError> {
    let AssetRequest {
        url,
        name,
        token,
        accept,
    } = request;
    let mut request = http_agent(url).get(url);
    if let Some(accept) = accept {
        request = request.header("Accept", accept);
    }
    if let Some(token) = token {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    let mut body = request
        .call()
        .map_err(|e| match e {
            ureq::Error::StatusCode(code) => DownloadError::Status {
                url: url.to_string(),
                code,
            },
            // http: the asset download request failed.
            other => DownloadError::transport(url, other),
        })?
        .into_body();
    body.with_config()
        .limit(MAX_ASSET_BYTES)
        .read_to_vec()
        // http: the asset download failed.
        .map_err(|e| DownloadError::body_read(url, name, e))
}

#[cfg(test)]
mod tests {
    use super::{http_agent, plain_http_allowed};
    use ureq::config::RedirectAuthHeaders;

    /// The agent's transport pins, asserted on the built config: redirects
    /// never carry the bearer token (ADR-0358 §6), and every non-loopback
    /// URL's agent refuses a plaintext downgrade — the third-party tier's
    /// one integrity leg (LABS-470 security review). A loopback plain-http
    /// URL (the fixture servers) is the one carve-out.
    #[test]
    fn agent_pins_https_only_and_redirect_auth() {
        for url in [
            "https://relate.org/extensions.json",
            "http://example.com/ext.tar.gz",
            "http://10.0.0.1/ext.tar.gz",
        ] {
            let agent = http_agent(url);
            let config = agent.config();
            assert!(config.https_only(), "{url} must pin https_only");
            assert_eq!(config.redirect_auth_headers(), RedirectAuthHeaders::Never);
        }
        for url in [
            "http://127.0.0.1:64817/ext.tar.gz",
            "http://localhost/ext.tar.gz",
            "http://[::1]/ext.tar.gz",
            "http://127.0.0.5:1/ext.tar.gz",
        ] {
            let agent = http_agent(url);
            let config = agent.config();
            assert!(!config.https_only(), "{url} is the loopback carve-out");
        }
    }

    /// The carve-out predicate: loopback hosts only, and only over plain
    /// http — an https URL needs no carve-out, and no other host gets one.
    #[test]
    fn plain_http_is_allowed_only_for_loopback() {
        assert!(plain_http_allowed("http://127.0.0.1:64817/x"));
        assert!(plain_http_allowed("http://localhost/x"));
        assert!(plain_http_allowed("http://[::1]/x"));
        assert!(plain_http_allowed("http://127.192.0.9/x"));
        assert!(!plain_http_allowed("http://example.com/x"));
        assert!(!plain_http_allowed("http://127.0.0.1.evil.com/x"));
        assert!(!plain_http_allowed("http://128.0.0.1/x"));
        // Userinfo is a refusal, never a parse: the host split must not
        // read `localhost` out of `http://localhost:80@evil.com/` (the
        // LABS-470 security round's blocker).
        assert!(!plain_http_allowed("http://localhost:80@evil.com/x"));
        assert!(!plain_http_allowed("http://127.0.0.1@evil.com/x"));
        assert!(!plain_http_allowed("http://user@127.0.0.1/x"));
        assert!(!plain_http_allowed("http://127.0.0.999/x"));
        assert!(!plain_http_allowed("https://127.0.0.1/x"));
        assert!(!plain_http_allowed("ftp://127.0.0.1/x"));
    }
}
