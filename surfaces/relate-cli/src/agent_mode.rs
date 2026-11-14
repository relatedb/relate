//! The resolutions over the detection vocabulary: the agent-mode ladder
//! (ADR-0407) — the decided [`AgentMode`] and its one resolution, consumed
//! by the row cap's gate and the announcement — and the one invoker
//! classification. Never mounted into the e2e harness — the detection
//! vocabulary they read lives in the mounted `agent_vars` (see that
//! module's header for the split's reason) — so its tests import the
//! canonical helpers.

use crate::agent_vars::detected_var;

/// The effective agent mode (ADR-0407), modeled as the three states
/// the ladder can reach: off, on by an explicit act (the flag or the env
/// var), or on by detection (ADR-0196's gate passed). One resolution for
/// both consumers — the row cap's gate and the announcement — so they can
/// never disagree, and no combination the type admits is nonsense to the
/// domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentMode {
    /// Off — an explicit `--no-agent-mode`/`RELATE_AGENT_MODE=0`, or
    /// detection did not fire (or fired on a terminal).
    Off,
    /// On by a deliberate act, carrying what resolved it — the only
    /// state that announces ([`AgentMode::announcement`]).
    Explicit(&'static str),
    /// On by detection: silent — a per-command CLI never repeats a
    /// banner on every one-shot run, the token cost DuckDB's own
    /// banner data warns about (ADR-0407). The detecting variable is
    /// not carried: no production reader names it (the announcement
    /// speaks only for the explicit acts), so the type carries no
    /// payload it would have to justify.
    Detected,
}

impl AgentMode {
    /// Agent ergonomics are in effect (the row cap's default applies).
    pub(crate) fn is_on(self) -> bool {
        !matches!(self, AgentMode::Off)
    }

    /// The one-line announcement (ADR-0407), authored whole beside the
    /// mode it describes: `Some` only for an explicit on — the
    /// deliberate acts confirm themselves; detection stays silent.
    pub(crate) fn announcement(self) -> Option<String> {
        match self {
            AgentMode::Explicit(trigger) => Some(format!(
                "relate agent mode on: {trigger} (--no-agent-mode to turn off)"
            )),
            _ => None,
        }
    }
}

/// Resolve the effective agent mode (ADR-0407), one ladder:
/// the explicit `--agent-mode`/`--no-agent-mode` flag (`force`), then
/// `RELATE_AGENT_MODE=1/0`, then detection — a non-empty presence of
/// a [`crate::agent_vars::PRESENCE_VARS`] entry, gated on stdout not
/// being a terminal (ADR-0196: a terminal is a human's). An explicit
/// choice overrides that gate: `--agent-mode` asks for agent ergonomics
/// even on a terminal. A `RELATE_AGENT_MODE` that is neither 1 nor 0
/// is a usage error (exit 2), the same contract as
/// `RELATE_MAX_ROWS`.
pub(crate) fn effective_agent_mode(
    force: Option<bool>,
    env: &dyn Fn(&str) -> Option<String>,
    stdout_is_tty: bool,
) -> Result<AgentMode, String> {
    if let Some(forced) = force {
        return Ok(match forced {
            true => AgentMode::Explicit("--agent-mode"),
            false => AgentMode::Off,
        });
    }
    match env("RELATE_AGENT_MODE").filter(|v| !v.trim().is_empty()) {
        Some(value) if value.trim() == "1" => {
            return Ok(AgentMode::Explicit("RELATE_AGENT_MODE=1"));
        }
        Some(value) if value.trim() == "0" => return Ok(AgentMode::Off),
        Some(value) => {
            return Err(format!(
                "invalid RELATE_AGENT_MODE: '{value}' is neither 1 nor 0"
            ));
        }
        None => {}
    }
    Ok(match detected_var(env) {
        Some(_) if !stdout_is_tty => AgentMode::Detected,
        _ => AgentMode::Off,
    })
}

/// [`effective_agent_mode`] against the real environment. The tty fact
/// arrives as a parameter: the caller reads stdout once per run (the
/// output-mode and pretty decisions need it too), so the ladder, the
/// mode default, and the terminal courtesies decide on one fact, read
/// at one moment.
pub(crate) fn effective_agent_mode_from_env(
    force: Option<bool>,
    stdout_is_tty: bool,
) -> Result<AgentMode, String> {
    effective_agent_mode(force, &|k| std::env::var(k).ok(), stdout_is_tty)
}

/// Who launched the process, before a surface's own vocabulary: a detected
/// agent harness, automation (`CI`), or a person. One classification — the
/// telemetry envelope's `invoker` dimension and the feedback report's
/// `invoker` both map it, so a process reports the same answer to each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Launched {
    Agent,
    Ci,
    Human,
}

/// Classify who launched the process: a known agent marker first (the most
/// specific signal), then a truthy `CI`, else a person. Callers spell the
/// answer in their own vocabulary — the telemetry envelope says `ci` where
/// the feedback report says `script`.
pub(crate) fn launched_by(env: &dyn Fn(&str) -> Option<String>) -> Launched {
    if detected_var(env).is_some() {
        return Launched::Agent;
    }
    let ci = env("CI").unwrap_or_default();
    let ci = ci.trim();
    if !ci.is_empty() && ci != "0" && !ci.eq_ignore_ascii_case("false") {
        Launched::Ci
    } else {
        Launched::Human
    }
}

/// Which feedback surface a run may use (ADR-0588 §3): the person's
/// prefilled-GitHub-issue form while agent mode is off, the agent's
/// consented send while it is on. Each surface's command is an unknown
/// command in the other mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FeedbackSurface {
    Human,
    Agent,
}

impl FeedbackSurface {
    /// Resolve from the one agent-mode ladder. A bad `RELATE_AGENT_MODE`
    /// reads as the human surface here — a statement run's own resolution
    /// is the one that fails loud (exit 2).
    pub(crate) fn resolve(
        force: Option<bool>,
        env: &dyn Fn(&str) -> Option<String>,
        stdout_is_tty: bool,
    ) -> Self {
        match effective_agent_mode(force, env, stdout_is_tty) {
            Ok(mode) if mode.is_on() => Self::Agent,
            _ => Self::Human,
        }
    }

    /// [`FeedbackSurface::resolve`] against the real environment.
    pub(crate) fn from_env(force: Option<bool>, stdout_is_tty: bool) -> Self {
        Self::resolve(force, &|k| std::env::var(k).ok(), stdout_is_tty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::env_of;

    /// The ADR-0407 ladder, each rung in turn: the flag, the env var,
    /// detection gated on the terminal — and the error contract.
    #[test]
    fn the_override_ladder_resolves_in_precedence_order() {
        let none = env_of(&[]);
        // The flag wins over everything, both directions.
        assert_eq!(
            effective_agent_mode(
                Some(true),
                &env_of(&[("RELATE_AGENT_MODE", "0"), ("CLAUDECODE", "1")]),
                true
            ),
            Ok(AgentMode::Explicit("--agent-mode"))
        );
        assert_eq!(
            effective_agent_mode(
                Some(false),
                &env_of(&[("RELATE_AGENT_MODE", "1"), ("CLAUDECODE", "1")]),
                false
            ),
            Ok(AgentMode::Off)
        );
        // The env var over detection — and, being explicit, over the
        // terminal gate too.
        assert_eq!(
            effective_agent_mode(
                None,
                &env_of(&[("RELATE_AGENT_MODE", "1"), ("CLAUDECODE", "1")]),
                true
            ),
            Ok(AgentMode::Explicit("RELATE_AGENT_MODE=1"))
        );
        assert_eq!(
            effective_agent_mode(
                None,
                &env_of(&[("RELATE_AGENT_MODE", "0"), ("CLAUDECODE", "1")]),
                false
            ),
            Ok(AgentMode::Off)
        );
        // Detection: on when a harness is present and stdout is not a
        // terminal (ADR-0196's gate), off on a terminal.
        assert_eq!(effective_agent_mode(None, &none, false), Ok(AgentMode::Off));
        assert_eq!(
            effective_agent_mode(None, &env_of(&[("CLAUDECODE", "1")]), false),
            Ok(AgentMode::Detected)
        );
        assert_eq!(
            effective_agent_mode(None, &env_of(&[("CLAUDECODE", "1")]), true),
            Ok(AgentMode::Off)
        );
        // The substring rules still detect, and the generic vars count —
        // the mode is the fact (which variable named it is pinned on the
        // detector's own test, in `agent_vars`).
        for (name, value) in [
            ("AI_AGENT", "claude-code"),
            ("AGENT", "1"),
            ("EDITOR", "Devin Editor"),
        ] {
            assert_eq!(
                effective_agent_mode(None, &env_of(&[(name, value)]), false),
                Ok(AgentMode::Detected)
            );
        }
        // An explicit value that is neither 1 nor 0 is a usage error.
        assert!(
            effective_agent_mode(None, &env_of(&[("RELATE_AGENT_MODE", "yes")]), false).is_err()
        );
    }

    /// The announcement: the deliberate acts' own method, the only
    /// states that speak (ADR-0407) — one author, the full line.
    #[test]
    fn the_announcement_speaks_only_for_the_explicit_acts() {
        assert_eq!(
            AgentMode::Explicit("--agent-mode").announcement(),
            Some("relate agent mode on: --agent-mode (--no-agent-mode to turn off)".to_string())
        );
        assert_eq!(
            AgentMode::Explicit("RELATE_AGENT_MODE=1").announcement(),
            Some(
                "relate agent mode on: RELATE_AGENT_MODE=1 (--no-agent-mode to turn off)"
                    .to_string()
            )
        );
        assert_eq!(AgentMode::Detected.announcement(), None);
        assert_eq!(AgentMode::Off.announcement(), None);
    }

    /// The feedback surfaces split on the same ladder: the person's while
    /// agent mode is off, the agent's while it is on.
    #[test]
    fn the_feedback_surface_follows_the_agent_mode() {
        assert_eq!(
            FeedbackSurface::resolve(None, &env_of(&[]), false),
            FeedbackSurface::Human
        );
        assert_eq!(
            FeedbackSurface::resolve(Some(false), &env_of(&[("CLAUDECODE", "1")]), false),
            FeedbackSurface::Human,
            "an explicit --no-agent-mode keeps the person's surface"
        );
        assert_eq!(
            FeedbackSurface::resolve(None, &env_of(&[("CLAUDECODE", "1")]), false),
            FeedbackSurface::Agent
        );
        assert_eq!(
            FeedbackSurface::resolve(None, &env_of(&[("RELATE_AGENT_MODE", "1")]), true),
            FeedbackSurface::Agent
        );
        // A bad value reads as the person's surface here; the statement
        // run's own resolution is the one that fails loud.
        assert_eq!(
            FeedbackSurface::resolve(None, &env_of(&[("RELATE_AGENT_MODE", "yes")]), false),
            FeedbackSurface::Human
        );
    }
}
