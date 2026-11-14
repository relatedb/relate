//! The one Mixpanel `/track` POST the two senders share (ADR-0079 §2,
//! ADR-0588 §1): an endpoint allow-list, a short deadline, no retries,
//! redirects never followed, and `ip=0` so Mixpanel never derives a
//! location from the request address. Events go as a JSON array (the
//! `/track` API's batch form); the per-event `$insert_id` lets the sink
//! dedup at-least-once delivery (ADR-0545 §2).

use std::time::Duration;

use serde_json::Value;

/// Default Mixpanel `/track` endpoint (ADR-0079 §2). Used when no
/// endpoint override is set; an override is accepted only if it is
/// `https://` for any host or `http://` for a loopback host (ADR-0263).
pub(crate) const DEFAULT_ENDPOINT: &str = "https://api.mixpanel.com/track";

/// Per-request HTTP timeout (ADR-0079 §5).
const HTTP_TIMEOUT: Duration = Duration::from_secs(2);

/// How one `/track` POST ended: the classification each sender acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    /// A 2xx response: the sink confirmed the batch.
    Accepted,
    /// A 4xx response: the sink refused the batch, so resending it
    /// unchanged cannot help.
    Rejected,
    /// A 5xx response (or any other non-2xx): the sink failed to take it.
    ServerError,
    /// No answer: the transport exchange never completed.
    Unreachable,
    /// The deadline passed with no answer: the batch may or may not have
    /// landed.
    TimedOut,
}

/// A resolved `/track` sink: the endpoint plus the one HTTP agent shape
/// every sender uses.
pub struct Client {
    agent: ureq::Agent,
    endpoint: String,
}

impl Client {
    /// A client for `endpoint`, which the caller resolved through
    /// [`resolve_endpoint`] (or, for feedback, through its own stricter
    /// consent check on top of `endpoint_allowed`).
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            agent: ureq::Agent::new_with_config(
                ureq::Agent::config_builder()
                    .timeout_global(Some(HTTP_TIMEOUT))
                    // No retries and no redirects: a `/track` POST is
                    // fire-and-forget — an undelivered batch is re-sent only
                    // by the spool's next run (ADR-0545 §2).
                    // `max_redirects(0)` hands a 3xx back as the response
                    // instead of following it anywhere.
                    .max_redirects(0)
                    .build(),
            ),
            endpoint: with_ip_pinned_off(endpoint.into()),
        }
    }

    /// POST one batch of envelopes as a JSON array and classify the answer:
    /// a 2xx whose body records the event is [`Reply::Accepted`], a 4xx
    /// [`Reply::Rejected`], anything else a server-side failure, and a
    /// transport or deadline error says which way the attempt failed. The
    /// telemetry spool keeps a batch that was not accepted for a later
    /// attempt (its at-least-once rule).
    pub fn post(&self, batch: &[Value]) -> Reply {
        let Ok(body) = serde_json::to_vec(batch) else {
            return Reply::ServerError;
        };
        match self
            .agent
            .post(&self.endpoint)
            .header("Content-Type", "application/json")
            .send(body.as_slice())
        {
            Ok(response) => {
                let status = response.status().as_u16();
                if !(200..=299).contains(&status) {
                    return classify_status(status);
                }
                let mut body = response.into_body();
                classify_accepted_body(&body.read_to_string().unwrap_or_default())
            }
            // The default `http_status_as_error` turns 4xx and 5xx into
            // this error; a 3xx is handed back as the response instead.
            Err(ureq::Error::StatusCode(status)) => classify_status(status),
            Err(ureq::Error::Timeout(_)) => Reply::TimedOut,
            Err(_) => Reply::Unreachable,
        }
    }
}

/// Classify an HTTP status: 2xx accepted, 4xx refused, everything else —
/// 5xx and the unfollowed 3xx — a server-side failure.
fn classify_status(status: u16) -> Reply {
    match status {
        200..=299 => Reply::Accepted,
        400..=499 => Reply::Rejected,
        _ => Reply::ServerError,
    }
}

/// Whether a 2xx body says the event was recorded. Mixpanel's `/track`
/// answers `1` for a recorded event and `0` for a dropped one — an unknown
/// project token, an oversized batch — with a 200 either way, so the status
/// alone would report a dropped report as sent. A body in neither shape, or
/// one that cannot be read, keeps the status's answer.
fn classify_accepted_body(body: &str) -> Reply {
    match serde_json::from_str::<Value>(body.trim()) {
        Ok(Value::Number(n)) if n.as_i64() == Some(0) => Reply::Rejected,
        Ok(Value::Object(map)) if map.get("status").and_then(Value::as_i64) == Some(0) => {
            Reply::Rejected
        }
        _ => Reply::Accepted,
    }
}

/// Append `ip=0` (ADR-0588 §1): Mixpanel must not geolocate from the
/// request address. An override that already carries a query keeps it.
fn with_ip_pinned_off(endpoint: String) -> String {
    let sep = if endpoint.contains('?') { '&' } else { '?' };
    format!("{endpoint}{sep}ip=0")
}

/// Resolve the endpoint to POST to (ADR-0263): the override when it passes
/// `endpoint_allowed`, `DEFAULT_ENDPOINT` when unset, or `Err(url)` for
/// a rejected override that the caller must refuse (disable sending for the
/// run and warn).
pub fn resolve_endpoint(raw: Option<String>) -> Result<String, String> {
    match raw {
        None => Ok(DEFAULT_ENDPOINT.to_string()),
        Some(url) if endpoint_allowed(&url, HttpsHosts::Any) => Ok(url),
        Some(url) => Err(url),
    }
}

/// Which hosts may receive the payload over `https` — the one policy a
/// surface supplies to [`endpoint_allowed`] on top of the shared floor.
#[derive(Debug, Clone, Copy)]
pub(crate) enum HttpsHosts {
    /// Any host: the scheme alone is the guarantee (telemetry's rule,
    /// ADR-0263).
    Any,
    /// Only these hosts, compared case-insensitively (feedback's Mixpanel
    /// allow-list, ADR-0588 §3).
    Only(&'static [&'static str]),
}

impl HttpsHosts {
    /// Whether `host` may receive a payload over `https` under this policy.
    fn admits(self, host: &str) -> bool {
        match self {
            Self::Any => true,
            Self::Only(hosts) => hosts.iter().any(|known| host.eq_ignore_ascii_case(known)),
        }
    }
}

/// Whether `url` is a safe endpoint (ADR-0263, ADR-0588 §3): `https://` for
/// the `https` policy's hosts, `http://` only for a loopback host. Scheme
/// and host compare case-insensitively and an IPv6 literal's brackets are
/// trimmed, so `HTTPS://sink.example` and `http://[::1]:8080` are allowed
/// while a schemeless, unrecognized-scheme, hostless, or non-loopback
/// cleartext value is not.
///
/// This is the shared floor: one scheme parse, with the https host rule as
/// the caller's data — [`HttpsHosts::Any`] for telemetry,
/// [`HttpsHosts::Only`] for a surface's own allow-list.
pub(crate) fn endpoint_allowed(url: &str, https: HttpsHosts) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    let host = authority_host(rest);
    // A scheme with no host names no destination, whatever the policy
    // (`https:///track`, `https://:8443/track`).
    if host.is_empty() {
        return false;
    }
    if scheme.eq_ignore_ascii_case("https") {
        return https.admits(host);
    }
    if !scheme.eq_ignore_ascii_case("http") {
        return false;
    }
    is_loopback_name(host) || host == "127.0.0.1"
}

/// The host of a URL authority (the part after `://`): drops any
/// `userinfo@`, the trailing `:port`, any path/query/fragment, and the
/// brackets around an IPv6 literal. The empty string is the honest answer
/// for an authority with no host (`http:///x`).
///
/// The same-vocabulary twin of the CLI's `plaintext::authority_host`: the
/// CLI is this crate's consumer, so it is not callable from here — the
/// plaintext module already carries the same twin across relate-serve's
/// boundary for the same reason.
pub(crate) fn authority_host(authority: &str) -> &str {
    let host_port = authority.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = host_port
        .rsplit_once('@')
        .map_or(host_port, |(_, host)| host);
    if let Some(rest) = host_port.strip_prefix('[') {
        return rest.split(']').next().unwrap_or_default();
    }
    host_port.split(':').next().unwrap_or_default()
}

/// The **named** loopback hosts: `localhost` in any case, and the IPv6
/// literal `::1` (the twin of the CLI's `plaintext::is_loopback_name`).
fn is_loopback_name(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host == "::1"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_2xx_body_of_zero_is_not_accepted() {
        assert_eq!(classify_accepted_body("0"), Reply::Rejected);
        assert_eq!(classify_accepted_body("{\"status\":0}"), Reply::Rejected);
        assert_eq!(classify_accepted_body("1"), Reply::Accepted);
        assert_eq!(classify_accepted_body("{\"status\":1}"), Reply::Accepted);
        assert_eq!(classify_accepted_body(""), Reply::Accepted);
    }

    #[test]
    fn endpoint_allowed_accepts_https_for_any_host() {
        assert!(endpoint_allowed(
            "https://api.mixpanel.com/track",
            HttpsHosts::Any
        ));
        assert!(endpoint_allowed(
            "https://sink.example:8443/track?x=1",
            HttpsHosts::Any
        ));
        assert!(endpoint_allowed(
            "HTTPS://api.mixpanel.com/track",
            HttpsHosts::Any
        ));
    }

    #[test]
    fn endpoint_allowed_accepts_loopback_http_only() {
        assert!(endpoint_allowed("http://127.0.0.1:8080", HttpsHosts::Any));
        assert!(endpoint_allowed(
            "http://localhost:8080/track",
            HttpsHosts::Any
        ));
        assert!(endpoint_allowed("http://[::1]:8080", HttpsHosts::Any));
        assert!(endpoint_allowed("HTTP://LOCALHOST/track", HttpsHosts::Any));
        assert!(!endpoint_allowed(
            "http://api.mixpanel.com/track",
            HttpsHosts::Any
        ));
        assert!(!endpoint_allowed("http://10.0.0.1:8080", HttpsHosts::Any));
        assert!(!endpoint_allowed("HTTP://example.com", HttpsHosts::Any));
    }

    #[test]
    fn endpoint_allowed_rejects_schemeless_and_garbage() {
        assert!(!endpoint_allowed("api.mixpanel.com/track", HttpsHosts::Any));
        assert!(!endpoint_allowed("not a url", HttpsHosts::Any));
        assert!(!endpoint_allowed("", HttpsHosts::Any));
        assert!(!endpoint_allowed("ftp://127.0.0.1/track", HttpsHosts::Any));
        // An https URL with no host names no destination.
        assert!(!endpoint_allowed("https:///track", HttpsHosts::Any));
        assert!(!endpoint_allowed("https://:8443/track", HttpsHosts::Any));
    }

    #[test]
    fn resolve_endpoint_defaults_and_rejects() {
        assert_eq!(resolve_endpoint(None).unwrap(), DEFAULT_ENDPOINT);
        let allowed = "https://sink.example/track".to_string();
        assert_eq!(resolve_endpoint(Some(allowed.clone())).unwrap(), allowed);
        let rejected = "http://evil.example/track".to_string();
        assert_eq!(
            resolve_endpoint(Some(rejected.clone())).unwrap_err(),
            rejected
        );
    }

    #[test]
    fn http_statuses_classify_into_accepted_rejected_and_server_error() {
        for accepted in [200, 202, 299] {
            assert_eq!(classify_status(accepted), Reply::Accepted, "{accepted}");
        }
        for rejected in [400, 404, 429] {
            assert_eq!(classify_status(rejected), Reply::Rejected, "{rejected}");
        }
        for failed in [301, 302, 500, 503] {
            assert_eq!(classify_status(failed), Reply::ServerError, "{failed}");
        }
    }

    #[test]
    fn the_post_url_always_pins_ip_off() {
        assert_eq!(
            with_ip_pinned_off(DEFAULT_ENDPOINT.to_string()),
            "https://api.mixpanel.com/track?ip=0"
        );
        assert_eq!(
            with_ip_pinned_off("http://127.0.0.1:9/track?x=1".to_string()),
            "http://127.0.0.1:9/track?x=1&ip=0"
        );
    }
}
