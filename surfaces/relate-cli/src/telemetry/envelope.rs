//! Mixpanel envelope building and base dimensions (ADR-0079 §4).
//!
//! Base dimensions on every event: `token`, `time` (ms since epoch),
//! `distinct_id` (install ID), `$insert_id` (random UUID per event), `$os`,
//! `arch`, `version`, `uptime` (seconds since [`BaseContext`] creation),
//! `invoker`. The events themselves are defined in `event.rs`.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use relate_feedback::ids::uuid_v4;

use crate::agent_mode::{launched_by, Launched};

use super::Event;

/// CLI version, single-sourced from the crate's `Cargo.toml`.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Serialize one already-built envelope as a JSON line — the pipeline's
/// one serializer seam, always the swallowed form (ADR-0079 §5: no
/// telemetry failure may panic or print; an envelope that cannot
/// serialize is dropped, like an overflowed one). The batch form of the
/// same policy lives at its single call site (the drain loop's
/// `Client::post`), not behind a second pass-through wrapper.
pub(crate) fn spool_line(event: &Value) -> Option<String> {
    serde_json::to_string(event).ok()
}

/// `invoker` base dimension (ADR-0079 §4): who launched the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invoker {
    Human,
    Agent,
    Ci,
}

impl Invoker {
    fn as_str(self) -> &'static str {
        match self {
            Invoker::Human => "human",
            Invoker::Agent => "agent",
            Invoker::Ci => "ci",
        }
    }
}

/// The invoker classification (ADR-0079 §4) in this surface's vocabulary:
/// the shared [`launched_by`] ladder — agent marker, truthy `CI`, person —
/// mapped to `agent`/`ci`/`human`, now over the CLI's one marker table
/// (`agent_vars::PRESENCE_VARS`).
///
/// A non-TTY stdin alone does not demote a run to `agent`/`ci`: piped input
/// is overwhelmingly human shell pipelines, so only positive automation
/// markers move a run off `human`.
pub(crate) fn detect_invoker(env: &dyn Fn(&str) -> Option<String>) -> Invoker {
    match launched_by(env) {
        Launched::Agent => Invoker::Agent,
        Launched::Ci => Invoker::Ci,
        Launched::Human => Invoker::Human,
    }
}

/// Per-process base dimensions attached to every event (ADR-0079 §4).
pub struct BaseContext {
    token: String,
    distinct_id: String,
    invoker: Invoker,
    started: Instant,
}

impl BaseContext {
    /// Build from the process environment (invoker detection reads `CI` and
    /// the agent marker vars).
    pub(crate) fn detect(token: String, distinct_id: String) -> Self {
        Self::with_invoker(
            token,
            distinct_id,
            detect_invoker(&|k| std::env::var(k).ok()),
        )
    }

    /// Build with an explicit invoker, for tests.
    pub(crate) fn with_invoker(token: String, distinct_id: String, invoker: Invoker) -> Self {
        Self {
            token,
            distinct_id,
            invoker,
            started: Instant::now(),
        }
    }

    /// Base properties for one event: `token`, `time`, `distinct_id`,
    /// `$insert_id`, `$os`, `arch`, `version`, `uptime`, `invoker`.
    fn properties(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("token".into(), self.token.clone().into());
        m.insert("time".into(), epoch_millis().into());
        m.insert("distinct_id".into(), self.distinct_id.clone().into());
        m.insert("$insert_id".into(), uuid_v4().into());
        m.insert("$os".into(), std::env::consts::OS.into());
        m.insert("arch".into(), std::env::consts::ARCH.into());
        m.insert("version".into(), VERSION.into());
        m.insert("uptime".into(), self.started.elapsed().as_secs().into());
        m.insert("invoker".into(), self.invoker.as_str().into());
        m
    }
}

/// Build the Mixpanel `/track` envelope for one event:
/// `{"event": <name>, "properties": {<base dims> + <event props>}}`.
pub(crate) fn build_envelope(event: &Event, base: &BaseContext) -> Value {
    let mut properties = base.properties();
    properties.extend(event.properties());
    let mut envelope = Map::new();
    envelope.insert("event".into(), event.name().into());
    envelope.insert("properties".into(), properties.into());
    Value::Object(envelope)
}

fn epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::{env_of, OutputMode, QuerySource, StartupKind, StorageKind};

    fn base() -> BaseContext {
        BaseContext::with_invoker(
            "test-token".to_string(),
            "test-install-id".to_string(),
            Invoker::Human,
        )
    }

    fn all_events() -> Vec<Event> {
        vec![
            Event::Startup {
                kind: StartupKind::Oneshot,
            },
            Event::Startup {
                kind: StartupKind::Repl,
            },
            Event::Query {
                success: true,
                source: QuerySource::Stdin,
                storage: StorageKind::Memory,
                output_mode: OutputMode::Json,
            },
            Event::Query {
                success: false,
                source: QuerySource::Repl,
                storage: StorageKind::Persistent,
                output_mode: OutputMode::Table,
            },
            Event::Command {
                command: "skill",
                action: Some("install"),
                success: true,
            },
            Event::Command {
                command: "upgrade",
                action: None,
                success: false,
            },
            Event::Help,
        ]
    }

    #[test]
    fn uuid_v4_has_rfc4122_shape_version_and_variant() {
        let id = uuid_v4();
        assert_eq!(id.len(), 36);
        for (i, c) in id.chars().enumerate() {
            if matches!(i, 8 | 13 | 18 | 23) {
                assert_eq!(c, '-', "dash expected at {i}");
            } else {
                assert!(c.is_ascii_hexdigit(), "hex expected at {i}: {id}");
            }
        }
        assert_eq!(id.chars().nth(14), Some('4'), "version nibble");
        assert!(
            matches!(id.chars().nth(19), Some('8' | '9' | 'a' | 'b')),
            "variant nibble: {id}"
        );
        // Random: two draws differ (collision probability ~2^-122).
        assert_ne!(id, uuid_v4());
    }

    #[test]
    fn event_property_values_match_adr_taxonomy() {
        let env = build_envelope(
            &Event::Startup {
                kind: StartupKind::Stdin,
            },
            &base(),
        );
        assert_eq!(env["properties"]["kind"], "stdin");

        let env = build_envelope(
            &Event::Query {
                success: true,
                source: QuerySource::Oneshot,
                storage: StorageKind::Persistent,
                output_mode: OutputMode::Markdown,
            },
            &base(),
        );
        assert_eq!(env["properties"]["success"], true);
        assert_eq!(env["properties"]["source"], "oneshot");
        assert_eq!(env["properties"]["storage"], "persistent");
        assert_eq!(env["properties"]["output_mode"], "markdown");

        let env = build_envelope(
            &Event::Command {
                command: "skill",
                action: Some("remove"),
                success: false,
            },
            &base(),
        );
        assert_eq!(env["properties"]["command"], "skill");
        assert_eq!(env["properties"]["action"], "remove");
        assert_eq!(env["properties"]["success"], false);

        // Help carries no event-specific properties.
        let env = build_envelope(&Event::Help, &base());
        let props = env["properties"].as_object().unwrap();
        for key in ["kind", "source", "storage", "output_mode", "command"] {
            assert!(!props.contains_key(key), "Help must not carry {key}");
        }
    }

    #[test]
    fn envelope_has_all_base_dimensions() {
        let env = build_envelope(&Event::Help, &base());
        assert_eq!(env["event"], "RELATE-CLI_HELP");
        let props = env["properties"].as_object().unwrap();
        for key in [
            "token",
            "time",
            "distinct_id",
            "$insert_id",
            "$os",
            "arch",
            "version",
            "uptime",
            "invoker",
        ] {
            assert!(props.contains_key(key), "missing base dimension {key}");
        }
        assert_eq!(props["token"], "test-token");
        assert_eq!(props["distinct_id"], "test-install-id");
        assert_eq!(props["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(props["$os"], std::env::consts::OS);
        assert_eq!(props["arch"], std::env::consts::ARCH);
        assert_eq!(props["invoker"], "human");
        assert!(props["time"].as_u64().unwrap() > 0);
        assert_eq!(props["uptime"].as_u64(), Some(0));
    }

    #[test]
    fn insert_id_is_unique_per_event() {
        let base = base();
        let a = build_envelope(&Event::Help, &base);
        let b = build_envelope(&Event::Help, &base);
        assert_ne!(a["properties"]["$insert_id"], b["properties"]["$insert_id"]);
    }

    #[test]
    fn no_event_carries_privacy_forbidden_keys() {
        // ADR-0079 §4 hard privacy rule: never query text, database paths,
        // or parameter values. These key names must never appear.
        const FORBIDDEN: &[&str] = &["query", "cypher", "path", "db", "wal", "param"];
        for event in all_events() {
            let env = build_envelope(&event, &base());
            let props = env["properties"].as_object().unwrap();
            for key in props.keys() {
                assert!(
                    !FORBIDDEN.contains(&key.as_str()),
                    "event {} carries forbidden key {key}",
                    event.name()
                );
            }
        }
    }

    #[test]
    fn detect_invoker_agent_markers_win() {
        for var in crate::agent_vars::PRESENCE_VARS {
            let inv = detect_invoker(&env_of(&[(var, "1"), ("CI", "true")]));
            assert_eq!(inv, Invoker::Agent, "{var} should classify as agent");
        }
    }

    /// One process, one invoker: the telemetry event and the feedback report
    /// read the same classification, so the generic markers and a
    /// set-but-empty `CLAUDECODE` cannot tell the two pipelines different
    /// stories about the same run.
    #[test]
    fn invoker_classification_agrees_with_the_feedback_report() {
        for (pairs, agent) in [
            (&[("AI_AGENT", "claude-code")][..], true),
            (&[("COPILOT_AGENT", "1")][..], true),
            (&[("REPL_ID", "abc")][..], true),
            (&[("CLAUDECODE", "")][..], false),
        ] {
            let env = env_of(pairs);
            assert_eq!(
                detect_invoker(&env) == Invoker::Agent,
                agent,
                "telemetry event: {pairs:?}"
            );
            assert_eq!(
                crate::agent_feedback_cmd::invoker(&env) == relate_feedback::report::Invoker::Agent,
                agent,
                "feedback report: {pairs:?}"
            );
        }
    }

    #[test]
    fn detect_invoker_ci_truthy() {
        for v in ["true", "1", "yes", "continuous-integration"] {
            assert_eq!(detect_invoker(&env_of(&[("CI", v)])), Invoker::Ci);
        }
    }

    #[test]
    fn detect_invoker_ci_falsy_falls_through_to_human() {
        for v in ["", "0", "false", "FALSE"] {
            assert_eq!(detect_invoker(&env_of(&[("CI", v)])), Invoker::Human);
        }
    }

    #[test]
    fn detect_invoker_default_is_human() {
        assert_eq!(detect_invoker(&env_of(&[])), Invoker::Human);
    }
}
