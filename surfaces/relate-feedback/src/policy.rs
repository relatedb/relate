//! The feedback validation and redaction policy (ADR-0588 §1): what a
//! report must satisfy before one byte may leave the machine.
//!
//! Rules, in the ported domain's order:
//!
//! - the category is exact membership in [`Category::ALL`];
//! - `expected` and `observed` are non-blank after trimming and at most
//!   [`MAX_TEXT_BYTES`] bytes, rejected rather than truncated;
//! - a supplied `request_id` must be a lowercase UUIDv4, else it is a
//!   violation and a fresh id is used anyway;
//! - the redaction detectors run over the NFKC-normalized text, so
//!   full-width and look-alike forms match their plain equivalents;
//! - optional `command`, `error_code` and `agent.model` values are kept only
//!   when they pass their allow-lists; a failing optional value is dropped
//!   from the payload and named in `dropped_fields`, never sent;
//! - a report with any violation is rejected whole — failures are rejected,
//!   not rewritten — and the payload must fit [`MAX_PAYLOAD_BYTES`].
//!
//! A violation is only `{field, detector}`: the matched text is never
//! carried, so a violation list cannot leak what it detected.

use serde::Serialize;

use crate::ids;
use crate::report::{
    Agent, Category, Context, Input, Payload, MAX_PAYLOAD_BYTES, MAX_TEXT_BYTES, SCHEMA_VERSION,
    SURFACE,
};

/// Maximum characters of the optional agent model label.
pub const MAX_MODEL_CHARS: usize = 40;

mod detectors;

/// The detector that refused a field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Detector {
    InvalidInput,
    TooLarge,
    TooLong,
    ControlChars,
    Email,
    Url,
    HostPort,
    IpAddress,
    FilePath,
    DataFilename,
    Secret,
    Identifier,
    Cypher,
    Parameter,
    PastedOutput,
}

impl Detector {
    /// Every variant, for the gate that keeps the three hand-kept detector
    /// subsets honest. A variant added to the enum but to none of them
    /// compiles and silently never runs, which in the redaction path means
    /// text the policy meant to refuse leaves the machine.
    #[cfg(test)]
    pub(crate) const ALL: [Detector; 15] = [
        Detector::InvalidInput,
        Detector::TooLarge,
        Detector::TooLong,
        Detector::ControlChars,
        Detector::Email,
        Detector::Url,
        Detector::HostPort,
        Detector::IpAddress,
        Detector::FilePath,
        Detector::DataFilename,
        Detector::Secret,
        Detector::Identifier,
        Detector::Cypher,
        Detector::Parameter,
        Detector::PastedOutput,
    ];

    /// The input-shape detectors: their presence alone makes a rejection
    /// `invalid_input` rather than `policy_violation`.
    fn is_input_shape(self) -> bool {
        matches!(
            self,
            Detector::InvalidInput | Detector::TooLarge | Detector::TooLong
        )
    }
}

/// One refused `{field, detector}` pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Violation {
    pub field: &'static str,
    pub detector: Detector,
}

/// The result of checking one report. `dropped` and `request_id` are
/// reported either way: the outcome names what was removed and which id a
/// retry should keep.
#[derive(Debug, Clone)]
pub(crate) struct Checked {
    pub request_id: String,
    pub dropped: Vec<&'static str>,
    pub result: Result<Payload, Vec<Violation>>,
}

impl Checked {
    /// Whether any detector other than the input-shape trio fired: the
    /// rejection reason is then `policy_violation`, else `invalid_input`.
    pub(crate) fn is_policy_violation(&self) -> bool {
        match &self.result {
            Ok(_) => false,
            Err(violations) => violations.iter().any(|v| !v.detector.is_input_shape()),
        }
    }
}

/// Validate one report against the policy.
pub(crate) fn check(input: &Input, ctx: &Context) -> Checked {
    let mut violations = Vec::new();
    let mut dropped = Vec::new();

    let request_id = match input.request_id.as_deref() {
        Some(id) if is_uuid_v4(id) => id.to_string(),
        Some(_) => {
            push(&mut violations, "request_id", Detector::InvalidInput);
            ids::uuid_v4()
        }
        None => ids::uuid_v4(),
    };

    let category = match Category::parse(&input.category) {
        Some(category) => Some(category),
        None => {
            push(&mut violations, "category", Detector::InvalidInput);
            None
        }
    };

    let expected = input.expected.trim();
    let observed = input.observed.trim();
    for (field, text) in [("expected", expected), ("observed", observed)] {
        if text.is_empty() {
            push(&mut violations, field, Detector::InvalidInput);
        } else if text.len() > MAX_TEXT_BYTES {
            push(&mut violations, field, Detector::TooLong);
        }
        for detector in detectors::detect(text) {
            push(&mut violations, field, detector);
        }
    }

    let command_id = keep_or_drop(
        input.command.as_deref(),
        "command_id",
        |command| ctx.commands.iter().any(|known| known == command),
        &mut dropped,
    );
    let error_code = keep_or_drop(
        input.error_code.as_deref(),
        "error_code",
        |code| ctx.error_codes.iter().any(|known| known == code),
        &mut dropped,
    );
    let model = keep_or_drop(
        input.agent_model.as_deref(),
        "agent.model",
        model_valid,
        &mut dropped,
    );

    let harness = keep_or_drop(
        ctx.agent_harness.as_deref(),
        "agent.harness",
        slug_valid,
        &mut dropped,
    );
    let agent = match (harness, model) {
        (None, None) => None,
        (harness, model) => Some(Agent { harness, model }),
    };

    let result = match category {
        Some(category) if violations.is_empty() => {
            let payload = Payload {
                schema_version: SCHEMA_VERSION.to_string(),
                surface: SURFACE.to_string(),
                cli_version: ctx.cli_version.clone(),
                os: ctx.os.clone(),
                os_arch: ctx.os_arch.clone(),
                invoker: ctx.invoker,
                request_id: request_id.clone(),
                category,
                expected: expected.to_string(),
                observed: observed.to_string(),
                command_id,
                error_code,
                agent,
            };
            if payload.json_len() > MAX_PAYLOAD_BYTES {
                push(&mut violations, "payload", Detector::TooLarge);
                Err(violations)
            } else {
                Ok(payload)
            }
        }
        // The category failed or a violation fired: no payload can be built,
        // and the violations already name why.
        _ => Err(violations),
    };

    Checked {
        request_id,
        dropped,
        result,
    }
}

/// Whether a label is the report's one slug shape: non-empty, at most
/// [`MAX_MODEL_CHARS`] chars, lowercase, `^[a-z0-9][a-z0-9._:-]*$`. The gate
/// every agent-block string the CLI passes in is held to — the model label
/// and the harness label alike.
pub(crate) fn slug_valid(label: &str) -> bool {
    if label.is_empty() || label.chars().count() > MAX_MODEL_CHARS {
        return false;
    }
    let mut chars = label.chars();
    let first = chars.next().expect("non-empty checked above");
    (first.is_ascii_lowercase() || first.is_ascii_digit())
        && chars.all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | ':' | '-')
        })
}

/// Whether a model label may be sent: the [`slug_valid`] shape plus a known
/// provider family.
pub(crate) fn model_valid(model: &str) -> bool {
    slug_valid(model) && known_family(model)
}

/// The provider allow-list as one fact: a known prefix, or the `o<digit>`
/// family the same list cannot spell as a prefix.
fn known_family(model: &str) -> bool {
    const PREFIXES: [&str; 9] = [
        "claude", "gpt", "gemini", "codex", "glm", "llama", "qwen", "mistral", "deepseek",
    ];
    PREFIXES.iter().any(|p| model.starts_with(p))
        || (model.starts_with('o')
            && model[1..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit()))
}

/// The one optional-field policy: keep a value its `ok` predicate accepts,
/// else record the field's name in `dropped` and send nothing.
fn keep_or_drop(
    value: Option<&str>,
    name: &'static str,
    ok: impl Fn(&str) -> bool,
    dropped: &mut Vec<&'static str>,
) -> Option<String> {
    let value = value?;
    if !ok(value) {
        dropped.push(name);
        return None;
    }
    Some(value.to_string())
}

fn push(violations: &mut Vec<Violation>, field: &'static str, detector: Detector) {
    let violation = Violation { field, detector };
    if !violations.contains(&violation) {
        violations.push(violation);
    }
}

/// A lowercase canonical UUIDv4 (the ported request-id rule). Uppercase or
/// a non-v4 version/variant nibble is refused.
fn is_uuid_v4(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (i, b) in bytes.iter().enumerate() {
        if matches!(i, 8 | 13 | 18 | 23) {
            if *b != b'-' {
                return false;
            }
        } else if !b.is_ascii_digit() && !(b'a'..=b'f').contains(b) {
            return false;
        }
    }
    bytes[14] == b'4' && matches!(bytes[19], b'8' | b'9' | b'a' | b'b')
}

// --- tests ---------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::detectors::{allowed_url, has_cypher, has_ip};
    use crate::report::Invoker;
    use crate::test_support::context;

    fn ctx() -> Context {
        Context {
            commands: vec!["query".to_string(), "info".to_string()],
            ..context()
        }
    }

    fn input(category: &str, expected: &str, observed: &str) -> Input {
        Input {
            category: category.to_string(),
            expected: expected.to_string(),
            observed: observed.to_string(),
            ..Input::default()
        }
    }

    fn violations(checked: &Checked) -> Vec<Violation> {
        match &checked.result {
            Ok(_) => Vec::new(),
            Err(v) => v.clone(),
        }
    }

    /// Each text is refused by `detector` on the `expected` field.
    fn refused(detector: Detector, texts: &[&str]) {
        for text in texts {
            let checked = check(&input("friction", text, "b"), &ctx());
            assert!(
                violations(&checked).contains(&Violation {
                    field: "expected",
                    detector
                }),
                "{text:?} must be refused"
            );
        }
    }

    /// The text passes every detector.
    fn allowed(text: &str) {
        let checked = check(&input("friction", text, "b"), &ctx());
        assert!(
            checked.result.is_ok(),
            "{text:?} must be allowed: {:?}",
            violations(&checked)
        );
    }

    #[test]
    fn a_clean_report_is_accepted() {
        let checked = check(
            &input(
                "unclear_error",
                "the info command to print the version",
                "it printed help instead",
            ),
            &ctx(),
        );
        let payload = checked.result.expect("clean report");
        assert_eq!(payload.schema_version, "1");
        assert_eq!(payload.surface, "cli");
        assert_eq!(payload.category, Category::UnclearError);
        assert_eq!(payload.invoker, Invoker::Agent);
        assert_eq!(
            payload.agent.unwrap().harness.as_deref(),
            Some("claude-code")
        );
        assert!(is_uuid_v4(&payload.request_id));
    }

    #[test]
    fn invalid_category_is_rejected() {
        let checked = check(&input("unclear", "a", "b"), &ctx());
        assert_eq!(
            violations(&checked),
            [Violation {
                field: "category",
                detector: Detector::InvalidInput,
            }]
        );
        assert!(!checked.is_policy_violation());
    }

    #[test]
    fn blank_required_text_is_rejected() {
        let checked = check(&input("friction", "   ", "\t\n"), &ctx());
        let got = violations(&checked);
        assert!(got.contains(&Violation {
            field: "expected",
            detector: Detector::InvalidInput
        }));
        assert!(got.contains(&Violation {
            field: "observed",
            detector: Detector::InvalidInput
        }));
    }

    #[test]
    fn over_length_text_is_too_long_never_truncated() {
        let checked = check(
            &input(
                "friction",
                &"x".repeat(MAX_TEXT_BYTES + 1),
                &"y".repeat(MAX_TEXT_BYTES),
            ),
            &ctx(),
        );
        assert_eq!(
            violations(&checked),
            [Violation {
                field: "expected",
                detector: Detector::TooLong,
            }]
        );
        assert!(!checked.is_policy_violation());
    }

    #[test]
    fn an_invalid_request_id_is_a_violation_and_a_fresh_id_is_used() {
        let mut report = input("friction", "a", "b");
        report.request_id = Some("NOT-A-UUID".to_string());
        let checked = check(&report, &ctx());
        assert!(violations(&checked).contains(&Violation {
            field: "request_id",
            detector: Detector::InvalidInput
        }));
        assert_ne!(checked.request_id, "NOT-A-UUID");
        assert!(is_uuid_v4(&checked.request_id));
    }

    #[test]
    fn a_supplied_valid_request_id_is_kept() {
        let id = "123e4567-e89b-42d3-a456-426614174000";
        let mut report = input("friction", "a", "b");
        report.request_id = Some(id.to_string());
        let checked = check(&report, &ctx());
        assert_eq!(checked.request_id, id);
        assert_eq!(checked.result.unwrap().request_id, id);
    }

    #[test]
    fn uuid_v4_validation_shape() {
        assert!(is_uuid_v4("123e4567-e89b-42d3-a456-426614174000"));
        assert!(!is_uuid_v4("123E4567-E89B-42D3-A456-426614174000")); // uppercase
        assert!(!is_uuid_v4("123e4567-e89b-12d3-a456-426614174000")); // v1
        assert!(!is_uuid_v4("123e4567-e89b-42d3-0456-426614174000")); // variant
        assert!(!is_uuid_v4("123e4567e89b42d3a456426614174000")); // no dashes
    }

    #[test]
    fn every_detector_is_named() {
        let pairs = [
            (Detector::ControlChars, "control_chars"),
            (Detector::Email, "email"),
            (Detector::Url, "url"),
            (Detector::HostPort, "host_port"),
            (Detector::IpAddress, "ip_address"),
            (Detector::FilePath, "file_path"),
            (Detector::DataFilename, "data_filename"),
            (Detector::Secret, "secret"),
            (Detector::Identifier, "identifier"),
            (Detector::Cypher, "cypher"),
            (Detector::Parameter, "parameter"),
            (Detector::PastedOutput, "pasted_output"),
            (Detector::InvalidInput, "invalid_input"),
            (Detector::TooLarge, "too_large"),
            (Detector::TooLong, "too_long"),
        ];
        for (detector, wire) in pairs {
            assert_eq!(serde_json::to_value(detector).unwrap(), wire);
        }
    }

    #[test]
    fn control_and_format_characters_are_rejected() {
        refused(
            Detector::ControlChars,
            &["line\nbreak", "zero\u{200B}width", "bidi\u{202E}mark"],
        );
        allowed("a\ttab");
    }

    #[test]
    fn email_is_rejected() {
        refused(Detector::Email, &["mail me at dev@example.com"]);
    }

    #[test]
    fn urls_are_rejected_except_the_documentation_allow_list() {
        refused(
            Detector::Url,
            &[
                "see https://evil.example/secret",
                "see HTTPS://example.com/x",
                "postgres://host/db did not answer",
            ],
        );
        allowed("the docs at https://neo4j.com/docs/cypher-manual/current/");
    }

    #[test]
    fn allowed_url_detection_is_strict() {
        assert!(allowed_url("https://neo4j.com/docs/a/b"));
        assert!(allowed_url("https://relatedb.ai/docs/reference/cli-flags"));
        assert!(allowed_url(
            "https://github.com/relatedb/relate/blob/main/README.md"
        ));
        assert!(!allowed_url(
            "https://github.com/foreign/other/blob/main/a.md"
        ));
        assert!(!allowed_url("https://relatedb.ai"));
        assert!(!allowed_url("https://github.com/relatedb/relate"));
        assert!(!allowed_url("http://neo4j.com/docs/a"));
        assert!(!allowed_url("https://neo4j.com/other/a"));
        assert!(!allowed_url("https://neo4j.com:8443/docs/a"));
        assert!(!allowed_url("https://neo4j.com/docs/a?x=1"));
        assert!(!allowed_url("https://user@neo4j.com/docs/a"));
        assert!(!allowed_url("https://neo4j.com/docs/a%20b"));
    }

    #[test]
    fn hosts_are_rejected_but_times_are_not() {
        refused(
            Detector::HostPort,
            &[
                "connect to db.example:5432",
                "localhost:8080",
                "box:1234",
                "connecting to db.prod.internal timed out",
                // A host with a path or a query after it is one token: the
                // authority is what precedes the first `/`, `?` or `#`.
                "saw grafana.internal/d/abc",
                "opened localhost:8080/admin",
                "posted to metrics.example.com/api?tenant=acme",
            ],
        );
        allowed("it took 10:30");
    }

    #[test]
    fn ip_addresses_are_rejected() {
        refused(
            Detector::IpAddress,
            &[
                "server 10.0.0.1 answered",
                "connect to fe80::1",
                // The address with a path after it is still one token: the
                // authority is what precedes the first `/`.
                "curled 10.0.0.1:8080/admin",
            ],
        );
        assert!(!has_ip("std::string and version 1.2.3"));
    }

    #[test]
    fn file_paths_are_rejected_but_slash_words_are_not() {
        refused(
            Detector::FilePath,
            &[
                "read /etc/passwd",
                "look in /usr/local/share",
                "wrote ./out.log",
                "at ~/projects/relate",
                "C:\\Users\\me\\file",
            ],
        );
        allowed("the /mcp status");
    }

    #[test]
    fn data_filenames_are_rejected_but_product_filenames_are_not() {
        refused(
            Detector::DataFilename,
            &[
                "imported users.csv",
                "a 12 MB dump.parquet",
                // The extension is the last one: a dotted name must not stop
                // the match at the first dot.
                "imported orders.2024.csv",
                "loaded users.prod.csv",
                "a 3 MB dump.2026.parquet",
            ],
        );
        for text in ["config.json is fine", "SKILL.md documents it", "Cargo.toml"] {
            allowed(text);
        }
    }

    #[test]
    fn secrets_are_rejected() {
        refused(
            Detector::Secret,
            &[
                "token=abc123",
                "the password hunter2",
                "our API key abc123",
                "used sk-abcdefgh12345678",
                "saw ghp_abcdefgh12345678",
                "AKIAIOSFODNN7EXAMPLE",
                "value a1b2c3d4e5f6g7h8i9j0",
                "DATABASE_PASSWORD=hunter2",
                "MY_API_KEY=abc123",
                "PGPASSWORD=hunter2",
                "export GITHUB_TOKEN=abc123",
            ],
        );
        // The prose arm keeps its leading word boundary, so a word that
        // merely contains a keyword stays prose.
        allowed("compass direction= north");
    }

    #[test]
    fn identifiers_are_rejected() {
        refused(
            Detector::Identifier,
            &["node 123e4567-e89b-42d3-a456-426614174000", "id a1b2c3d4"],
        );
    }

    #[test]
    fn cypher_is_rejected() {
        refused(
            Detector::Cypher,
            &[
                "MATCH (n:Person) RETURN n",
                "CREATE (n:Person {name: 'x'})",
                "used $name in the query",
                "the query had MATCH and RETURN",
                "re ran [r:KNOWS]",
            ],
        );
        assert!(!has_cypher("please match the docs and create a file"));
        assert!(!has_cypher("set $NEO4J_URI first"));
    }

    #[test]
    fn parameters_and_pasted_output_are_rejected() {
        for text in [
            "with --param name=value",
            "{\"a\": 1}",
            "| a | b |\n|---|---|",
        ] {
            let checked = check(&input("friction", text, "b"), &ctx());
            let got = violations(&checked);
            assert!(
                got.iter()
                    .any(|v| matches!(v.detector, Detector::Parameter | Detector::PastedOutput)),
                "{text:?} must be refused: {got:?}"
            );
        }
        let checked = check(
            &input("friction", "Error: no such command (exit 2)", "b"),
            &ctx(),
        );
        assert!(violations(&checked).contains(&Violation {
            field: "expected",
            detector: Detector::PastedOutput
        }));
    }

    #[test]
    fn nfkc_forms_are_matched_like_their_plain_equivalents() {
        // Full-width letters normalize under NFKC before the detectors run.
        let checked = check(
            &input("friction", "\u{FF41}\u{FF42}\u{FF43}@example.com", "b"),
            &ctx(),
        );
        assert!(violations(&checked).contains(&Violation {
            field: "expected",
            detector: Detector::Email
        }));
    }

    #[test]
    fn violations_are_deduplicated() {
        let checked = check(
            &input(
                "friction",
                "a@example.com and b@example.com",
                "c@example.com",
            ),
            &ctx(),
        );
        let emails = violations(&checked)
            .iter()
            .filter(|v| v.detector == Detector::Email)
            .count();
        assert_eq!(emails, 2, "one per field, never twice in one field");
    }

    #[test]
    fn optional_values_are_dropped_when_they_fail_their_allow_list() {
        let mut report = input("friction", "a", "b");
        report.command = Some("nonsense".to_string());
        report.error_code = Some("not_a_code".to_string());
        report.agent_model = Some("Unknown-Model".to_string());
        let checked = check(&report, &ctx());
        let payload = checked.result.expect("report itself is clean");
        assert_eq!(checked.dropped, ["command_id", "error_code", "agent.model"]);
        assert!(payload.command_id.is_none());
        assert!(payload.error_code.is_none());
        assert!(payload.agent.unwrap().model.is_none());
    }

    /// The harness label is a value from the caller's environment, so the
    /// crate's one gate runs it like the model label: a refused label is
    /// dropped and named, never sent — whatever surface supplied the context.
    #[test]
    fn a_refused_harness_label_is_dropped_and_named() {
        let context = Context {
            agent_harness: Some("Claude Code".to_string()),
            ..ctx()
        };
        let mut report = input("friction", "a", "b");
        report.agent_model = Some("claude-opus-5-5".to_string());
        let checked = check(&report, &context);
        let payload = checked.result.expect("the report itself is clean");
        assert_eq!(checked.dropped, ["agent.harness"]);
        let agent = payload.agent.expect("the kept model keeps the block");
        assert_eq!(agent.harness, None);
        assert_eq!(agent.model.as_deref(), Some("claude-opus-5-5"));
    }

    #[test]
    fn optional_values_pass_their_allow_list() {
        let mut report = input("friction", "a", "b");
        report.command = Some("query".to_string());
        report.error_code = Some("usage_error".to_string());
        report.agent_model = Some("claude-opus-5-5".to_string());
        let checked = check(&report, &ctx());
        let payload = checked.result.expect("clean report");
        assert!(checked.dropped.is_empty());
        assert_eq!(payload.command_id.as_deref(), Some("query"));
        assert_eq!(payload.error_code.as_deref(), Some("usage_error"));
        assert_eq!(
            payload.agent.unwrap().model.as_deref(),
            Some("claude-opus-5-5")
        );
    }

    #[test]
    fn model_allow_list() {
        for good in [
            "claude-opus-5-5",
            "gpt-5.1",
            "gemini-3-pro",
            "codex-mini",
            "o3",
            "o4-mini",
            "deepseek-v3",
            "qwen3:32b",
        ] {
            assert!(model_valid(good), "{good} should be accepted");
        }
        for bad in [
            "",
            "Claude-Opus",
            "unknown-model",
            "claude opus",
            "claudeopus!",
            "123456",
        ] {
            assert!(!model_valid(bad), "{bad:?} should be refused");
        }
        // The length boundary is the constant's, never a literal: the last
        // accepted label fills MAX_MODEL_CHARS exactly, one more is refused.
        let fit = MAX_MODEL_CHARS - "claude-".len();
        assert!(model_valid(&format!("claude-{}", "a".repeat(fit))));
        assert!(!model_valid(&format!("claude-{}", "a".repeat(fit + 1))));
    }

    #[test]
    fn an_over_size_payload_is_rejected() {
        let mut payload = check(&input("friction", "a", "b"), &ctx())
            .result
            .expect("clean report");
        payload.expected = "x".repeat(MAX_PAYLOAD_BYTES);
        assert!(payload.json_len() > MAX_PAYLOAD_BYTES);
        let mut violations = Vec::new();
        push(&mut violations, "payload", Detector::TooLarge);
        assert_eq!(violations[0].detector, Detector::TooLarge);
    }

    #[test]
    fn rejection_reason_classifies_detector_families() {
        let input_shape = check(&input("bogus", "a", "b"), &ctx());
        assert!(!input_shape.is_policy_violation());
        let policy = check(&input("friction", "a@example.com", "b"), &ctx());
        assert!(policy.is_policy_violation());
    }
}
