//! Skill submodule: agent catalog, path expansion, the shared skill-directory
//! classifier, and the bundle recognizer.

use std::io::{self, Read};
use std::path::Path;

pub mod agents;

/// The post-upgrade skill-refresh subsystem (ADR-0261 / ADR-0262); only the
/// `upgrade` feature calls it.
#[cfg(feature = "upgrade")]
pub mod refresh;

/// The on-disk directory name of the installed skill bundle, used as the
/// frontmatter `name:` and the `<skills dir>/<name>/` install directory.
///
/// Shared by `skill_cmd` (install/remove) and `upgrade_cmd` (refresh
/// classification): it lives here, not in `skill_cmd`, so the `upgrade`
/// feature — which does not pull in `skill_cmd` — can use it.
pub const SKILL_NAME: &str = "relate";

/// The ownership marker written into the installed bundle directory. Its
/// presence is relate's proof that `<skills dir>/relate` was created
/// by this CLI and may be cleaned up or removed. Without it, the directory is
/// treated as user-authored/third-party content and left untouched unless
/// `--force` is passed.
///
/// Shared by `skill_cmd` and `upgrade_cmd` for the same reason as
/// [`SKILL_NAME`].
pub const MARKER_FILE: &str = ".relate-installed";

/// Byte cap for the pre-existing `SKILL.md` that `is_recognizable_bundle`
/// inspects before trusting it (LABS-123). Only the opening frontmatter
/// matters, and the installed bundle is far smaller than this; a file over the
/// cap is treated as foreign so a pre-planted huge `SKILL.md` cannot make a
/// caller allocate it all.
const MAX_SKILL_MD_BYTES: u64 = 64 * 1024;

/// How relate sees an agent's `<skills dir>/relate` destination
/// (ADR-0261 / ADR-0262). The marker and the `name: relate` frontmatter
/// are relate's own signals; anything else at the path is user-authored
/// and untouchable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillDirState {
    /// The destination does not exist (safe to create fresh).
    Missing,
    /// A real directory carrying a regular-file [`MARKER_FILE`].
    Owned,
    /// A real directory with no marker whose `SKILL.md` frontmatter declares
    /// `name: relate` **and** carries a `version:` line — a pre-v0.7.1 or
    /// hand-copied bundle.
    Legacy,
    /// Exists but is neither Owned nor Legacy (including a symlinked
    /// destination or marker, and any uninspectable path); never overwritten
    /// by `upgrade`.
    Foreign,
}

impl SkillDirState {
    /// ADR-0262 install permission: install **adopts** a recognizable but
    /// unmarked relate bundle, so `Missing` (safe to create fresh),
    /// `Owned`, and `Legacy` are installable without `--force`; `Foreign` —
    /// including an uninspectable path — is refused (fail closed: the
    /// directory is user-authored/third-party content relate must not
    /// touch).
    pub fn permits_install(self) -> bool {
        matches!(
            self,
            SkillDirState::Missing | SkillDirState::Owned | SkillDirState::Legacy
        )
    }

    /// ADR-0262 remove permission: remove stays **strict** — only a missing
    /// destination or relate's own marker (`Owned`) permits deleting the
    /// directory. A recognizable but unmarked `Legacy` bundle is refused
    /// alongside `Foreign`, because deleting a directory relate never
    /// marked is irreversible. That single-state difference from
    /// [`SkillDirState::permits_install`] is the ADR-0262
    /// install-adopts/remove-strict asymmetry.
    pub fn permits_remove(self) -> bool {
        matches!(self, SkillDirState::Missing | SkillDirState::Owned)
    }

    /// The one-line user-facing wording for this state (ADR-0242), shared by
    /// `skill list` and `skill check` so the two read-only commands phrase
    /// the classifier identically.
    pub fn label(self) -> &'static str {
        match self {
            SkillDirState::Owned => "installed",
            SkillDirState::Legacy => "installed (legacy bundle)",
            SkillDirState::Missing => "not installed",
            SkillDirState::Foreign => "foreign (not managed by relate)",
        }
    }
}

/// Classify `dest` (the `<skills dir>/relate` directory) per ADR-0261 /
/// ADR-0262. Each path is inspected at most once: one
/// [`std::fs::symlink_metadata`] of `dest` drives existence, the
/// symlink/non-directory rejection and the `is_dir` test; one of
/// `dest/.relate-installed` decides ownership; and the recognizer reads
/// `dest/SKILL.md` assuming `dest` is already known to be a real directory.
///
/// Fail closed: only `NotFound` is [`SkillDirState::Missing`]; any other
/// metadata error is [`SkillDirState::Foreign`], so an uninspectable path is
/// never silently treated as absent or as ours. Symlinks are never followed.
pub fn classify_skill_dir(dest: &Path) -> SkillDirState {
    let meta = match std::fs::symlink_metadata(dest) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return SkillDirState::Missing,
        Err(_) => return SkillDirState::Foreign,
    };
    // A symlink (or any non-directory) at the path is foreign content.
    if !meta.file_type().is_dir() {
        return SkillDirState::Foreign;
    }
    // Owned: a real regular-file marker (a symlinked marker does not count).
    if let Ok(marker) = std::fs::symlink_metadata(dest.join(MARKER_FILE)) {
        if marker.file_type().is_file() {
            return SkillDirState::Owned;
        }
    }
    // Legacy: our own unmarked bundle, recognized by the shared recognizer so
    // this classifier cannot drift from the install-side replace check.
    if is_recognizable_bundle(dest) {
        return SkillDirState::Legacy;
    }
    SkillDirState::Foreign
}

/// True when `dest`'s `SKILL.md` is relate's own **unmarked** bundle: a
/// real regular file (not a symlink) whose YAML frontmatter declares
/// `name: relate` **and** carries a `version:` line (the value may be a
/// concrete version or the literal `{{VERSION}}` placeholder). The installed
/// bundle opens with a `---` fence, the `name:` and `version:` lines, then a
/// closing `---`; LF and CRLF are both accepted.
///
/// The caller must already have confirmed `dest` is a real directory (see
/// [`classify_skill_dir`]); this only reads `dest/SKILL.md`. The file is
/// bounded (LABS-123): metadata over [`MAX_SKILL_MD_BYTES`] fails closed, and
/// the read itself is capped so a growing file cannot exceed it.
fn is_recognizable_bundle(dest: &Path) -> bool {
    let skill_md = dest.join("SKILL.md");
    let meta = match std::fs::symlink_metadata(&skill_md) {
        Ok(meta) => meta,
        Err(_) => return false,
    };
    if !meta.file_type().is_file() {
        return false;
    }
    if meta.len() > MAX_SKILL_MD_BYTES {
        return false;
    }
    let Ok(file) = std::fs::File::open(&skill_md) else {
        return false;
    };
    // Even if the file grew between the metadata check and here, cap the read.
    let mut content = String::new();
    if file
        .take(MAX_SKILL_MD_BYTES)
        .read_to_string(&mut content)
        .is_err()
    {
        return false;
    }
    frontmatter_declares_name_and_version(&content)
}

/// Pure frontmatter scan: the first line is a `---` fence, and the block up to
/// the next `---` contains both a `name:` entry equal to [`SKILL_NAME`] and a
/// `version:` line. Lines are trimmed of surrounding whitespace (and `\r`), so
/// CRLF files parse. The version value is not inspected: a concrete version
/// and the literal `{{VERSION}}` placeholder both count. An unterminated
/// frontmatter block (no closing `---` before EOF) fails closed: the file is
/// not one of ours (LABS-123).
fn frontmatter_declares_name_and_version(content: &str) -> bool {
    let mut lines = content.lines();
    match lines.next() {
        Some(first) if first.trim() == "---" => {}
        _ => return false,
    }
    let mut declares_name = false;
    let mut declares_version = false;
    let mut fence_closed = false;
    for line in lines {
        let line = line.trim();
        if line == "---" {
            fence_closed = true;
            break;
        }
        if let Some(rest) = line.strip_prefix("name:") {
            if rest.trim() == SKILL_NAME {
                declares_name = true;
            }
        }
        if line.strip_prefix("version:").is_some() {
            declares_version = true;
        }
    }
    fence_closed && declares_name && declares_version
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_bundle(dest: &Path, skill_md: &str) {
        std::fs::create_dir_all(dest).unwrap();
        std::fs::write(dest.join("SKILL.md"), skill_md).unwrap();
    }

    #[test]
    fn recognizes_name_and_version_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join(SKILL_NAME);
        write_bundle(
            &dest,
            "---\nname: relate\nversion: 0.7.0\ndescription: x\n---\n\n# Relate\n",
        );
        assert!(is_recognizable_bundle(&dest));
    }

    #[test]
    fn recognizes_version_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join(SKILL_NAME);
        write_bundle(
            &dest,
            "---\nname: relate\nversion: {{VERSION}}\n---\nbody\n",
        );
        assert!(is_recognizable_bundle(&dest));
    }

    #[test]
    fn recognizes_crlf_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join(SKILL_NAME);
        write_bundle(
            &dest,
            "---\r\nname: relate\r\nversion: 0.7.0\r\n---\r\nbody\r\n",
        );
        assert!(is_recognizable_bundle(&dest));
    }

    #[test]
    fn rejects_name_without_version() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join(SKILL_NAME);
        write_bundle(
            &dest,
            "---\nname: relate\ndescription: x\n---\n\n# Relate\n",
        );
        assert!(!is_recognizable_bundle(&dest));
    }

    #[test]
    fn rejects_version_without_name() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join(SKILL_NAME);
        write_bundle(&dest, "---\nversion: 0.7.0\n---\n");
        assert!(!is_recognizable_bundle(&dest));
    }

    #[test]
    fn rejects_other_name() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join(SKILL_NAME);
        write_bundle(&dest, "---\nname: someone-else\nversion: 0.7.0\n---\n");
        assert!(!is_recognizable_bundle(&dest));
    }

    #[test]
    fn rejects_oversized_skill_md() {
        // LABS-123: a valid-looking frontmatter with a body past the byte cap
        // is not read whole; the dir fails closed.
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join(SKILL_NAME);
        let mut body = String::from("---\nname: relate\nversion: 0.7.0\n---\n");
        body.push_str(&"x".repeat(MAX_SKILL_MD_BYTES as usize + 1));
        assert!(body.len() as u64 > MAX_SKILL_MD_BYTES);
        write_bundle(&dest, &body);
        assert!(!is_recognizable_bundle(&dest));
    }

    #[test]
    fn rejects_unterminated_frontmatter() {
        // LABS-123: name and version present, but no closing `---` fence.
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join(SKILL_NAME);
        write_bundle(&dest, "---\nname: relate\nversion: 0.7.0\ndescription: x\n");
        assert!(!is_recognizable_bundle(&dest));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_skill_md() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join(SKILL_NAME);
        std::fs::create_dir_all(&dest).unwrap();
        let target = dir.path().join("skill-target.md");
        std::fs::write(&target, "---\nname: relate\nversion: 0.7.0\n---\n").unwrap();
        std::os::unix::fs::symlink(&target, dest.join("SKILL.md")).unwrap();
        assert!(!is_recognizable_bundle(&dest));
    }

    #[test]
    fn state_policy_is_the_adr_0262_matrix() {
        // The whole state-to-permission mapping, in one place: install
        // adopts (Missing|Owned|Legacy), remove stays strict (Missing|Owned).
        // The refresh's role mapping (installed = Owned|Legacy auto-update,
        // Missing consent, Foreign report-only) is pinned by
        // `skill_refresh_decision`'s tests in refresh.rs.
        for (state, install, remove) in [
            (SkillDirState::Missing, true, true),
            (SkillDirState::Owned, true, true),
            (SkillDirState::Legacy, true, false),
            (SkillDirState::Foreign, false, false),
        ] {
            assert_eq!(state.permits_install(), install, "install: {state:?}");
            assert_eq!(state.permits_remove(), remove, "remove: {state:?}");
        }
    }

    #[test]
    fn classifies_missing_destination() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            classify_skill_dir(&dir.path().join("nope")),
            SkillDirState::Missing
        );
    }

    #[test]
    fn classifies_marker_owned() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join(SKILL_NAME);
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(dest.join(MARKER_FILE), b"").unwrap();
        assert_eq!(classify_skill_dir(&dest), SkillDirState::Owned);
    }

    #[test]
    fn classifies_recognizable_bundle_as_legacy() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join(SKILL_NAME);
        write_bundle(&dest, "---\nname: relate\nversion: 0.7.0\n---\nbody\n");
        assert_eq!(classify_skill_dir(&dest), SkillDirState::Legacy);
    }

    #[test]
    fn classifies_unrelated_directory_as_foreign() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join(SKILL_NAME);
        std::fs::create_dir(&dest).unwrap();
        assert_eq!(classify_skill_dir(&dest), SkillDirState::Foreign);
        // A SKILL.md that declares a different name is not ours either.
        std::fs::write(
            dest.join("SKILL.md"),
            "---\nname: someone-else\nversion: 0.7.0\n---\n",
        )
        .unwrap();
        assert_eq!(classify_skill_dir(&dest), SkillDirState::Foreign);
    }

    #[cfg(unix)]
    #[test]
    fn classifies_uninspectable_path_as_foreign() {
        // A metadata error other than NotFound (here ENOTDIR: a path below a
        // regular file) must fail closed as Foreign, never Missing. The
        // ENOTDIR simulation is Unix-only: Windows returns NotFound for a
        // path below a regular file.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-dir");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(
            classify_skill_dir(&file.join(SKILL_NAME)),
            SkillDirState::Foreign
        );
    }

    #[cfg(unix)]
    #[test]
    fn classifies_symlinked_destination_or_marker_as_foreign() {
        let dir = tempfile::tempdir().unwrap();
        // A symlinked destination is Foreign even when the target is Owned.
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join(MARKER_FILE), b"").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(classify_skill_dir(&link), SkillDirState::Foreign);

        // A symlinked marker inside a real dir is Foreign, not Owned.
        let dest = dir.path().join(SKILL_NAME);
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(real.join("marker-target"), b"").unwrap();
        std::os::unix::fs::symlink(real.join("marker-target"), dest.join(MARKER_FILE)).unwrap();
        assert_eq!(classify_skill_dir(&dest), SkillDirState::Foreign);
    }
}
