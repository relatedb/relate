//! The CLI's config directory (ADR-0079 §3): `$XDG_CONFIG_HOME/relate/`
//! when `$XDG_CONFIG_HOME` is set, non-empty, and absolute, else
//! `~/.config/relate/` — the one resolution every config-dir walk
//! shares: telemetry's state and opt-out files (ADR-0079) and the
//! extension registry's discovery directory (ADR-0429 §6). The two
//! consumers walked their own copy of this rule before the extension
//! program needed a third; the fact now lives here, once.

use std::path::{Path, PathBuf};

/// The config directory's name under whichever base resolves — the one
/// segment every config-dir join shares (the resolution below, the
/// registry's extensions dir, the test kit's fixture installs), so the
/// name cannot be re-spelled per join site.
pub(crate) const CONFIG_DIR_NAME: &str = "relate";

/// The config directory: `$XDG_CONFIG_HOME/relate/` when
/// `$XDG_CONFIG_HOME` is set, non-empty, and absolute, else
/// `~/.config/relate/`. `None` when neither base resolves.
///
/// A relative `$XDG_CONFIG_HOME` is treated as unset, exactly like an
/// empty one: a config dir resolved against the current working
/// directory would make every consumer of this rule cwd-dependent —
/// for the registry's walk that is the drive-by-execution guard undone
/// (ADR-0429 §6: the grammar is a property of the machine, never of the
/// directory the command runs in), so a relative value never names a
/// base.
///
/// The real-env twin of [`config_dir_with`], called by the telemetry
/// surfaces (`telemetry_cmd`'s runs, the handle's event walks) and by
/// the registry's real-env entry (`ActionRegistry::load` in
/// `extensions::registry`, ADR-0429 §6).
#[cfg(any(feature = "telemetry", feature = "extensions"))]
pub(crate) fn config_dir() -> Option<PathBuf> {
    config_dir_with(&|k| std::env::var(k).ok())
}

/// Injectable-env variant of [`config_dir`]: the same resolution with
/// the base read through `env`, so tests and the registry's injectable
/// loader can pin it against a fixture environment.
pub(crate) fn config_dir_with(env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    // Set, non-empty, and absolute — anything less is unset (see the
    // module doc for why a relative value can never name a base).
    if let Some(xdg) = env("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        if Path::new(&xdg).is_absolute() {
            return Some(PathBuf::from(xdg).join(CONFIG_DIR_NAME));
        }
    }
    let home = env("HOME").filter(|v| !v.is_empty())?;
    Some(PathBuf::from(home).join(".config").join(CONFIG_DIR_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::env_of;

    #[test]
    fn config_dir_prefers_xdg_config_home() {
        let dir = config_dir_with(&env_of(&[("XDG_CONFIG_HOME", "/xdg"), ("HOME", "/home/u")]));
        assert_eq!(dir, Some(PathBuf::from("/xdg/relate")));
    }

    #[test]
    fn config_dir_falls_back_to_home_dot_config() {
        let dir = config_dir_with(&env_of(&[("HOME", "/home/u")]));
        assert_eq!(dir, Some(PathBuf::from("/home/u/.config/relate")));
        // Empty XDG is treated as unset.
        let dir = config_dir_with(&env_of(&[("XDG_CONFIG_HOME", ""), ("HOME", "/home/u")]));
        assert_eq!(dir, Some(PathBuf::from("/home/u/.config/relate")));
    }

    #[test]
    fn config_dir_none_without_any_base() {
        assert_eq!(config_dir_with(&env_of(&[])), None);
    }

    #[test]
    fn a_relative_xdg_config_home_is_treated_as_unset() {
        // A relative value never names a base: a config dir resolved
        // against the current working directory would make the registry's
        // walk cwd-dependent — the drive-by-execution guard undone
        // (ADR-0429 §6) — so it resolves exactly like an empty one: the
        // $HOME fallback decides, and with nothing else to resolve there
        // is no config dir at all (the consumer's own authored refusal —
        // the install's "no config directory resolves").
        let dir = config_dir_with(&env_of(&[
            ("XDG_CONFIG_HOME", "relconf"),
            ("HOME", "/home/u"),
        ]));
        assert_eq!(dir, Some(PathBuf::from("/home/u/.config/relate")));
        assert_eq!(
            config_dir_with(&env_of(&[("XDG_CONFIG_HOME", "relconf")])),
            None
        );
    }
}
