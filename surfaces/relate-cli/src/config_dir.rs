//! The CLI's config directory (ADR-0079 §3, extended by ADR-0545 §6):
//! `$XDG_CONFIG_HOME/relate/` when `$XDG_CONFIG_HOME` is set, non-empty,
//! and absolute, else `~/.config/relate/`, else — the Windows bases —
//! `%APPDATA%\relate\`, finally `%USERPROFILE%\.config\relate\` — the
//! one resolution every config-dir walk shares: telemetry's state,
//! opt-out, and spool files (ADR-0079, ADR-0545) and the extension
//! registry's discovery directory (ADR-0429 §6). The two consumers
//! walked their own copy of this rule before the extension program
//! needed a third; the fact now lives here, once.

use std::path::PathBuf;

use crate::home_dir::absolute_base;

/// The config directory's name under whichever base resolves — the one
/// segment every config-dir join shares (the resolution below, the
/// registry's extensions dir, the test kit's fixture installs), so the
/// name cannot be re-spelled per join site.
pub(crate) const CONFIG_DIR_NAME: &str = "relate";

/// The config directory: `$XDG_CONFIG_HOME/relate/` when
/// `$XDG_CONFIG_HOME` is set, non-empty, and absolute, else
/// `~/.config/relate/`, else `%APPDATA%\relate\`, else
/// `%USERPROFILE%\.config\relate\`. `None` when no base resolves.
///
/// A relative `$XDG_CONFIG_HOME` (or any other base, through the shared
/// `home_dir::absolute_base` rule), or any base holding a `..` component, is
/// treated as unset, exactly like an empty one: a config dir resolved against the current working
/// directory would make every consumer of this rule cwd-dependent —
/// for the registry's walk that is the drive-by-execution guard undone
/// (ADR-0429 §6: the grammar is a property of the machine, never of the
/// directory the command runs in), so a relative value never names a
/// base.
///
/// The Windows bases resolve only when the POSIX pair does not: stock
/// Windows sets neither `$XDG_CONFIG_HOME` nor `$HOME`, so before
/// ADR-0545 every walk here returned `None` there — telemetry ran
/// stateless (a fresh per-process install ID, a first-run notice that
/// never persisted) and the registry discovered nothing. A POSIX-style
/// shell on Windows (which sets `$HOME`) keeps the `~/.config` path and
/// its existing state; `%APPDATA%` is the Windows-native per-user
/// config root, with `%USERPROFILE%` covering environments that clear
/// it.
///
/// The real-env twin of [`config_dir_with`], called by the telemetry
/// surfaces (`telemetry_cmd`'s runs, the handle's event walks) and by
/// the registry's real-env entry (`ActionRegistry::load` in
/// `extensions::registry`, ADR-0429 §6).
#[cfg(any(feature = "telemetry", feature = "extensions"))]
pub(crate) fn config_dir() -> Option<PathBuf> {
    config_dir_with(&|k| std::env::var(k).ok())
}

/// The user-facing hint for the unresolved-base error, stated once
/// beside the resolution it names so the advertised bases cannot drift
/// from the chain. Consumed by the `telemetry` command and the
/// extension surfaces' no-base refusals, hence the gate.
#[cfg(any(feature = "telemetry", feature = "extensions"))]
pub(crate) const NO_BASE_HINT: &str = "set $XDG_CONFIG_HOME, $HOME, $APPDATA, or $USERPROFILE";

/// Injectable-env variant of [`config_dir`]: the same resolution with
/// the base read through `env`, so tests and the registry's injectable
/// loader can pin it against a fixture environment.
pub(crate) fn config_dir_with(env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if let Some(xdg) = absolute_base(env, "XDG_CONFIG_HOME") {
        return Some(xdg.join(CONFIG_DIR_NAME));
    }
    if let Some(home) = absolute_base(env, "HOME") {
        return Some(home.join(".config").join(CONFIG_DIR_NAME));
    }
    // The Windows bases (ADR-0545 §6), reached only when the POSIX pair
    // is absent — stock Windows sets neither, and its native values are
    // absolute by construction.
    if let Some(appdata) = absolute_base(env, "APPDATA") {
        return Some(appdata.join(CONFIG_DIR_NAME));
    }
    absolute_base(env, "USERPROFILE").map(|profile| profile.join(".config").join(CONFIG_DIR_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::home_dir::host_absolute;
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

    #[test]
    fn appdata_resolves_when_the_posix_pair_is_absent() {
        // Stock Windows sets neither XDG nor HOME, so APPDATA — the
        // Windows-native per-user config root — is the first base that
        // can resolve (ADR-0545 §6). The value must be absolute on the
        // host running the test: Windows wants a drive prefix, everything
        // else a leading slash (a real Windows host supplies the former
        // natively).
        let appdata = host_absolute("appdata");
        let dir = config_dir_with(&env_of(&[("APPDATA", appdata.as_str())]));
        assert_eq!(dir, Some(PathBuf::from(appdata).join("relate")));
    }

    #[test]
    fn a_traversing_base_falls_through_to_the_next_tier() {
        // CWE-22: a `..` component disqualifies a base (the shared
        // home_dir rule), so APPDATA's traversing value yields to
        // USERPROFILE instead of steering the config dir elsewhere.
        let appdata = host_absolute("appdata/../windows");
        let profile = host_absolute("users/u");
        let pairs = [
            ("APPDATA", appdata.as_str()),
            ("USERPROFILE", profile.as_str()),
        ];
        assert_eq!(
            config_dir_with(&env_of(&pairs)),
            Some(
                PathBuf::from(profile.as_str())
                    .join(".config")
                    .join("relate")
            )
        );
    }

    #[test]
    fn userprofile_dot_config_is_the_last_resort() {
        let profile = host_absolute("users/u");
        let dir = config_dir_with(&env_of(&[("USERPROFILE", profile.as_str())]));
        assert_eq!(
            dir,
            Some(
                PathBuf::from(profile.as_str())
                    .join(".config")
                    .join("relate")
            )
        );
        // An empty APPDATA is treated as unset, exactly like an empty XDG.
        let dir = config_dir_with(&env_of(&[
            ("APPDATA", ""),
            ("USERPROFILE", profile.as_str()),
        ]));
        assert_eq!(
            dir,
            Some(
                PathBuf::from(profile.as_str())
                    .join(".config")
                    .join("relate")
            )
        );
    }

    #[test]
    fn home_still_wins_over_the_windows_bases() {
        // A POSIX-style shell on Windows sets HOME: its existing
        // ~/.config/relate state must not move under APPDATA.
        let appdata = host_absolute("appdata");
        let profile = host_absolute("users/u");
        let dir = config_dir_with(&env_of(&[
            ("HOME", "/home/u"),
            ("APPDATA", appdata.as_str()),
            ("USERPROFILE", profile.as_str()),
        ]));
        assert_eq!(dir, Some(PathBuf::from("/home/u/.config/relate")));
    }

    #[test]
    fn a_relative_windows_base_never_names_a_base() {
        // The absolute gate on the new bases: a relative value would make
        // the config dir cwd-dependent (the module doc's rule), so it is
        // treated as unset and resolution falls through — to nothing,
        // when no other base is set.
        assert_eq!(
            config_dir_with(&env_of(&[("APPDATA", "relappdata")])),
            None,
            "a relative APPDATA must not resolve"
        );
        assert_eq!(
            config_dir_with(&env_of(&[("USERPROFILE", "relprofile")])),
            None,
            "a relative USERPROFILE must not resolve"
        );
    }
}
