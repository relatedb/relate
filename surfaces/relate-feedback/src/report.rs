//! The agent-feedback report model (ADR-0588 §1): what a caller supplies,
//! what the sender contributes, and the payload — the one value that may
//! leave the machine.
//!
//! The shape and limits are the ported domain's: a fixed schema version and
//! surface, the free text capped at 255 bytes, an optional command path,
//! error code and agent model that only survive their allow-lists, and a
//! payload capped at 16 KiB. Every limit and allow-list is enforced in
//! [`crate::policy`]; this module owns the model and the constants.

use serde::{Serialize, Serializer};

/// The payload's schema version. A constant: there is one report shape.
pub const SCHEMA_VERSION: &str = "1";

/// The reporting surface of the CLI build.
pub const SURFACE: &str = "cli";

/// Maximum bytes of `expected` and `observed` (the port's `len` check
/// counts bytes, not characters). Over-limit text is rejected, never
/// truncated.
pub const MAX_TEXT_BYTES: usize = 255;

/// Maximum serialized payload size. Anything larger is rejected rather than
/// sent.
pub const MAX_PAYLOAD_BYTES: usize = 16 * 1024;

/// The four accepted report categories; [`Category::Friction`] is the
/// surface default when the caller names none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Friction,
    UnclearError,
    UnexpectedBehavior,
    MissingCapability,
}

impl Category {
    /// Every accepted category, the default first.
    pub const ALL: [Category; 4] = [
        Category::Friction,
        Category::UnclearError,
        Category::UnexpectedBehavior,
        Category::MissingCapability,
    ];

    /// The exact wire spelling accepted from the caller.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }

    /// The exact wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Friction => "friction",
            Category::UnclearError => "unclear_error",
            Category::UnexpectedBehavior => "unexpected_behavior",
            Category::MissingCapability => "missing_capability",
        }
    }
}

/// The serialized spelling is [`Category::as_str`]'s: the one author, so the
/// wire document and the Mixpanel envelope cannot drift apart.
impl Serialize for Category {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// Who ran the CLI: agent, script or human. The CLI's agent detection
/// classifies the invocation; this crate only carries the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invoker {
    Agent,
    Script,
    Human,
}

impl Invoker {
    pub fn as_str(self) -> &'static str {
        match self {
            Invoker::Agent => "agent",
            Invoker::Script => "script",
            Invoker::Human => "human",
        }
    }
}

/// The serialized spelling is [`Invoker::as_str`]'s, as for [`Category`].
impl Serialize for Invoker {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// What the caller supplies for one report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Input {
    /// Raw caller value; validated against [`Category::ALL`].
    pub category: String,
    /// What the caller expected, trimmed and rejected when over
    /// [`MAX_TEXT_BYTES`].
    pub expected: String,
    /// What actually happened, same rules as `expected`.
    pub observed: String,
    /// Optional CLI command path, kept only when visible and not feedback.
    pub command: Option<String>,
    /// Optional CLI error-code name, kept only against the allow-list.
    pub error_code: Option<String>,
    /// Optional public model label, kept only when its shape is valid.
    pub agent_model: Option<String>,
    /// Optional caller-supplied UUIDv4; an invalid value is a violation and
    /// a fresh id is generated.
    pub request_id: Option<String>,
}

/// The sender-owned part of a report: never taken from report text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    /// The running CLI's version.
    pub cli_version: String,
    /// The OS name (`linux`, `darwin`, `windows`), when known.
    pub os: Option<String>,
    /// The CPU architecture (`arm64`, `amd64`), when known.
    pub os_arch: Option<String>,
    /// The classified invocation.
    pub invoker: Invoker,
    /// The detected agent harness label (`claude-code`, `codex`, …), when
    /// the caller is an agent with a known label.
    pub agent_harness: Option<String>,
    /// Visible CLI command paths with the root name stripped, used to
    /// allow-list `command`.
    pub commands: Vec<String>,
    /// The CLI's exit-table `code` names, used to allow-list `error_code` —
    /// the vocabulary is the CLI's, passed in like `commands`.
    pub error_codes: Vec<String>,
}

/// The agent block: present only when the caller is a labelled agent or
/// carried a valid model label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Agent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// The validated report: the only value that may leave the machine, and the
/// value a local receipt records for a send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Payload {
    pub schema_version: String,
    pub surface: String,
    pub cli_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_arch: Option<String>,
    pub invoker: Invoker,
    pub request_id: String,
    pub category: Category,
    pub expected: String,
    pub observed: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<Agent>,
}

impl Payload {
    /// The serialized byte length the [`MAX_PAYLOAD_BYTES`] check compares.
    /// A fixed struct of strings and enums always serializes, so a failure is
    /// an invariant break, not an over-size payload.
    pub fn json_len(&self) -> usize {
        serde_json::to_vec(self)
            .expect("a payload of strings and enums serializes")
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_parse_is_exact_membership() {
        for c in Category::ALL {
            assert_eq!(Category::parse(c.as_str()), Some(c));
        }
        for bad in [
            "",
            "Unclear_Error",
            "unclear error",
            "other",
            "clarity",
            "other_friction",
        ] {
            assert_eq!(Category::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn category_and_invoker_wire_spellings() {
        let cats: Vec<_> = Category::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(
            cats,
            [
                "friction",
                "unclear_error",
                "unexpected_behavior",
                "missing_capability"
            ]
        );
        assert_eq!(Invoker::Agent.as_str(), "agent");
        assert_eq!(Invoker::Script.as_str(), "script");
        assert_eq!(Invoker::Human.as_str(), "human");
        // The serialized spelling is the same one spelling.
        for c in Category::ALL {
            assert_eq!(serde_json::to_value(c).unwrap(), c.as_str());
        }
        for invoker in [Invoker::Agent, Invoker::Script, Invoker::Human] {
            assert_eq!(serde_json::to_value(invoker).unwrap(), invoker.as_str());
        }
    }

    #[test]
    fn payload_serializes_optional_fields_only_when_set() {
        let payload = Payload {
            schema_version: SCHEMA_VERSION.to_string(),
            surface: SURFACE.to_string(),
            cli_version: "1.0.2".to_string(),
            os: None,
            os_arch: None,
            invoker: Invoker::Agent,
            request_id: "123e4567-e89b-42d3-a456-426614174000".to_string(),
            category: Category::UnexpectedBehavior,
            expected: "a".to_string(),
            observed: "b".to_string(),
            command_id: None,
            error_code: None,
            agent: None,
        };
        let value = serde_json::to_value(&payload).unwrap();
        assert_eq!(value["category"], "unexpected_behavior");
        assert_eq!(value["invoker"], "agent");
        for absent in ["os", "os_arch", "command_id", "error_code", "agent"] {
            assert!(value.get(absent).is_none(), "{absent} must be omitted");
        }
    }

    #[test]
    fn payload_len_is_the_serialized_length() {
        let payload = crate::test_support::payload();
        assert_eq!(
            payload.json_len(),
            serde_json::to_vec(&payload).unwrap().len()
        );
    }
}
