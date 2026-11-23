//! The package-managed marker: a package manager that ships relate writes
//! `.relate-package-managed` beside the real binary — JSON
//! `{"manager": "<id>", "upgrade": "<command>"}`, both required non-empty
//! strings — and exposes the binary on `PATH` by symlink. `relate upgrade`
//! never replaces such an install; it names the package's own upgrade command
//! instead. The writer lives outside this repository, so an unknown field is
//! ignored and only the two required ones are read.
//!
//! Presence is detected, content carries the advice (PEP 668's
//! `EXTERNALLY-MANAGED` shape). Only `NotFound` is an unmanaged install; a
//! marker that is a regular file, a symlink, a directory, or uninspectable is
//! package-managed — the same `NotFound`-vs-`Foreign` split
//! [`crate::skill::classify_skill_dir`] uses. Symlinks are never followed and
//! only a regular file's content is read. It is a convention, not a security
//! control: whoever can write the binary's directory can replace it anyway.

use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::UpgradeError;

/// The marker's file name, in the directory of the real binary — the
/// canonicalized `current_exe()`, never the `PATH` symlink.
const MARKER_FILE: &str = ".relate-package-managed";

/// A package-managed install: the marker's declaration, or a marker that is
/// present but unusable. Both refuse the self-replace — a malformed marker
/// must never read as unmanaged.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum PackageManaged {
    Declared { manager: String, upgrade: String },
    Malformed { path: PathBuf, cause: String },
}

impl PackageManaged {
    /// The one refusal sentence, shared by the plain upgrade abort and the
    /// `--check` advice, so the two sites cannot word it differently.
    pub(super) fn refusal(&self) -> String {
        match self {
            PackageManaged::Declared { manager, upgrade } => {
                format!("relate was installed by {manager}; upgrade it with: {upgrade}")
            }
            PackageManaged::Malformed { path, cause } => format!(
                "relate is package-managed; marker {} is unreadable/invalid: {cause}",
                path.display()
            ),
        }
    }
}

/// The marker's parsed shape: both fields are required, and serde's default
/// unknown-field handling ignores a package's extra keys.
#[derive(Deserialize)]
struct Marker {
    manager: String,
    upgrade: String,
}

/// The running binary's package provenance, or `None` for an unmanaged
/// install (the curl installer, a manual copy, or a package's binary copied
/// without its sibling marker).
pub(super) fn detect() -> Result<Option<PackageManaged>, UpgradeError> {
    let exe = std::env::current_exe()
        // resolve: the running binary's path is unavailable.
        .map_err(|e| UpgradeError::new(format!("resolving current exe: {e}")))?;
    // macOS's `current_exe()` returns the path used to exec — the `PATH`
    // symlink — not the real binary, and the marker sits beside the real one
    // (Homebrew's keg). Linux's `/proc/self/exe` already resolves, so this is
    // a no-op there.
    let real = std::fs::canonicalize(&exe)
        // resolve: the running binary's real path is unavailable.
        .map_err(|e| UpgradeError::new(format!("resolving {}: {e}", exe.display())))?;
    match real.parent() {
        Some(dir) => Ok(provenance_at(dir)),
        // resolve: a canonical path always has a parent; this is the root
        // directory itself, never a binary.
        None => Err(UpgradeError::new(format!(
            "resolving {}: no parent directory",
            real.display()
        ))),
    }
}

/// [`PackageManaged`] for the binary directory `dir`, or `None` when `dir`
/// holds no marker. Pure over `dir`, so the whole classifier is unit-tested
/// against temp directories.
fn provenance_at(dir: &Path) -> Option<PackageManaged> {
    let path = dir.join(MARKER_FILE);
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
        Err(e) => return Some(malformed(path, e)),
    };
    // A symlinked, directory or otherwise non-regular marker is present but
    // unusable: package-managed, never followed.
    if !meta.file_type().is_file() {
        return Some(malformed(path, "not a regular file"));
    }
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) => return Some(malformed(path, e)),
    };
    match serde_json::from_slice::<Marker>(&bytes) {
        Ok(marker) if !marker.manager.is_empty() && !marker.upgrade.is_empty() => {
            Some(PackageManaged::Declared {
                manager: marker.manager,
                upgrade: marker.upgrade,
            })
        }
        Ok(_) => Some(malformed(
            path,
            "manager and upgrade must be non-empty strings",
        )),
        Err(e) => Some(malformed(path, e)),
    }
}

fn malformed(path: PathBuf, cause: impl std::fmt::Display) -> PackageManaged {
    PackageManaged::Malformed {
        path,
        cause: cause.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_marker(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join(MARKER_FILE);
        fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn an_absent_marker_is_an_unmanaged_install() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(provenance_at(dir.path()), None);
    }

    #[test]
    fn a_declared_marker_refuses_with_the_packages_own_command() {
        let dir = tempfile::tempdir().unwrap();
        // Unknown fields are a package's own business and are ignored.
        write_marker(
            dir.path(),
            r#"{"manager": "homebrew", "upgrade": "brew upgrade relate", "channel": "stable"}"#,
        );
        let managed = provenance_at(dir.path()).expect("a marker is package-managed");
        assert_eq!(
            managed,
            PackageManaged::Declared {
                manager: "homebrew".to_string(),
                upgrade: "brew upgrade relate".to_string(),
            }
        );
        assert_eq!(
            managed.refusal(),
            "relate was installed by homebrew; upgrade it with: brew upgrade relate"
        );
    }

    #[test]
    fn a_malformed_marker_still_reads_as_package_managed() {
        let dir = tempfile::tempdir().unwrap();
        for body in [
            "not json",
            r#"{"manager": "homebrew"}"#,
            r#"{"manager": "", "upgrade": "brew upgrade relate"}"#,
            r#"{"manager": "homebrew", "upgrade": 7}"#,
        ] {
            let path = write_marker(dir.path(), body);
            match provenance_at(dir.path()) {
                Some(PackageManaged::Malformed { path: found, .. }) => {
                    assert_eq!(found, path, "body {body:?}")
                }
                other => panic!("body {body:?}: expected Malformed, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_directory_marker_still_reads_as_package_managed() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(MARKER_FILE)).unwrap();
        assert!(matches!(
            provenance_at(dir.path()),
            Some(PackageManaged::Malformed { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_marker_is_not_followed() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("elsewhere.json");
        fs::write(
            &target,
            r#"{"manager": "homebrew", "upgrade": "brew upgrade relate"}"#,
        )
        .unwrap();
        symlink(&target, dir.path().join(MARKER_FILE)).unwrap();
        match provenance_at(dir.path()) {
            Some(PackageManaged::Malformed { cause, .. }) => {
                assert_eq!(cause, "not a regular file")
            }
            other => panic!("expected Malformed, got {other:?}"),
        }
    }
}
