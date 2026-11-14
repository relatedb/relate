//! The feedback surface's `/track` sender (ADR-0588 §1, §3): one validated
//! report becomes one Mixpanel event under the feedback project's token,
//! with this surface's own endpoint policy — the Mixpanel hosts over
//! `https`, or loopback over `http`.
//!
//! The event is the ported `RELATE-CLI_AGENT_FEEDBACK` shape: the report id
//! is both `distinct_id` (one report is one deletable identity) and
//! `$insert_id` (deduplication); optional properties are omitted; no time
//! and no IP are sent.

use serde::Serialize;
use serde_json::{json, Value};

use crate::client::{self, Client, Reply, DEFAULT_ENDPOINT};
use crate::outcome::Reason;
use crate::report::Payload;
use crate::submit::Sender;

/// The one Mixpanel event a report becomes.
pub(crate) const EVENT_NAME: &str = "RELATE-CLI_AGENT_FEEDBACK";

/// The feedback project's compiled-in ingestion token (ADR-0588 §3): public
/// client-side by design, overridable with `RELATE_FEEDBACK_TOKEN`. Empty
/// until the project is provisioned — an empty token means no default
/// destination, so only a loopback endpoint can send.
pub(crate) const TOKEN: &str = "";

/// The hosts a report may be sent to over `https` (ADR-0588 §3).
const MIXPANEL_HOSTS: [&str; 3] = [
    "api.mixpanel.com",
    "api-eu.mixpanel.com",
    "api-in.mixpanel.com",
];

/// Whether `url` may receive a report (ADR-0588 §3): the shared floor with
/// this surface's https policy — `https` only for a Mixpanel host, `http`
/// only for loopback.
pub(crate) fn endpoint_allowed(url: &str) -> bool {
    client::endpoint_allowed(url, client::HttpsHosts::Only(&MIXPANEL_HOSTS))
}

/// The token rule, in one place: `RELATE_FEEDBACK_TOKEN` when set, else the
/// compiled-in [`TOKEN`] (empty until the project is provisioned).
fn token(env: &dyn Fn(&str) -> Option<String>) -> String {
    env("RELATE_FEEDBACK_TOKEN").unwrap_or_else(|| TOKEN.to_string())
}

/// Resolve the endpoint to send to: `RELATE_FEEDBACK_ENDPOINT` when it
/// passes `endpoint_allowed`, else `DEFAULT_ENDPOINT` when a token is
/// configured, else none (no destination). A set-but-refused override is
/// `Err(url)`, so the caller can report it instead of recording no
/// destination.
pub fn resolve_feedback_endpoint(
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Option<String>, String> {
    match env("RELATE_FEEDBACK_ENDPOINT") {
        Some(raw) if endpoint_allowed(&raw) => Ok(Some(raw)),
        Some(raw) => Err(raw),
        None => Ok((!token(env).is_empty()).then(|| DEFAULT_ENDPOINT.to_string())),
    }
}

/// The feedback [`Sender`]: one event per report over the shared client,
/// to the endpoint the consent record names.
pub struct FeedbackSender {
    token: String,
}

impl FeedbackSender {
    /// Build from the injected environment lookup. The token is read here,
    /// not recorded: a credential is not a destination.
    pub fn from_env(env: &dyn Fn(&str) -> Option<String>) -> Self {
        Self { token: token(env) }
    }
}

impl Sender for FeedbackSender {
    fn send(&self, endpoint: &str, payload: &Payload) -> Reason {
        if !endpoint_allowed(endpoint) {
            return Reason::NotConfigured;
        }
        match Client::new(endpoint).post(&[envelope(payload, &self.token)]) {
            Reply::Accepted => Reason::Sent,
            Reply::Rejected => Reason::ServerRejected,
            Reply::ServerError => Reason::ServerError,
            Reply::Unreachable => Reason::Unreachable,
            Reply::TimedOut => Reason::Timeout,
        }
    }
}

/// The event's properties: the payload's fields projected to the flat
/// Mixpanel vocabulary, with the agent block flattened. Serde emits the
/// optional fields only when set, like [`Payload`]'s own attributes.
#[derive(Serialize)]
struct EventProperties<'a> {
    token: &'a str,
    distinct_id: &'a str,
    #[serde(rename = "$insert_id")]
    insert_id: &'a str,
    schema_version: &'a str,
    surface: &'a str,
    cli_version: &'a str,
    category: &'static str,
    expected: &'a str,
    observed: &'a str,
    #[serde(rename = "$os", skip_serializing_if = "Option::is_none")]
    os: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    os_arch: Option<&'a str>,
    invoker: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    command_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_harness: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_model: Option<&'a str>,
}

/// The one Mixpanel `/track` envelope for a report.
fn envelope(payload: &Payload, token: &str) -> Value {
    json!({
        "event": EVENT_NAME,
        "properties": EventProperties {
            token,
            distinct_id: &payload.request_id,
            insert_id: &payload.request_id,
            schema_version: &payload.schema_version,
            surface: &payload.surface,
            cli_version: &payload.cli_version,
            category: payload.category.as_str(),
            expected: &payload.expected,
            observed: &payload.observed,
            os: payload.os.as_deref(),
            os_arch: payload.os_arch.as_deref(),
            invoker: payload.invoker.as_str(),
            command_id: payload.command_id.as_deref(),
            error_code: payload.error_code.as_deref(),
            agent_harness: payload.agent.as_ref().and_then(|a| a.harness.as_deref()),
            agent_model: payload.agent.as_ref().and_then(|a| a.model.as_deref()),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{Category, Invoker};
    use crate::test_support::{env_of, payload};

    /// The envelope's report: one labelled agent run that named a command.
    fn agent_payload() -> Payload {
        Payload {
            invoker: Invoker::Agent,
            category: Category::UnclearError,
            expected: "the info command to print the version".to_string(),
            observed: "it printed help instead".to_string(),
            command_id: Some("query".to_string()),
            ..payload()
        }
    }

    #[test]
    fn the_endpoint_policy_admits_mixpanel_hosts_and_loopback_only() {
        for allowed in [
            "https://api.mixpanel.com/track",
            "https://api-eu.mixpanel.com/track",
            "https://API-IN.mixpanel.com/track",
            "http://127.0.0.1:9999/track",
            "http://localhost:9999/track",
        ] {
            assert!(endpoint_allowed(allowed), "{allowed} should be allowed");
        }
        for rejected in [
            "https://sink.example/track",
            "https://api.mixpanel.com.evil.example/track",
            "http://api.mixpanel.com/track",
            "ftp://api.mixpanel.com/track",
            "",
        ] {
            assert!(!endpoint_allowed(rejected), "{rejected} should be refused");
        }
    }

    #[test]
    fn an_empty_token_has_no_default_destination() {
        assert_eq!(resolve_feedback_endpoint(&env_of(&[])), Ok(None));
        assert_eq!(
            resolve_feedback_endpoint(&env_of(&[("RELATE_FEEDBACK_TOKEN", "t")])),
            Ok(Some(DEFAULT_ENDPOINT.to_string()))
        );
        assert_eq!(
            resolve_feedback_endpoint(&env_of(&[(
                "RELATE_FEEDBACK_ENDPOINT",
                "http://127.0.0.1:9/track"
            )])),
            Ok(Some("http://127.0.0.1:9/track".to_string()))
        );
        assert_eq!(
            resolve_feedback_endpoint(&env_of(&[(
                "RELATE_FEEDBACK_ENDPOINT",
                "https://evil.example/track"
            )])),
            Err("https://evil.example/track".to_string()),
            "a refused override is handed back, never dropped"
        );
    }

    #[test]
    fn a_sender_without_a_destination_reports_not_configured() {
        let sender = FeedbackSender::from_env(&env_of(&[]));
        assert_eq!(sender.send("", &payload()), Reason::NotConfigured);
        assert_eq!(
            sender.send("https://sink.example/track", &payload()),
            Reason::NotConfigured,
            "an endpoint outside the policy is not a destination"
        );
    }

    /// A 4xx answer is a rejection, and an endpoint that answers nothing
    /// (the mock stopped) is unreachable — the two classifications the
    /// outcome document names.
    #[test]
    fn a_4xx_reply_is_server_rejected_and_a_stopped_mock_is_unreachable() {
        let (sink, port) =
            relate_test_support::loopback::spawn_capture_server("400 Bad Request", "");
        let sender = FeedbackSender::from_env(&env_of(&[("RELATE_FEEDBACK_TOKEN", "t")]));
        let endpoint = format!("http://127.0.0.1:{port}/track");
        assert_eq!(sender.send(&endpoint, &payload()), Reason::ServerRejected);
        assert_eq!(sink.bodies().len(), 1, "the batch reached the mock");
        drop(sink);
        assert_eq!(sender.send(&endpoint, &payload()), Reason::Unreachable);
    }

    #[test]
    fn a_2xx_reply_is_sent() {
        let (sink, port) = relate_test_support::loopback::spawn_capture_server("200 OK", "1");
        let sender = FeedbackSender::from_env(&env_of(&[("RELATE_FEEDBACK_TOKEN", "t")]));
        let endpoint = format!("http://127.0.0.1:{port}/track");
        assert_eq!(sender.send(&endpoint, &payload()), Reason::Sent);
        assert_eq!(sink.bodies().len(), 1);
    }

    #[test]
    fn the_envelope_is_the_one_feedback_event() {
        let payload = agent_payload();
        let event = envelope(&payload, "test-token");
        assert_eq!(event["event"], EVENT_NAME);
        let properties = &event["properties"];
        assert_eq!(properties["token"], "test-token");
        assert_eq!(properties["distinct_id"], payload.request_id);
        assert_eq!(properties["$insert_id"], payload.request_id);
        assert_eq!(properties["category"], "unclear_error");
        assert_eq!(properties["invoker"], "agent");
        assert_eq!(properties["$os"], "darwin");
        assert_eq!(properties["os_arch"], "arm64");
        assert_eq!(properties["command_id"], "query");
        for absent in ["error_code", "agent_harness", "agent_model", "time"] {
            assert!(properties.get(absent).is_none(), "{absent} must be omitted");
        }
    }
}
