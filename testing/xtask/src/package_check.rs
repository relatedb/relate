//! `cargo xtask check-package` — prove the `relate` facade's packaged
//! crate builds against the *current* workspace, inside the package
//! drift window (the gap between a change landing in a workspace crate
//! and that crate's republish to crates.io).
//!
//! `cargo package` verifies the packaged crate against the crates.io
//! registry, so it fails throughout the window: the facade calls symbols
//! the published dependency versions do not carry yet. This check runs
//! the same verification against the workspace instead:
//!
//! 1. `cargo package --no-verify` the facade into a scratch directory
//!    under the system temp dir (nothing is written in the repository);
//! 2. extract the `.crate` tarball;
//! 3. append a `[patch.crates-io]` table to the extracted manifest that
//!    redirects every member of the facade's transitive workspace closure
//!    to its workspace path;
//! 4. `cargo build --lib --examples` the extracted crate under the patch.
//!
//! The closure follows normal and build dependency edges only — exactly
//! the edges packaging rewrites into registry version requirements. Dev
//! edges are excluded: path-only dev-deps are dropped by packaging
//! (`relate-test-support`, see the workspace manifest), and patching a
//! crate the build never references makes cargo warn on every run. For
//! the same reason the extracted tests are not built: they need the
//! dev-deps packaging drops, by design.
//!
//! The scratch directory is a `tempfile::TempDir`: removed on every
//! exit path — success, failure and panic alike.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Output};

use crate::workspace::{run_cargo, workspace_graph, DepKind, Package};

/// The facade crate the check packages (`relate::Database`).
const FACADE: &str = "relate";
/// The facade's manifest, relative to the workspace root.
const FACADE_MANIFEST: &str = "relate/relate/Cargo.toml";

/// What one `check-package` run concluded; printed as the summary line.
pub(crate) struct Summary {
    /// The packaged facade's version, from the extracted package
    /// directory name.
    version: String,
    /// Workspace members the patch manifest redirected.
    patched: usize,
    /// Examples built alongside the lib.
    examples: usize,
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "check-package: relate {} packaged, {} workspace crates patched, lib and {} examples built against the workspace",
            self.version, self.patched, self.examples
        )
    }
}

/// How a `check-package` run failed. The class is the exit contract:
/// `Tooling` — the check could not run — is the environment class
/// `check-boundaries` also exits 2 for; `Build` — the check ran and the
/// packaged crate did not build — is the violation class, exit 1.
pub(crate) enum Failure {
    Tooling(String),
    Build(String),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Failure::Tooling(message) | Failure::Build(message) => message,
        })
    }
}

impl Failure {
    /// The exit status for this failure class: 2 for a check that could
    /// not run, 1 for a check that ran and failed.
    pub(crate) fn exit_code(&self) -> ExitCode {
        match self {
            Failure::Tooling(_) => ExitCode::from(2),
            Failure::Build(_) => ExitCode::FAILURE,
        }
    }
}

/// Run the check: package, extract, patch, build. A failure before the
/// pipeline can start — resolving the workspace root, the workspace
/// graph, the facade's closure, the scratch directory — is
/// `Failure::Tooling` (exit 2); every pipeline failure is
/// `Failure::Build` (exit 1). The scratch `TempDir` is removed on every
/// exit path, panic included.
pub(crate) fn check(root: &Path) -> Result<Summary, Failure> {
    let root = root.canonicalize().map_err(|e| {
        Failure::Tooling(format!(
            "failed to resolve the workspace root {}: {e}",
            root.display()
        ))
    })?;
    let packages = workspace_graph().map_err(Failure::Tooling)?;
    let closure = workspace_closure(&packages).map_err(Failure::Tooling)?;
    let scratch = scratch_dir().map_err(Failure::Tooling)?;
    package_and_build(&root, &closure, scratch.path()).map_err(Failure::Build)
}

/// Package the facade, extract and patch the tarball, build lib and
/// examples under the patch. Every step fails loud: a missing or
/// ambiguous artifact is an error, never a guess.
fn package_and_build(root: &Path, closure: &[&Package], scratch: &Path) -> Result<Summary, String> {
    // --no-verify: registry verification is exactly what cannot pass
    // inside the drift window; the patched build below replaces it.
    let target_dir = scratch.join("target");
    let facade_manifest = root.join(FACADE_MANIFEST).to_string_lossy().into_owned();
    let package_target = target_dir.to_string_lossy().into_owned();
    let package = run_cargo(&[
        "package",
        "--manifest-path",
        facade_manifest.as_str(),
        "--no-verify",
        "--target-dir",
        package_target.as_str(),
    ])?;
    expect_success(package, "`cargo package --no-verify` (relate facade)")?;

    let tarball = crate_tarball(&target_dir.join("package"))?;
    let pkg_root = scratch.join("pkg");
    fs::create_dir(&pkg_root)
        .map_err(|e| format!("failed to create {}: {e}", pkg_root.display()))?;
    extract_tarball(&tarball, &pkg_root)?;
    let (pkg, version) = extracted_package(&pkg_root)?;

    let manifest = pkg.join("Cargo.toml");
    append_patch_manifest(&manifest, &patch_manifest(root, closure))?;

    // Lib and examples only: the packaged tests need the dev-deps
    // packaging drops (see the module docs), so building them is not
    // part of the check.
    let build_manifest = manifest.to_string_lossy().into_owned();
    let build_target = pkg.join("target").to_string_lossy().into_owned();
    let build = run_cargo(&[
        "build",
        "--manifest-path",
        build_manifest.as_str(),
        "--lib",
        "--examples",
        "--target-dir",
        build_target.as_str(),
    ])?;
    expect_success(
        build,
        "`cargo build --lib --examples` (extracted relate package)",
    )?;

    Ok(Summary {
        version,
        patched: closure.len(),
        examples: count_examples(&pkg)?,
    })
}

/// The facade's transitive workspace closure: every workspace member
/// reachable from `relate` over normal and build dependency edges,
/// sorted by name, the facade itself excluded (it is the package being
/// built, not a patch target). The traversal's unit is the package: the
/// queue and the visited set carry `&Package`, so workspace membership
/// is proven by construction — nothing re-resolves a directory string.
fn workspace_closure(packages: &[Package]) -> Result<Vec<&Package>, String> {
    let by_dir: BTreeMap<&str, &Package> = packages.iter().map(|p| (p.dir.as_str(), p)).collect();
    let facade = packages
        .iter()
        .find(|p| p.name == FACADE)
        .ok_or_else(|| format!("cargo metadata: no workspace member named '{FACADE}'"))?;
    // Visited packages keyed by directory: insertion is the visit, so
    // the closure falls out of the values with no second lookup.
    let mut seen: BTreeMap<&str, &Package> = BTreeMap::from([(facade.dir.as_str(), facade)]);
    let mut queue: Vec<&Package> = vec![facade];
    while let Some(package) = queue.pop() {
        for dep in &package.deps {
            if !matches!(dep.kind, DepKind::Normal | DepKind::Build) {
                continue;
            }
            let Some(path) = dep.path.as_deref() else {
                continue; // a registry dependency: no workspace path to follow
            };
            let Some(&dep_pkg) = by_dir.get(path) else {
                continue; // a path dependency outside the workspace
            };
            if seen.insert(dep_pkg.dir.as_str(), dep_pkg).is_none() {
                queue.push(dep_pkg);
            }
        }
    }
    let mut closure: Vec<&Package> = seen
        .values()
        .copied()
        .filter(|p| p.dir != facade.dir)
        .collect();
    closure.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(closure)
}

/// Render the `[patch.crates-io]` table mapping every closure member to
/// its workspace path. Paths are absolute — the scratch directory the
/// manifest is read from moves with every run.
fn patch_manifest(root: &Path, closure: &[&Package]) -> String {
    let mut out = String::from("\n[patch.crates-io]\n");
    for package in closure {
        out.push_str(&format!(
            "{} = {{ path = {:?} }}\n",
            package.name,
            root.join(&package.dir)
        ));
    }
    out
}

/// Append the patch table to the extracted manifest. The extracted
/// package is the root of its own build, so its manifest is where
/// patches belong; a manifest that already carries a `[patch]` table is
/// refused rather than corrupted by a second one.
fn append_patch_manifest(manifest: &Path, patch: &str) -> Result<(), String> {
    let current = fs::read_to_string(manifest)
        .map_err(|e| format!("failed to read {}: {e}", manifest.display()))?;
    if current.contains("[patch") {
        return Err(format!(
            "{} already carries a [patch] table",
            manifest.display()
        ));
    }
    let patched = current + patch;
    fs::write(manifest, &patched)
        .map_err(|e| format!("failed to write {}: {e}", manifest.display()))
}

/// The single `.crate` file `cargo package` wrote into its package
/// directory. Zero or several is an error — never a guess.
fn crate_tarball(package_dir: &Path) -> Result<PathBuf, String> {
    let entries: Result<Vec<_>, _> = fs::read_dir(package_dir)
        .map_err(|e| {
            format!(
                "`cargo package` wrote no package directory {}: {e}",
                package_dir.display()
            )
        })?
        .collect();
    let tarballs: Vec<PathBuf> = entries
        .map_err(|e| format!("failed to read {}: {e}", package_dir.display()))?
        .into_iter()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "crate"))
        .collect();
    let [tarball] = tarballs.as_slice() else {
        return Err(format!(
            "expected exactly one .crate under {}, found {}",
            package_dir.display(),
            tarballs.len()
        ));
    };
    Ok(tarball.to_path_buf())
}

/// Extract the `.crate` tarball (a gzipped tar) into `dest` with
/// `tar -xf`, which auto-detects the compression on every platform the
/// check runs on.
fn extract_tarball(tarball: &Path, dest: &Path) -> Result<(), String> {
    let output = Command::new("tar")
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
        })?;
    expect_success(output, &format!("`tar -xf {}`", tarball.display()))
}

/// The extracted `.crate` layout: the one `<name>-<version>` package
/// directory, with the facade's version split off its name.
fn extracted_package(pkg_root: &Path) -> Result<(PathBuf, String), String> {
    let entries: Result<Vec<_>, _> = fs::read_dir(pkg_root)
        .map_err(|e| format!("failed to read {}: {e}", pkg_root.display()))?
        .collect();
    let dirs: Vec<PathBuf> = entries
        .map_err(|e| format!("failed to read {}: {e}", pkg_root.display()))?
        .into_iter()
        .map(|entry| entry.path())
        .collect();
    let [dir] = dirs.as_slice() else {
        return Err(format!(
            "expected exactly one package directory under {}, found {}",
            pkg_root.display(),
            dirs.len()
        ));
    };
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
    Ok((dir.to_path_buf(), version.to_string()))
}

/// How many example targets the extracted package carries — the
/// `examples/*.rs` files cargo discovered when it packaged the facade.
fn count_examples(pkg: &Path) -> Result<usize, String> {
    let examples = pkg.join("examples");
    let entries: Result<Vec<_>, _> = match fs::read_dir(&examples) {
        Ok(iter) => iter.collect(),
        // A facade without examples is a fact, not a failure: nothing
        // was packaged and nothing was built.
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(format!("failed to read {}: {e}", examples.display())),
    };
    let entries = entries.map_err(|e| format!("failed to read {}: {e}", examples.display()))?;
    Ok(entries
        .iter()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "rs"))
        .count())
}

/// Turn a non-zero child exit into a clear error, keeping its stderr.
fn expect_success(output: Output, command: &str) -> Result<(), String> {
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{command} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// A fresh scratch directory under the system temp dir, removed on
/// every exit path — success, failure and panic alike — by `TempDir`'s
/// Drop. Everything the check produces dies with it.
fn scratch_dir() -> Result<tempfile::TempDir, String> {
    tempfile::TempDir::new()
        .map_err(|e| format!("failed to create a scratch directory under the system temp dir: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Dep;

    /// A workspace member fixture: `deps` are `(workspace dir, kind)`
    /// pairs, `None` marking a registry dependency with no path.
    fn pkg(name: &str, dir: &str, deps: &[(Option<&str>, DepKind)]) -> Package {
        Package {
            name: name.to_string(),
            dir: dir.to_string(),
            deps: deps
                .iter()
                .map(|(path, kind)| Dep {
                    kind: *kind,
                    path: path.map(str::to_string),
                })
                .collect(),
        }
    }

    /// The facade with normal, build and dev edges, a transitive chain,
    /// a normal cycle back into the facade, and members no patch may
    /// ever reach: the dev-only test support (whose own normal edge
    /// cycles back), a dev-only surface, and an unrelated surface.
    fn fixture_workspace() -> Vec<Package> {
        vec![
            pkg(
                "relate",
                "relate/relate",
                &[
                    (Some("relate/relate-core"), DepKind::Normal),
                    (Some("relate/relate-model"), DepKind::Build),
                    (Some("testing/relate-test-support"), DepKind::Dev),
                    (None, DepKind::Normal), // a registry dependency
                ],
            ),
            pkg(
                "relate-core",
                "relate/relate-core",
                &[(Some("relate/relate-executor"), DepKind::Normal)],
            ),
            pkg(
                "relate-executor",
                "relate/relate-executor",
                &[
                    (Some("relate/relate-markdown"), DepKind::Normal),
                    (Some("surfaces/relate-formatter"), DepKind::Dev),
                ],
            ),
            pkg("relate-markdown", "relate/relate-markdown", &[]),
            pkg("relate-model", "relate/relate-model", &[]),
            pkg(
                "relate-test-support",
                "testing/relate-test-support",
                &[(Some("relate/relate"), DepKind::Normal)],
            ),
            pkg("relate-formatter", "surfaces/relate-formatter", &[]),
            pkg("relate-cli", "surfaces/relate-cli", &[]),
        ]
    }

    #[test]
    fn closure_follows_normal_and_build_edges_from_the_facade() {
        let packages = fixture_workspace();
        let closure = workspace_closure(&packages).unwrap();
        let names: Vec<&str> = closure.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "relate-core",
                "relate-executor",
                "relate-markdown",
                "relate-model"
            ]
        );
    }

    #[test]
    fn patch_manifest_maps_every_closure_member_to_its_workspace_path() {
        let root = Path::new("/ws");
        let packages = fixture_workspace();
        let closure = workspace_closure(&packages).unwrap();
        let manifest = patch_manifest(root, &closure);
        for (name, dir) in [
            ("relate-core", "relate/relate-core"),
            ("relate-executor", "relate/relate-executor"),
            ("relate-markdown", "relate/relate-markdown"),
            ("relate-model", "relate/relate-model"),
        ] {
            let line = format!("{name} = {{ path = {:?} }}\n", root.join(dir));
            assert!(manifest.contains(&line), "missing {line}in:\n{manifest}");
        }
        // The header plus one line per closure member, nothing else.
        assert_eq!(manifest.lines().count(), closure.len() + 2);
        for outside in ["relate-test-support", "relate-formatter", "relate-cli"] {
            assert!(!manifest.contains(outside), "{outside} must not be patched");
        }
        assert!(
            !manifest.contains("relate = {"),
            "the facade itself must not be patched"
        );
    }

    #[test]
    fn closure_without_the_facade_member_is_an_error() {
        let packages: Vec<Package> = fixture_workspace()
            .into_iter()
            .filter(|p| p.name != "relate")
            .collect();
        let error = workspace_closure(&packages).unwrap_err();
        assert_eq!(error, "cargo metadata: no workspace member named 'relate'");
    }

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
        extract_tarball(&tarball, &pkg_root).unwrap();
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
        let error = extract_tarball(&bogus, &pkg_root).unwrap_err();
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
        fs::write(package_dir.join("relate-0.9.0.crate"), "").unwrap();
        let two = crate_tarball(&package_dir).unwrap_err();
        assert!(two.contains("found 2"), "{two}");
    }
}
