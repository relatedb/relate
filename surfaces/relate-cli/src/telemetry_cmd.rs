//! `relate telemetry on|off|status` (ADR-0079 §6): persistent control of
//! anonymous usage metrics, following the ADR-0069 lifecycle-command grammar
//! (first positional only; `./telemetry` and `-- telemetry` stay db paths).
//!
//! `on`/`off` persist the `telemetry` key in `<config>/relate/config.json`
//! via the shared config helpers; `status` reports the already-resolved
//! effective state (env layers included), so it honestly reflects the same
//! precedence as `Telemetry::new`.

use std::path::Path;

use crate::parse_error::ParseError;
use crate::telemetry::config;
use crate::telemetry::{EffectiveState, Layer};

/// What `relate telemetry` should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryAction {
    /// Persist `telemetry: true` in `config.json`.
    On,
    /// Persist `telemetry: false` in `config.json`.
    Off,
    /// Print the effective state and the layer that decided it.
    Status,
}

impl TelemetryAction {
    /// The CLI spelling of the action (also the telemetry event value).
    pub fn as_str(&self) -> &'static str {
        match self {
            TelemetryAction::On => "on",
            TelemetryAction::Off => "off",
            TelemetryAction::Status => "status",
        }
    }
}

/// Parse the argument tail after the `telemetry` verb. Pure — returns the
/// usage-error message instead of exiting, so the grammar is unit-testable;
/// `main` turns `Err` into the hand-rolled parser's exit-2 convention.
pub fn parse_action(rest: &[String]) -> Result<TelemetryAction, ParseError> {
    let Some(action_arg) = rest.first() else {
        return Err("telemetry requires an action (on, off, or status)".into());
    };
    let action = match action_arg.as_str() {
        "on" => TelemetryAction::On,
        "off" => TelemetryAction::Off,
        "status" => TelemetryAction::Status,
        other => return Err(ParseError::unexpected("telemetry", other)),
    };
    if let Some(extra) = rest.get(1) {
        return Err(ParseError::unexpected("telemetry", extra));
    }
    Ok(action)
}

/// Run `relate telemetry on|off|status`. Returns the process exit code
/// (0 on success, 1 when no config dir resolves or the write fails); the
/// caller (main) owns `std::process::exit`. The effective state comes
/// from `telemetry` itself — the same handle `Telemetry::new` resolved —
/// so `status` reports the same precedence (env layers override the
/// config file) with no second copy of the answer to drift.
pub fn run(action: TelemetryAction, telemetry: &mut crate::telemetry::Telemetry) -> i32 {
    run_inner(
        action,
        telemetry,
        crate::config_dir::config_dir().as_deref(),
    )
}

fn run_inner(
    action: TelemetryAction,
    telemetry: &mut crate::telemetry::Telemetry,
    dir: Option<&Path>,
) -> i32 {
    match action {
        TelemetryAction::Status => {
            println!("{}", status_line(telemetry.effective_state()));
            0
        }
        TelemetryAction::On => persist(dir, true),
        TelemetryAction::Off => {
            // One ordered transition (ADR-0545 §5): disarm the run's
            // pipeline first — the drain starts at the first recorded
            // event and this command records none from here on, so no
            // delivery is ever started and the exit writes no spool
            // file of its own — then persist the off, then clear the
            // pending spool. `persist` owns the unresolved-dir error,
            // and its success implies the dir resolved.
            telemetry.disarm();
            let code = persist(dir, false);
            if code == 0 {
                let dir = dir.expect("persist returns 0 only with a resolved dir");
                crate::telemetry::spool::clear(dir);
            }
            code
        }
    }
}

/// Persist the `telemetry` key and print a short confirmation.
fn persist(dir: Option<&Path>, enabled: bool) -> i32 {
    let Some(dir) = dir else {
        eprintln!(
            "Error: telemetry: cannot resolve a config directory ({})",
            crate::config_dir::NO_BASE_HINT
        );
        return 1;
    };
    if let Err(e) = config::save_config_telemetry(dir, enabled) {
        crate::print_error(format!(
            "Error: telemetry: writing {}: {e}",
            config::config_path(dir).display()
        ));
        return 1;
    }
    println!(
        "telemetry {} ({})",
        if enabled { "enabled" } else { "disabled" },
        config::config_path(dir).display()
    );
    0
}

/// The `status` output line: effective state plus the deciding layer, e.g.
/// `enabled (default)` or `disabled (DO_NOT_TRACK)`.
fn status_line(state: EffectiveState) -> String {
    let layer = match state.layer {
        Layer::DoNotTrack => "DO_NOT_TRACK",
        Layer::Env => "RELATE_TELEMETRY",
        Layer::ConfigFile => "config file",
        Layer::Default => "default",
    };
    format!(
        "{} ({layer})",
        if state.enabled { "enabled" } else { "disabled" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(enabled: bool, layer: Layer) -> EffectiveState {
        EffectiveState { enabled, layer }
    }

    /// A handle carrying exactly the effective state its test asserts —
    /// the state is the reportable answer, so a disabled pipeline with an
    /// enabled state is a legitimate (and pinned) shape. The `off` arm's
    /// disarm is a no-op against it, exactly as against a run that never
    /// armed a pipeline; the live-path ordering (disarm before persist
    /// and clear) is pinned end-to-end by e2e_telemetry's off test.
    fn handle_as(enabled: bool, layer: Layer) -> crate::telemetry::Telemetry {
        crate::telemetry::Telemetry::disabled(state(enabled, layer))
    }

    // --- parse_action grammar ---

    #[test]
    fn parse_all_actions() {
        let s = |v: &str| v.to_string();
        assert_eq!(parse_action(&[s("on")]), Ok(TelemetryAction::On));
        assert_eq!(parse_action(&[s("off")]), Ok(TelemetryAction::Off));
        assert_eq!(parse_action(&[s("status")]), Ok(TelemetryAction::Status));
    }

    #[test]
    fn parse_bare_verb_requires_action() {
        assert_eq!(
            parse_action(&[]),
            Err("telemetry requires an action (on, off, or status)".into())
        );
    }

    #[test]
    fn parse_unknown_action_is_unexpected_argument() {
        assert_eq!(
            parse_action(&["frobnicate".to_string()]),
            Err(ParseError::unexpected("telemetry", "frobnicate"))
        );
    }

    #[test]
    fn parse_rejects_extra_arguments_after_action() {
        assert_eq!(
            parse_action(&["on".to_string(), ":memory:".to_string()]),
            Err(ParseError::unexpected("telemetry", ":memory:"))
        );
        assert_eq!(
            parse_action(&["status".to_string(), "extra".to_string()]),
            Err(ParseError::unexpected("telemetry", "extra"))
        );
    }

    // --- status line ---

    #[test]
    fn status_line_names_state_and_layer() {
        assert_eq!(
            status_line(state(true, Layer::Default)),
            "enabled (default)"
        );
        assert_eq!(
            status_line(state(false, Layer::ConfigFile)),
            "disabled (config file)"
        );
        assert_eq!(
            status_line(state(false, Layer::DoNotTrack)),
            "disabled (DO_NOT_TRACK)"
        );
        assert_eq!(
            status_line(state(false, Layer::Env)),
            "disabled (RELATE_TELEMETRY)"
        );
        assert_eq!(
            status_line(state(true, Layer::Env)),
            "enabled (RELATE_TELEMETRY)"
        );
        assert_eq!(
            status_line(state(true, Layer::ConfigFile)),
            "enabled (config file)"
        );
    }

    // --- on/off against a temp config dir ---

    #[test]
    fn on_and_off_persist_the_config_key() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        assert_eq!(
            run_inner(
                TelemetryAction::Off,
                &mut handle_as(true, Layer::Default),
                Some(dir)
            ),
            0
        );
        assert_eq!(config::load_config_telemetry(dir), Some(false));
        assert_eq!(
            run_inner(
                TelemetryAction::On,
                &mut handle_as(true, Layer::Default),
                Some(dir)
            ),
            0
        );
        assert_eq!(config::load_config_telemetry(dir), Some(true));
    }

    #[test]
    fn off_clears_the_spool_but_on_leaves_it() {
        // ADR-0545 §5: `off` means no further transmissions — pending
        // spooled events are cleared together with the persisted key;
        // `on` persists the key only.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // The seed name derives from the one prefix fact, so a rename
        // cannot leave this test seeding a file the sweep would ignore.
        let seed = format!("{}seed.jsonl", crate::telemetry::spool::SPOOL_PREFIX);
        std::fs::write(dir.join(&seed), "{\"event\":\"e\"}\n").unwrap();
        assert_eq!(
            run_inner(
                TelemetryAction::On,
                &mut handle_as(true, Layer::Default),
                Some(dir)
            ),
            0
        );
        assert!(dir.join(&seed).exists(), "on must not clear the spool");
        assert_eq!(
            run_inner(
                TelemetryAction::Off,
                &mut handle_as(true, Layer::Default),
                Some(dir)
            ),
            0
        );
        assert!(!dir.join(&seed).exists(), "off clears the spool");
    }

    #[test]
    fn status_does_not_touch_the_config_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        assert_eq!(
            run_inner(
                TelemetryAction::Status,
                &mut handle_as(false, Layer::Env),
                Some(dir)
            ),
            0
        );
        assert_eq!(config::load_config_telemetry(dir), None);
        assert!(!config::config_path(dir).exists());
    }

    #[test]
    fn on_off_without_config_dir_is_exit_1() {
        assert_eq!(
            run_inner(
                TelemetryAction::On,
                &mut handle_as(true, Layer::Default),
                None
            ),
            1
        );
        assert_eq!(
            run_inner(
                TelemetryAction::Off,
                &mut handle_as(true, Layer::Default),
                None
            ),
            1
        );
        // Status never needs the config dir.
        assert_eq!(
            run_inner(
                TelemetryAction::Status,
                &mut handle_as(true, Layer::Default),
                None
            ),
            0
        );
    }
}
