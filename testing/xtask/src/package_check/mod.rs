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
//!    redirects every workspace member to its workspace path, then
//!    narrow the table to the closure the patched package resolves for
//!    the host platform: optional dependencies that stay inactive and
//!    other platforms' target dependencies are not patched, because the
//!    build below does not compile them;
//! 4. `cargo build --lib --examples --message-format=json` the extracted
//!    crate under the patch, and read the verdict from the typed build
//!    records: every resolved closure member — and the extracted facade
//!    itself — must have produced a compiler artifact whose manifest is
//!    the workspace one. A `[patch]` entry cargo refused to apply shows
//!    up as an artifact compiled from the registry cache, or as no
//!    artifact at all, so a registry fallback fails the check instead of
//!    greening it.
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
//!
//! Each stage lives in its own file: [`package`] packages and extracts,
//! [`resolve`] turns the patched manifest into the host closure,
//! [`patch`] renders and writes the patch table, [`evidence`] reads the
//! build records and judges them.

mod evidence;
mod package;
mod patch;
mod resolve;

#[cfg(test)]
mod fixtures;

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::workspace::{cargo_arg, expect_success, run_cargo, workspace_graph, Package};
use crate::Outcome;

use evidence::{build_diagnostics, parse_build_evidence, verify_workspace_sources};
use package::{crate_tarball, extract_tarball, extracted_package};
use patch::{patch_manifest, read_extracted_manifest, write_patched_manifest};
use resolve::{load_resolved_metadata, resolved_closure};

/// Print a failure and classify it — the one bridge from the pipeline's
/// `String` errors to the binary's verdict vocabulary. Every failure
/// prints exactly where it is classified; `Outcome` carries only the
/// exit class.
fn said(outcome: Outcome, message: String) -> Outcome {
    eprintln!("error: {message}");
    outcome
}

/// The facade crate the check packages (`relate::Database`).
pub(super) const FACADE: &str = "relate";
/// The facade's manifest, relative to the workspace root.
const FACADE_MANIFEST: &str = "relate/relate/Cargo.toml";

/// What one `check-package` run concluded; printed as the summary line.
pub(crate) struct Summary {
    /// The packaged facade's version, from the extracted package
    /// directory name.
    version: String,
    /// Workspace members the patch manifest redirected.
    patched: usize,
    /// Example targets the typed build records show were compiled.
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

/// Run the check: package, extract, patch, build, read the typed build
/// records. Every failure is classified where it occurs, in the one
/// verdict vocabulary the binary carries: `Outcome::Error` — the check
/// could not run (the workspace root, the graph, the scratch directory,
/// a cargo or `tar` spawn, the patch render) — is the exit-2 class
/// `check-boundaries` also uses; `Outcome::Findings` — the check ran and
/// the packaged crate failed it (packaging, extraction, the build, or a
/// source compiled from outside the workspace) — is the exit-1 class.
pub(crate) fn check(root: &Path) -> Result<Summary, Outcome> {
    let root = root.canonicalize().map_err(|e| {
        said(
            Outcome::Error,
            format!(
                "failed to resolve the workspace root {}: {e}",
                root.display()
            ),
        )
    })?;
    let packages = workspace_graph(&root).map_err(|e| said(Outcome::Error, e))?;
    package_and_build(&root, &packages)
}

/// The four stages in order, each classifying its own failures: the
/// scratch `TempDir` is owned here, where its class is known.
fn package_and_build(root: &Path, packages: &[Package]) -> Result<Summary, Outcome> {
    let scratch = scratch_dir().map_err(|e| said(Outcome::Error, e))?;
    let scratch = scratch.path();
    let extracted = package_facade(root, scratch)?;
    let closure = patch_and_resolve(root, packages, &extracted.manifest)?;
    let examples = build_and_verify(root, &extracted, &closure)?;
    Ok(Summary {
        version: extracted.version,
        patched: closure.len(),
        examples,
    })
}

/// The extracted facade package: its directory, the manifest the patch
/// table is written into, and the version packaging stamped.
struct Extracted {
    dir: PathBuf,
    manifest: PathBuf,
    version: String,
}

/// Stage one: package the facade (registry verification is exactly what
/// cannot pass inside the drift window, so it is skipped; the patched
/// build replaces it), extract the tarball into the scratch directory,
/// and find the one `<name>-<version>` package directory. A cargo spawn
/// failure is the tooling class; a packaging refusal (dirty tree,
/// first-release shape) is a finding.
fn package_facade(root: &Path, scratch: &Path) -> Result<Extracted, Outcome> {
    let target_dir = scratch.join("target");
    let package = run_cargo(&[
        "package",
        "--manifest-path",
        cargo_arg(&root.join(FACADE_MANIFEST)).map_err(|e| said(Outcome::Error, e))?,
        "--no-verify",
        "--target-dir",
        cargo_arg(&target_dir).map_err(|e| said(Outcome::Error, e))?,
    ])
    .map_err(|e| said(Outcome::Error, e))?;
    expect_success(&package, "`cargo package --no-verify` (relate facade)")
        .map_err(|e| said(Outcome::Findings, e))?;

    let tarball =
        crate_tarball(&target_dir.join("package")).map_err(|e| said(Outcome::Findings, e))?;
    let pkg_root = scratch.join("pkg");
    fs::create_dir(&pkg_root).map_err(|e| {
        said(
            Outcome::Error,
            format!("failed to create {}: {e}", pkg_root.display()),
        )
    })?;
    let tar_output = extract_tarball(&tarball, &pkg_root).map_err(|e| said(Outcome::Error, e))?;
    expect_success(&tar_output, &format!("`tar -xf {}`", tarball.display()))
        .map_err(|e| said(Outcome::Findings, e))?;
    let (dir, version) = extracted_package(&pkg_root).map_err(|e| said(Outcome::Findings, e))?;
    let manifest = dir.join("Cargo.toml");
    Ok(Extracted {
        dir,
        manifest,
        version,
    })
}

/// Stage two: write the patch table twice. The first pass patches every
/// workspace member, so the resolution below can find each of them on a
/// path; the second patches exactly the closure the host build resolves,
/// so an inactive optional dependency or another platform's edge neither
/// warns as an unused patch nor demands an artifact the build cannot
/// produce.
fn patch_and_resolve<'a>(
    root: &Path,
    packages: &'a [Package],
    manifest: &Path,
) -> Result<Vec<&'a Package>, Outcome> {
    let original = read_extracted_manifest(manifest).map_err(|e| said(Outcome::Findings, e))?;
    let members: Vec<&Package> = packages.iter().filter(|p| p.name != FACADE).collect();
    let patch = patch_manifest(root, &members).map_err(|e| said(Outcome::Error, e))?;
    write_patched_manifest(manifest, &original, &patch).map_err(|e| said(Outcome::Error, e))?;
    let resolved = load_resolved_metadata(manifest).map_err(|e| said(Outcome::Error, e))?;
    let closure = resolved_closure(&resolved, packages).map_err(|e| said(Outcome::Error, e))?;
    let patch = patch_manifest(root, &closure).map_err(|e| said(Outcome::Error, e))?;
    write_patched_manifest(manifest, &original, &patch).map_err(|e| said(Outcome::Error, e))?;
    Ok(closure)
}

/// Stage three: build the lib and every example under the patch, then
/// read the verdict from the typed records. Lib and examples only — the
/// packaged tests need the dev-deps packaging drops (see the module
/// docs). A failed build is a finding whose error carries the rendered
/// compiler diagnostics; the evidence stream is evidence only once the
/// exit is green.
fn build_and_verify(
    root: &Path,
    extracted: &Extracted,
    closure: &[&Package],
) -> Result<usize, Outcome> {
    let build_target = extracted.dir.join("target");
    let build = run_cargo(&[
        "build",
        "--manifest-path",
        cargo_arg(&extracted.manifest).map_err(|e| said(Outcome::Error, e))?,
        "--lib",
        "--examples",
        "--message-format=json",
        "--target-dir",
        cargo_arg(&build_target).map_err(|e| said(Outcome::Error, e))?,
    ])
    .map_err(|e| said(Outcome::Error, e))?;
    expect_success(
        &build,
        "`cargo build --lib --examples` (extracted relate package)",
    )
    .map_err(|e| said(Outcome::Findings, build_failure(e, &build.stdout)))?;
    let build_stdout = std::str::from_utf8(&build.stdout).map_err(|e| {
        said(
            Outcome::Error,
            format!("cargo's --message-format=json stream is not valid UTF-8: {e}"),
        )
    })?;
    let evidence = parse_build_evidence(build_stdout).map_err(|e| said(Outcome::Error, e))?;
    verify_workspace_sources(&evidence, root, closure, &extracted.manifest)
        .map_err(|e| said(Outcome::Findings, e))?;
    Ok(evidence.examples)
}

/// A failed build's error text with the rendered compiler diagnostics
/// appended: `--message-format=json` puts them on stdout, where stderr
/// carries only cargo's summary. A failed build is the finding either
/// way, so an unreadable diagnostic stream annotates the error instead
/// of replacing its class.
fn build_failure(failure: String, stdout: &[u8]) -> String {
    match build_diagnostics(stdout) {
        Ok(diagnostics) if !diagnostics.is_empty() => {
            format!("{failure}\n\n{}", diagnostics.join("\n"))
        }
        Ok(_) => failure,
        Err(unreadable) => format!("{failure}\n({unreadable})"),
    }
}

/// A fresh scratch directory under the system temp dir, removed on
/// every exit path — success, failure and panic alike — by `TempDir`'s
/// Drop. Everything the check produces dies with it. Owned by the check
/// root, where the scratch policy lives; the stage tests import it from
/// here.
fn scratch_dir() -> Result<tempfile::TempDir, String> {
    tempfile::TempDir::new()
        .map_err(|e| format!("failed to create a scratch directory under the system temp dir: {e}"))
}
