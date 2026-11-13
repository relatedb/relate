//! The per-user base reads every home-relative CLI path shares: the one
//! read rule (set, non-empty and absolute) and the home directory it
//! resolves. The config directory (`config_dir`,
//! ADR-0545 §6) and the agent catalog's `~` expansion (`skill::agents`)
//! both read their bases through here, so the rules are spelled once.
//!
//! The home directory is `$HOME` when set, non-empty and absolute, else
//! `%USERPROFILE%` under the same rule. A relative `$HOME` names no base:
//! home-relative paths resolved against the working directory would make
//! every consumer cwd-dependent. On Windows that includes a rooted value
//! with no drive (`/c/Users/u`, `\Users\u`), which resolves against the
//! current drive; Git Bash, MSYS2 and Cygwin convert `$HOME` to its native
//! `C:\…` spelling for the native programs they launch. Stock PowerShell and
//! `cmd` on Windows set no `$HOME`, so without the second tier every
//! home-relative path resolved to nothing there. A POSIX-style shell on
//! Windows (Git Bash, MSYS, Cygwin) sets `$HOME`, which keeps priority so
//! its existing state does not move. `std::env::home_dir` is not used:
//! it reads `%USERPROFILE%` before `$HOME` on Windows, and it cannot take
//! an injected environment for tests.
//!
//! Every base also refuses a `..` component (CWE-22): a value such as
//! `/home/u/../../etc` names no base, so no home-relative write can be
//! steered outside the directory the variable appears to name. A base that
//! is itself a symlink is resolved to its target, and the target must pass
//! the same rules; a dangling or unreadable link names no base. Only the
//! base itself is resolved: links earlier in the path (macOS `/var`, an
//! automounted `/home`) are ordinary and are left as written.
//!
//! The reads take an `Option<String>` environment: a variable that is
//! unset, empty, or not valid UTF-8 names no base, and resolution falls
//! through to the next tier. That differs on purpose from
//! `release_channel::env_nonempty`, which refuses a non-UTF-8 value: a
//! release seam must never silently fall back to its default, while a home
//! or config base with another tier to try degrades to that tier.

use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf, Prefix};

/// One base read's shared half: set and non-empty. An empty value never
/// names a base — it is how shells and CI runners spell "cleared".
fn non_empty(env: &dyn Fn(&str) -> Option<String>, key: &str) -> Option<String> {
    env(key).filter(|v| !v.is_empty())
}

/// The traversal rule every base shares: no `..` component. One check, so
/// the `$HOME` tier and the absolute tiers cannot disagree on it.
fn free_of_traversal(path: &Path) -> bool {
    !path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
}

/// The one base read every tier shares: set, non-empty, absolute, and free
/// of `..`. A relative value never names a base, because a base resolved
/// against the current working directory (or, on Windows, the current
/// drive) would make every consumer cwd-dependent; a Windows host's native
/// `%APPDATA%` / `%USERPROFILE%` values are absolute by construction. A
/// base that is itself a symlink resolves to its canonical target, which
/// must pass the same checks; a dangling link, or a base whose kind cannot
/// be read (permission denied, I/O error), names no base. A base that does
/// not exist yet is kept as written: a config directory is created on
/// demand.
pub(crate) fn absolute_base(env: &dyn Fn(&str) -> Option<String>, key: &str) -> Option<PathBuf> {
    let passes = |path: &Path| path.is_absolute() && free_of_traversal(path);
    let path = PathBuf::from(non_empty(env, key)?);
    if !passes(&path) {
        return None;
    }
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            let target = without_verbatim_prefix(std::fs::canonicalize(&path).ok()?);
            passes(&target).then_some(target)
        }
        Ok(_) => Some(path),
        Err(error) if error.kind() == ErrorKind::NotFound => Some(path),
        Err(_) => None,
    }
}

/// `canonicalize` on Windows answers in the verbatim form; a resolved base
/// reads like any other path once the verbatim prefix is rebuilt in its
/// plain spelling: a verbatim disk (`\\?\C:\…`) becomes `C:\…`, a verbatim
/// UNC share (`\\?\UNC\server\share\…`) becomes `\\server\share\…`. Every
/// other path, including the other verbatim forms (a volume GUID), has no
/// plain spelling and is kept as is; it is still a valid path. Off Windows
/// no path carries a prefix, so this is the identity.
fn without_verbatim_prefix(path: PathBuf) -> PathBuf {
    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return path;
    };
    let mut plain = match prefix.kind() {
        Prefix::VerbatimDisk(drive) => OsString::from(format!("{}:", char::from(drive))),
        Prefix::VerbatimUNC(server, share) => {
            let mut unc = OsString::from(r"\\");
            unc.push(server);
            unc.push(r"\");
            unc.push(share);
            unc
        }
        _ => return path,
    };
    plain.push(r"\");
    if components.clone().next() == Some(Component::RootDir) {
        components.next();
    }
    PathBuf::from(plain).join(components.as_path())
}

/// The home directory read through `env`: an absolute `$HOME`, else an
/// absolute `%USERPROFILE%`. `None` when neither resolves.
#[cfg_attr(not(any(feature = "skill", feature = "upgrade")), allow(dead_code))]
pub(crate) fn home_dir_with(env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    absolute_base(env, "HOME").or_else(|| absolute_base(env, "USERPROFILE"))
}

/// The XDG config base read through `env`: an absolute `$XDG_CONFIG_HOME`
/// free of `..` components (a symlinked one resolved to its target), else
/// `<home>/.config`. A relative or traversing `$XDG_CONFIG_HOME` is treated
/// as unset, as the XDG spec requires for relative values and as the config
/// directory's own first tier reads it. `None` when neither resolves.
#[cfg_attr(not(any(feature = "skill", feature = "upgrade")), allow(dead_code))]
pub(crate) fn xdg_config_base_with(env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    absolute_base(env, "XDG_CONFIG_HOME")
        .or_else(|| home_dir_with(env).map(|home| home.join(".config")))
}

/// A base value `is_absolute` accepts on the host running the test:
/// Windows wants a drive prefix, everything else a leading slash. The one
/// host-absolute fixture every base-resolution test (here, `config_dir`,
/// `skill::agents`) builds its values from.
#[cfg(test)]
pub(crate) fn host_absolute(rest: &str) -> String {
    if cfg!(windows) {
        format!("C:\\{rest}")
    } else {
        format!("/{rest}")
    }
}

/// Whether this test runner enforces permission bits: a root or
/// `CAP_DAC_OVERRIDE` runner (CI containers) bypasses them, and a
/// permission-denial test's premise (a `chmod 000` entry cannot be read)
/// does not hold there. Probed through a real 000-mode read attempt under
/// `dir`: a denied read means the bits bind, a successful one means they do
/// not, and any other failure proves neither and fails the test loudly. A
/// test on a runner where the bits do not bind skips with a stderr note
/// instead of failing. The one probe every permission-denial unit test
/// (`telemetry::spool`, `home_dir`) calls; it sits beside
/// [`host_absolute`], the other host fixture, so it compiles exactly when
/// either caller does.
#[cfg(all(test, unix))]
pub(crate) fn permission_bits_enforced(dir: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let probe = dir.join("permission-probe");
    std::fs::write(&probe, b"x").expect("write probe");
    let mut perms = std::fs::metadata(&probe)
        .expect("probe metadata")
        .permissions();
    perms.set_mode(0o000);
    std::fs::set_permissions(&probe, perms).expect("chmod probe");
    let read = std::fs::read(&probe);
    let _ = std::fs::remove_file(&probe);
    match read {
        Ok(_) => false,
        Err(error) if error.kind() == ErrorKind::PermissionDenied => true,
        Err(error) => {
            panic!("permission probe failed for a reason other than a denied read: {error}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::env_of;

    #[test]
    fn home_wins_over_userprofile() {
        let profile = host_absolute("Users/u");
        let home = host_absolute("home/u");
        let pairs = [("HOME", home.as_str()), ("USERPROFILE", profile.as_str())];
        assert_eq!(home_dir_with(&env_of(&pairs)), Some(PathBuf::from(&home)));
    }

    #[test]
    fn userprofile_resolves_when_home_is_unset_or_empty() {
        // Stock PowerShell and cmd on Windows set no HOME.
        let profile = host_absolute("Users/u");
        assert_eq!(
            home_dir_with(&env_of(&[("USERPROFILE", profile.as_str())])),
            Some(PathBuf::from(&profile))
        );
        assert_eq!(
            home_dir_with(&env_of(&[("HOME", ""), ("USERPROFILE", profile.as_str())])),
            Some(PathBuf::from(&profile))
        );
    }

    #[test]
    fn a_relative_or_empty_userprofile_never_resolves() {
        assert_eq!(
            home_dir_with(&env_of(&[("USERPROFILE", "relprofile")])),
            None
        );
        assert_eq!(home_dir_with(&env_of(&[("USERPROFILE", "")])), None);
        assert_eq!(home_dir_with(&env_of(&[])), None);
    }

    #[test]
    fn xdg_config_base_ignores_a_relative_value() {
        let profile = host_absolute("Users/u");
        let pairs = [
            ("XDG_CONFIG_HOME", "relconf"),
            ("USERPROFILE", profile.as_str()),
        ];
        assert_eq!(
            xdg_config_base_with(&env_of(&pairs)),
            Some(PathBuf::from(&profile).join(".config"))
        );
        let absolute = host_absolute("xdg");
        let home = host_absolute("home/u");
        let pairs = [
            ("XDG_CONFIG_HOME", absolute.as_str()),
            ("HOME", home.as_str()),
        ];
        assert_eq!(
            xdg_config_base_with(&env_of(&pairs)),
            Some(PathBuf::from(&absolute))
        );
        assert_eq!(xdg_config_base_with(&env_of(&[])), None);
    }

    #[test]
    fn a_relative_home_never_resolves() {
        // A relative HOME would make every home-relative path depend on the
        // working directory; the USERPROFILE tier decides instead.
        let profile = host_absolute("Users/u");
        let pairs = [("HOME", "relhome"), ("USERPROFILE", profile.as_str())];
        assert_eq!(
            home_dir_with(&env_of(&pairs)),
            Some(PathBuf::from(&profile))
        );
        assert_eq!(home_dir_with(&env_of(&[("HOME", "relhome")])), None);
    }

    #[test]
    fn a_traversal_component_never_names_a_base() {
        // CWE-22: `..` in any base would steer home-relative writes outside
        // the directory the variable appears to name. Each tier refuses it
        // and resolution falls through to the next.
        let profile = host_absolute("Users/u");
        let traversing_home = host_absolute("home/u/../../etc");
        let pairs = [
            ("HOME", traversing_home.as_str()),
            ("USERPROFILE", profile.as_str()),
        ];
        assert_eq!(
            home_dir_with(&env_of(&pairs)),
            Some(PathBuf::from(&profile))
        );

        let traversing_profile = host_absolute("Users/../Windows");
        let pairs = [("USERPROFILE", traversing_profile.as_str())];
        assert_eq!(home_dir_with(&env_of(&pairs)), None);

        let traversing_xdg = host_absolute("xdg/../etc");
        let home = host_absolute("home/u");
        let pairs = [
            ("XDG_CONFIG_HOME", traversing_xdg.as_str()),
            ("HOME", home.as_str()),
        ];
        assert_eq!(
            xdg_config_base_with(&env_of(&pairs)),
            Some(PathBuf::from(&home).join(".config"))
        );
    }

    /// A base that is itself a symlink resolves to its target; a dangling
    /// one names no base, and resolution falls through to the next tier.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_base_resolves_to_its_target() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("real-home");
        std::fs::create_dir(&target).unwrap();
        let link = tmp.path().join("home-link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let canonical = std::fs::canonicalize(&target).unwrap();

        let link_str = link.to_str().unwrap();
        assert_eq!(
            home_dir_with(&env_of(&[("HOME", link_str)])),
            Some(canonical.clone())
        );
        let pairs = [("XDG_CONFIG_HOME", link_str), ("HOME", "/home/elsewhere")];
        assert_eq!(xdg_config_base_with(&env_of(&pairs)), Some(canonical));

        let dangling = tmp.path().join("dangling-link");
        std::os::unix::fs::symlink(tmp.path().join("missing"), &dangling).unwrap();
        let profile = host_absolute("Users/u");
        let pairs = [
            ("HOME", dangling.to_str().unwrap()),
            ("USERPROFILE", profile.as_str()),
        ];
        assert_eq!(
            home_dir_with(&env_of(&pairs)),
            Some(PathBuf::from(&profile))
        );
    }

    /// A base whose kind cannot be read fails closed: it names no base, and
    /// resolution falls through to the next tier.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_base_names_no_base() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        if !permission_bits_enforced(tmp.path()) {
            eprintln!(
                "note: this runner bypasses permission bits (root?); \
                 the unreadable-base rule cannot be tested here"
            );
            return;
        }
        let locked = tmp.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        let base = locked.join("home");
        std::fs::create_dir(&base).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let profile = host_absolute("Users/u");
        let pairs = [
            ("HOME", base.to_str().unwrap()),
            ("USERPROFILE", profile.as_str()),
        ];
        let resolved = home_dir_with(&env_of(&pairs));
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(resolved, Some(PathBuf::from(&profile)));
    }

    /// The verbatim prefixes `canonicalize` answers in on Windows rebuild
    /// to their plain spelling; a verbatim form with no plain spelling, and
    /// a path already plain, are kept as they are.
    #[cfg(windows)]
    #[test]
    fn a_verbatim_prefix_rebuilds_to_its_plain_spelling() {
        for (verbatim, plain) in [
            (r"\\?\C:\Users\u", r"C:\Users\u"),
            (r"\\?\C:\", r"C:\"),
            (r"\\?\UNC\server\share\home\u", r"\\server\share\home\u"),
            (r"\\?\UNC\server\share", r"\\server\share\"),
            (r"\\?\Volume{0000}\u", r"\\?\Volume{0000}\u"),
            (r"C:\Users\u", r"C:\Users\u"),
        ] {
            assert_eq!(
                without_verbatim_prefix(PathBuf::from(verbatim)),
                PathBuf::from(plain),
                "{verbatim}"
            );
        }
    }

    #[test]
    fn a_base_that_does_not_exist_is_kept_as_written() {
        // A config base is created on demand, so a missing one is not an
        // error; only an existing symlink is resolved.
        let missing = host_absolute("nonexistent-relate-home");
        assert_eq!(
            home_dir_with(&env_of(&[("HOME", missing.as_str())])),
            Some(PathBuf::from(&missing))
        );
    }

    #[test]
    fn a_dot_dot_inside_a_name_is_not_traversal() {
        // Only a whole `..` component is traversal; a name that merely
        // contains dots is an ordinary directory.
        let dotted = host_absolute("home/u..v");
        assert_eq!(
            home_dir_with(&env_of(&[("HOME", dotted.as_str())])),
            Some(PathBuf::from(&dotted))
        );
    }

    #[test]
    fn a_driveless_home_is_absolute_only_off_windows() {
        // `/c/Users/u` is absolute on Unix. On Windows it has a root but no
        // drive, so it would resolve against the current drive: it names
        // no base there, and the USERPROFILE tier decides.
        let profile = host_absolute("Users/u");
        let pairs = [("HOME", "/c/Users/u"), ("USERPROFILE", profile.as_str())];
        let expected = if cfg!(windows) {
            PathBuf::from(&profile)
        } else {
            PathBuf::from("/c/Users/u")
        };
        assert_eq!(home_dir_with(&env_of(&pairs)), Some(expected));
    }
}
