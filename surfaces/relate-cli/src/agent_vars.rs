//! The agent-harness detection vocabulary (ADR-0196; the generic markers
//! and the ladder per ADR-0407). A normal module of the binary; the e2e
//! harness mounts this same file with `#[path]`
//! (`tests/support/ambient_scrub.rs`), so there is exactly one copy — and
//! exactly why it carries only the tables and what reads them without crate
//! paths: every e2e target compiles the whole mounted file, so the
//! resolutions over it (the `AgentMode` ladder and the invoker
//! classification) live in the unmounted `agent_mode` module beside it, not
//! here.

/// Environment variables whose mere presence (non-empty) marks a known agent
/// harness (ADR-0196; the generic markers per the cross-harness
/// `AI_AGENT` proposal, ADR-0407). The list `neo4j-cli` uses, which
/// follows `unjs/std-env`.
pub(crate) const PRESENCE_VARS: &[&str] = &[
    // The cross-harness generic markers: `AI_AGENT` names the harness
    // (a slug), `AGENT` is its legacy spelling (ADR-0407).
    "AI_AGENT",
    "AGENT",
    "CLAUDECODE",
    "CLAUDE_CODE",
    // GitHub Copilot's trio (ADR-0407).
    "COPILOT_AGENT",
    "COPILOT_CLI",
    "COPILOT_AGENT_SESSION_ID",
    "REPL_ID",
    "GEMINI_CLI",
    "CODEX_SANDBOX",
    "CODEX_THREAD_ID",
    "OPENCODE",
    "AUGMENT_AGENT",
    "GOOSE_PROVIDER",
    "CURSOR_AGENT",
];

/// Case-insensitive substring rules: `(variable, lowercase substring)`.
pub(crate) const SUBSTRING_RULES: &[(&str, &str)] = &[
    ("EDITOR", "devin"),
    ("TERM_PROGRAM", "kiro"),
    ("PATH", ".pi/agent"),
];

/// The substring rules' match: case-insensitive containment — the
/// **haystack** is folded; the **needle must already be lowercase**
/// (the `SUBSTRING_RULES` table's rows are, and that contract is part
/// of this predicate, not just the table's doc). The one match both
/// the product detector and the e2e harness's ambient scrub read — so
/// a PATH entry the detector would hit is exactly the shape the scrub
/// filters.
pub(crate) fn substring_hit(value: &str, needle: &str) -> bool {
    value.to_lowercase().contains(needle)
}

/// The first presence variable that marks a known agent harness, reading
/// the variables through `env` so tests need not touch the real
/// environment — the variable's own name is the trigger the mode
/// resolution reports.
pub(crate) fn detected_var(env: &dyn Fn(&str) -> Option<String>) -> Option<&'static str> {
    PRESENCE_VARS
        .iter()
        .find(|name| env(name).is_some_and(|v| !v.is_empty()))
        .copied()
        .or_else(|| {
            SUBSTRING_RULES
                .iter()
                .find(|(name, needle)| env(name).is_some_and(|v| substring_hit(&v, needle)))
                .map(|(name, _)| *name)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    // A local copy, not the canonical `crate::telemetry::env_of` import:
    // the `#[path]` mount into tests/support compiles this file in the
    // e2e crates, where `crate::telemetry` does not exist (the module
    // header's one-copy mount note) — the second copy's stated reason.
    fn env_of<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(name, _)| *name == k)
                .map(|(_, v)| v.to_string())
        }
    }

    /// The vocabulary detects by presence (non-empty) and by the
    /// case-insensitive substring rules, and names the variable that did.
    #[test]
    fn detection_names_its_variable() {
        for name in PRESENCE_VARS {
            assert_eq!(detected_var(&env_of(&[(name, "1")])), Some(*name), "{name}");
            assert_eq!(detected_var(&env_of(&[(name, "")])), None, "{name} empty");
        }
        assert_eq!(
            detected_var(&env_of(&[("AI_AGENT", "claude-code")])),
            Some("AI_AGENT")
        );
        assert_eq!(
            detected_var(&env_of(&[("EDITOR", "/usr/bin/Devin-editor")])),
            Some("EDITOR")
        );
        assert_eq!(
            detected_var(&env_of(&[("TERM_PROGRAM", "KIRO")])),
            Some("TERM_PROGRAM")
        );
        assert_eq!(
            detected_var(&env_of(&[("PATH", "/a:/home/u/.pi/agent/bin")])),
            Some("PATH")
        );
        assert_eq!(detected_var(&env_of(&[("EDITOR", "vim")])), None);
        assert_eq!(detected_var(&env_of(&[])), None);
    }
}
