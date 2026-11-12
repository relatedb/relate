//! Packaging and extraction: the `.crate` `cargo package` wrote, the
//! one directory it extracts to, and the singleton guards every reader
//! in this check shares.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::FACADE;

/// One directory listing into paths, per-entry errors surfaced — the
/// shape every reader in this check shares.
pub(super) fn read_dir_paths(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let entries: Result<Vec<_>, _> = fs::read_dir(dir)
        .map_err(|e| format!("failed to read {}: {e}", dir.display()))?
        .collect();
    entries
        .map_err(|e| format!("failed to read {}: {e}", dir.display()))
        .map(|entries| entries.into_iter().map(|entry| entry.path()).collect())
}

/// The one singleton guard every reader here shares: zero or several
/// entries is an error naming what was expected and where — never a
/// guess.
pub(super) fn one_entry(dir: &Path, what: &str, entries: Vec<PathBuf>) -> Result<PathBuf, String> {
    let [entry] = entries.as_slice() else {
        return Err(format!(
            "expected exactly one {what} under {}, found {}",
            dir.display(),
            entries.len()
        ));
    };
    Ok(entry.to_path_buf())
}

/// The single `.crate` file `cargo package` wrote into its package
/// directory. Zero or several is an error — never a guess.
pub(super) fn crate_tarball(package_dir: &Path) -> Result<PathBuf, String> {
    let tarballs: Vec<PathBuf> = read_dir_paths(package_dir)?
        .into_iter()
        .filter(|path| path.extension().is_some_and(|ext| ext == "crate"))
        .collect();
    one_entry(package_dir, ".crate file", tarballs)
}

/// Extract the `.crate` tarball (a gzipped tar) into `dest` with
/// `tar -xf`, which auto-detects the compression on every platform the
/// check runs on. Returns the child's output: the spawn failure (no
/// `tar` on PATH) is the caller's could-not-run class, the exit status
/// its finding class — the same split `run_cargo` makes.
pub(super) fn extract_tarball(tarball: &Path, dest: &Path) -> Result<std::process::Output, String> {
    Command::new("tar")
        .arg("-xf")
        .arg(tarball)
        .arg("-C")
        .arg(dest)
        .output()
        .map_err(|e| {
            format!(
                "failed to run `tar -xf {} -C {}`: {e}",
                tarball.display(),
                dest.display()
            )
        })
}

/// The extracted `.crate` layout: the one `<name>-<version>` package
/// directory, with the facade's version split off its name.
pub(super) fn extracted_package(pkg_root: &Path) -> Result<(PathBuf, String), String> {
    let dir = one_entry(pkg_root, "package directory", read_dir_paths(pkg_root)?)?;
    if !dir.is_dir() {
        return Err(format!(
            "the extracted entry {} is not a directory",
            dir.display()
        ));
    }
    let name = dir
        .file_name()
        .expect("a read_dir entry always carries a file name")
        .to_string_lossy()
        .into_owned();
    let Some(version) = name.strip_prefix(&format!("{FACADE}-")) else {
        return Err(format!(
            "expected an extracted directory named '{FACADE}-<version>', found {name}"
        ));
    };
    Ok((dir, version.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_check::scratch_dir;
    use crate::workspace::expect_success;

    #[test]
    fn extracts_the_tarball_and_finds_the_package_directory() {
        let scratch = scratch_dir().unwrap();
        let fixture = scratch.path().join("fixture").join("relate-9.9.9");
        fs::create_dir_all(fixture.join("src")).unwrap();
        fs::write(
            fixture.join("Cargo.toml"),
            "[package]\nname = \"relate\"\nversion = \"9.9.9\"\n",
        )
        .unwrap();
        fs::write(
            fixture.join("src").join("lib.rs"),
            "pub fn answer() -> i32 { 42 }\n",
        )
        .unwrap();
        let tarball = scratch.path().join("relate-9.9.9.crate");
        let tar = Command::new("tar")
            .arg("-czf")
            .arg(&tarball)
            .arg("-C")
            .arg(scratch.path().join("fixture"))
            .arg("relate-9.9.9")
            .output()
            .unwrap();
        assert!(
            tar.status.success(),
            "tar -czf fixture failed: {}",
            String::from_utf8_lossy(&tar.stderr)
        );
        let pkg_root = scratch.path().join("pkg");
        fs::create_dir(&pkg_root).unwrap();
        expect_success(&extract_tarball(&tarball, &pkg_root).unwrap(), "`tar -xf`").unwrap();
        let (pkg, version) = extracted_package(&pkg_root).unwrap();
        assert_eq!(version, "9.9.9");
        assert_eq!(
            fs::read_to_string(pkg.join("src").join("lib.rs")).unwrap(),
            "pub fn answer() -> i32 { 42 }\n"
        );
    }

    #[test]
    fn extracting_a_non_tarball_is_a_clear_error() {
        let scratch = scratch_dir().unwrap();
        let bogus = scratch.path().join("not-a-crate.crate");
        fs::write(&bogus, "definitely not a tarball").unwrap();
        let pkg_root = scratch.path().join("pkg");
        fs::create_dir(&pkg_root).unwrap();
        // The spawn succeeds (tar exists); the exit status is the error.
        let output = extract_tarball(&bogus, &pkg_root).unwrap();
        let error = expect_success(&output, "`tar -xf`").unwrap_err();
        assert!(
            error.starts_with("`tar -xf") && error.contains("failed"),
            "{error}"
        );
    }

    #[test]
    fn the_single_crate_tarball_is_found_and_ambiguity_refused() {
        let scratch = scratch_dir().unwrap();
        let package_dir = scratch.path().join("package");
        fs::create_dir(&package_dir).unwrap();
        let none = crate_tarball(&package_dir).unwrap_err();
        assert!(none.contains("found 0"), "{none}");
        fs::write(package_dir.join("relate-1.0.0.crate"), "").unwrap();
        assert_eq!(
            crate_tarball(&package_dir).unwrap(),
            package_dir.join("relate-1.0.0.crate")
        );
        fs::write(package_dir.join("relate-1.0.1.crate"), "").unwrap();
        let ambiguity = crate_tarball(&package_dir).unwrap_err();
        assert!(ambiguity.contains("found 2"), "{ambiguity}");
    }

    #[test]
    fn one_entry_refuses_zero_and_several() {
        let scratch = scratch_dir().unwrap();
        let dir = scratch.path();
        let none = one_entry(dir, "thing", Vec::new()).unwrap_err();
        assert!(none.contains("found 0"), "{none}");
        let several = one_entry(dir, "thing", vec![dir.join("a"), dir.join("b")]).unwrap_err();
        assert!(several.contains("found 2"), "{several}");
        fs::write(dir.join("only"), "").unwrap();
        assert_eq!(
            one_entry(dir, "thing", read_dir_paths(dir).unwrap()).unwrap(),
            dir.join("only")
        );
    }
}
