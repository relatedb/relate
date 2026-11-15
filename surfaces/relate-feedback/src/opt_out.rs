//! Layered telemetry opt-out (ADR-0079 §6). First match wins:
//!
//! 1. `DO_NOT_TRACK` set to `1` or `true` (the consoledonottrack
//!    convention)
//! 2. `RELATE_TELEMETRY` (`0`/`false`/`off` disables; `1`/`true`/`on`
//!    enables; unrecognized values fall through)
//! 3. `telemetry: false` (or `true`) in `<config>/relate/config.json`
//! 4. default: enabled
//!
//! The environment lookup is injected (`&dyn Fn(&str) -> Option<String>`)
//! so the precedence matrix is unit-testable without mutating process env.
//!
//! The feedback consent record lives in [`crate::authorization`].

use crate::state::ConfigTelemetry;

/// The resolved telemetry state: the verdict and the layer that decided it
/// (surfaced by `relate telemetry status`, ADR-0079 §6). Every variant is a
/// state the ladder can produce, so no consumer has to fold away an
/// impossible combination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveState {
    /// Nothing was set anywhere; telemetry defaults to on.
    Default,
    /// `RELATE_TELEMETRY` held a recognized on/off value.
    Env(bool),
    /// `config.json` held a `telemetry` bool; an unreadable file reads as
    /// `ConfigFile(false)`, so a corrupt file cannot re-enable telemetry.
    ConfigFile(bool),
    /// `DO_NOT_TRACK=1|true`.
    DoNotTrack,
}

impl EffectiveState {
    /// Whether telemetry is on.
    pub fn enabled(self) -> bool {
        match self {
            EffectiveState::Default
            | EffectiveState::Env(true)
            | EffectiveState::ConfigFile(true) => true,
            EffectiveState::Env(false)
            | EffectiveState::ConfigFile(false)
            | EffectiveState::DoNotTrack => false,
        }
    }

    /// The layer's name in the status lines (`relate telemetry status`,
    /// the bare `relate feedback`): one spelling, so the two commands that
    /// report this fact cannot drift apart.
    pub fn label(self) -> &'static str {
        match self {
            EffectiveState::DoNotTrack => "DO_NOT_TRACK",
            EffectiveState::Env(_) => "RELATE_TELEMETRY",
            EffectiveState::ConfigFile(_) => "config file",
            EffectiveState::Default => "default",
        }
    }

    /// The verdict line both commands print, e.g. `enabled (default)` or
    /// `disabled (DO_NOT_TRACK)` — the verdict word and the deciding layer in
    /// one home, so a second printer cannot re-spell either half.
    pub fn status_line(self) -> String {
        format!(
            "{} ({})",
            if self.enabled() {
                "enabled"
            } else {
                "disabled"
            },
            self.label()
        )
    }
}

/// Resolve the effective state. `env` looks up an environment variable
/// (returning `None` when unset); `config_telemetry` is the typed
/// `telemetry` read from `config.json`.
pub fn resolve(
    env: &dyn Fn(&str) -> Option<String>,
    config_telemetry: ConfigTelemetry,
) -> EffectiveState {
    if let Some(v) = env("DO_NOT_TRACK") {
        let v = v.trim();
        if v == "1" || v.eq_ignore_ascii_case("true") {
            return EffectiveState::DoNotTrack;
        }
    }
    if let Some(v) = env("RELATE_TELEMETRY") {
        // An unrecognized value falls through to the next layer.
        if let Some(enabled) = parse_on_off(&v) {
            return EffectiveState::Env(enabled);
        }
    }
    match config_telemetry {
        ConfigTelemetry::Value(enabled) => EffectiveState::ConfigFile(enabled),
        // The file cannot be trusted: fail closed rather than fall through to
        // the enabled default.
        ConfigTelemetry::Unreadable => EffectiveState::ConfigFile(false),
        ConfigTelemetry::Missing => EffectiveState::Default,
    }
}

/// Parse an on/off value: `0`/`false`/`off` disable, `1`/`true`/`on`
/// enable (trimmed, case-insensitive). Anything else is `None`.
fn parse_on_off(v: &str) -> Option<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "0" | "false" | "off" => Some(false),
        "1" | "true" | "on" => Some(true),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::env_of;

    /// The three config layers' typed reads, for the precedence matrix.
    const NONE: ConfigTelemetry = ConfigTelemetry::Missing;
    const OFF: ConfigTelemetry = ConfigTelemetry::Value(false);
    const ON: ConfigTelemetry = ConfigTelemetry::Value(true);

    #[test]
    fn nothing_set_means_enabled_by_default() {
        assert_eq!(resolve(&env_of(&[]), NONE), EffectiveState::Default);
        assert!(resolve(&env_of(&[]), NONE).enabled());
    }

    #[test]
    fn do_not_track_one_or_true_disables() {
        for v in ["1", "true", "TRUE", " True "] {
            let s = resolve(&env_of(&[("DO_NOT_TRACK", v)]), NONE);
            assert_eq!(s, EffectiveState::DoNotTrack, "DO_NOT_TRACK={v:?}");
            assert!(!s.enabled());
        }
    }

    #[test]
    fn do_not_track_beats_config_true() {
        assert_eq!(
            resolve(&env_of(&[("DO_NOT_TRACK", "1")]), ON),
            EffectiveState::DoNotTrack
        );
    }

    #[test]
    fn do_not_track_beats_relate_telemetry_on() {
        assert_eq!(
            resolve(
                &env_of(&[("DO_NOT_TRACK", "1"), ("RELATE_TELEMETRY", "1")]),
                NONE
            ),
            EffectiveState::DoNotTrack
        );
    }

    #[test]
    fn do_not_track_other_values_fall_through() {
        for v in ["0", "false", "yes", ""] {
            assert_eq!(
                resolve(&env_of(&[("DO_NOT_TRACK", v)]), NONE),
                EffectiveState::Default,
                "DO_NOT_TRACK={v:?}"
            );
        }
    }

    #[test]
    fn relate_telemetry_off_values_disable() {
        for v in ["0", "false", "off", "OFF", " False "] {
            assert_eq!(
                resolve(&env_of(&[("RELATE_TELEMETRY", v)]), NONE),
                EffectiveState::Env(false),
                "RELATE_TELEMETRY={v:?}"
            );
        }
    }

    #[test]
    fn relate_telemetry_on_values_enable() {
        for v in ["1", "true", "on", "ON", " True "] {
            assert_eq!(
                resolve(&env_of(&[("RELATE_TELEMETRY", v)]), NONE),
                EffectiveState::Env(true),
                "RELATE_TELEMETRY={v:?}"
            );
        }
    }

    #[test]
    fn relate_telemetry_zero_beats_config_true() {
        assert_eq!(
            resolve(&env_of(&[("RELATE_TELEMETRY", "0")]), ON),
            EffectiveState::Env(false)
        );
    }

    #[test]
    fn relate_telemetry_on_beats_config_false() {
        assert_eq!(
            resolve(&env_of(&[("RELATE_TELEMETRY", "on")]), OFF),
            EffectiveState::Env(true)
        );
    }

    #[test]
    fn unrecognized_env_value_falls_through_to_config() {
        assert_eq!(
            resolve(&env_of(&[("RELATE_TELEMETRY", "bogus")]), OFF),
            EffectiveState::ConfigFile(false)
        );
    }

    #[test]
    fn config_file_false_disables() {
        let s = resolve(&env_of(&[]), OFF);
        assert_eq!(s, EffectiveState::ConfigFile(false));
        assert!(!s.enabled());
    }

    #[test]
    fn config_file_true_enables() {
        assert_eq!(resolve(&env_of(&[]), ON), EffectiveState::ConfigFile(true));
    }

    /// The one status line both commands print: the verdict word plus the
    /// deciding layer, for every state the ladder can reach.
    #[test]
    fn status_line_names_the_verdict_and_its_layer() {
        assert_eq!(EffectiveState::Default.status_line(), "enabled (default)");
        assert_eq!(
            EffectiveState::ConfigFile(true).status_line(),
            "enabled (config file)"
        );
        assert_eq!(
            EffectiveState::Env(true).status_line(),
            "enabled (RELATE_TELEMETRY)"
        );
        assert_eq!(
            EffectiveState::ConfigFile(false).status_line(),
            "disabled (config file)"
        );
        assert_eq!(
            EffectiveState::Env(false).status_line(),
            "disabled (RELATE_TELEMETRY)"
        );
        assert_eq!(
            EffectiveState::DoNotTrack.status_line(),
            "disabled (DO_NOT_TRACK)"
        );
    }

    /// The typed read keeps an unreadable file apart from an absent key, and
    /// the ladder fails closed on it: corrupting a `config.json` that recorded
    /// `telemetry: false` cannot re-enable telemetry.
    #[test]
    fn a_corrupt_config_cannot_re_enable_telemetry() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        crate::state::save_config_telemetry(dir, false).unwrap();
        let recorded = crate::state::load_config_telemetry(dir);
        assert_eq!(recorded, ConfigTelemetry::Value(false));
        assert!(!resolve(&env_of(&[]), recorded).enabled());

        std::fs::write(crate::state::config_path(dir), "{\"telemetry\": false").unwrap();
        let corrupt = crate::state::load_config_telemetry(dir);
        assert_eq!(corrupt, ConfigTelemetry::Unreadable);
        assert!(
            !resolve(&env_of(&[]), corrupt).enabled(),
            "a corrupt file must not fall through to the enabled default"
        );
    }
}
