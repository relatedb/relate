//! The install flow's preflight predicates (ADR-0431 §3, §5, §7): the
//! name shape, the reserved vocabulary, the manifest checks, the `--from`
//! source-writability rule, and the target-decision matrix — the checks an
//! install that would fail at load fails here, with the load-time refusals'
//! own pinned texts. `mod.rs` runs them in the pre-fetch order.

use std::path::Path;

use crate::extensions::manifest::Manifest;
use crate::extensions::provenance::{Provenance, Tier, MARKER_FILE};
use crate::extensions::registry::{package_name, CollidesWith, RegistryError};

use super::error::InstallError;
use super::package_target;

/// The package name must be a single safe path segment (it becomes a
/// directory name directly under the XDG extensions dir, ADR-0429 §2):
/// non-empty, no separators, not `.` or `..` — the same lexical rule the
/// manifest's executable path applies to its components (ADR-0429 §2),
/// stated for the one component the installer creates: a name that is
/// not a segment could otherwise place the package outside the
/// extensions dir.
pub(super) fn validate_package_name(name: &str) -> Result<(), InstallError> {
    if crate::extensions::is_package_name(name) {
        Ok(())
    } else {
        Err(InstallError::Usage(format!(
            "'{name}' is not a valid package name (a single directory name, no path separators)"
        )))
    }
}

/// The reserved-vocabulary check (ADR-0431 §7): a package whose name
/// collides with a reserved command word ([`crate::flags::reserved_word`],
/// ADR-0069 — locally knowable, dependency-free) refuses with the registry's pinned
/// collision text — the same text the load-time refusal prints
/// (§4 step 5's rule: an install that would fail at load fails here,
/// with the same pinned texts). With the `lab` surface retired
/// (ADR-0431 §12) there are no built-in action words left to collide
/// with: the first-party importers are installed packages, so the
/// installed-set check in [`check_target`] is the one that refuses.
pub(super) fn check_reserved_words(name: &str) -> Result<(), InstallError> {
    match crate::flags::reserved_word(name) {
        Some(word) => Err(InstallError::Usage(
            RegistryError::reserved_word(name, word).to_string(),
        )),
        None => Ok(()),
    }
}

/// The per-manifest load refusals (ADR-0429 §6 + §8's protocol check),
/// the same predicates the registry applies at load — an install that
/// would fail at load fails here (ADR-0431 §4 step 5): the strict decode
/// ran at the caller; here the escaping executable, the manifest's name,
/// and the protocol the host speaks.
pub(super) fn check_manifest(name: &str, manifest: &Manifest) -> Result<(), InstallError> {
    let Manifest::Command(command) = manifest;
    // An empty executable is not a path at all: it would join to the
    // package directory itself (the loose-files tiers' staging and the
    // URL join both take it as a relative path), so it refuses here with
    // the manifest class rather than surfacing as a staging IO error.
    if command.executable.as_os_str().is_empty() {
        return Err(InstallError::Usage(format!(
            "extension '{name}': the manifest's executable path is empty"
        )));
    }
    // The escaping-executable rule (ADR-0429 §2): the executable resolves
    // only inside the package directory — the extensions root's one
    // containment predicate.
    if crate::extensions::escapes_package_dir(&command.executable) {
        return Err(InstallError::Usage(
            RegistryError::EscapingExecutable {
                package: name.to_string(),
                executable: command.executable.display().to_string(),
            }
            .to_string(),
        ));
    }
    // The manifest's name is the mount name; the install's positional
    // names the package — they must agree, so no package lands under a
    // name it does not claim.
    if command.name != name {
        return Err(InstallError::Runtime(format!(
            "the package's manifest names '{}' but the install was for '{name}'",
            command.name
        )));
    }
    // The protocol check (§8): the manifest's protocol must be the MCP
    // revision this host serves, else the fail-closed-no-negotiation
    // refusal (ADR-0447), caught one boundary earlier — a manifest problem
    // (exit 2).
    let served = relate_wire::mcp::MCP_PROTOCOL_VERSION;
    if command.protocol != served {
        return Err(InstallError::Usage(format!(
            "package '{name}' {} speaks protocol '{}', but this host speaks '{served}'",
            command.version, command.protocol
        )));
    }
    Ok(())
}

/// A source file's permission bits, or 0 where the platform has none.
/// These are the bits the `--from` copy can land with (`fs::copy` carries
/// the mode) and the writability check judges; on non-unix there are no
/// mode bits, so the value is 0 and [`check_source_writable`] has nothing
/// to refuse — the same platform split the unpacker's `apply_mode` keeps.
#[cfg(unix)]
pub(super) fn source_mode(metadata: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode()
}

/// Non-unix builds carry no mode bits: [`unpack::apply_mode`] is the
/// unpacker's no-op there, and the writability check accepts every file.
#[cfg(not(unix))]
pub(super) fn source_mode(_metadata: &std::fs::Metadata) -> u32 {
    0
}

/// Refuse a source file a group or another account can write, before the
/// installer copies it. The invoker points `--from` at a directory they
/// provisioned, but a group-/world-writable executable is a file a third
/// account could swap between the checks above and the landing; the
/// refusal names the file, its mode, and the offending bits, in the
/// module's runtime-refusal style (exit 1). On non-unix [`source_mode`]
/// returns 0, so nothing is refused.
pub(super) fn check_source_writable(path: &Path, mode: u32) -> Result<(), InstallError> {
    // 0o022 is exactly the group-write and other-write bits: the classes
    // whose write permission admits an account other than the owner.
    let offending = mode & 0o022;
    if offending == 0 {
        return Ok(());
    }
    Err(InstallError::Runtime(format!(
        "{} is writable by group or others (mode {:03o}, offending bits {:03o}); --from \
         refuses a source file other accounts can change — clear the group and other write bits",
        path.display(),
        mode & 0o777,
        offending
    )))
}

/// The installed-set and target checks (ADR-0431 §7, §3): a package whose
/// name collides with an installed extension of any provenance refuses
/// naming both sides — a first-party package can never be replaced by a
/// third-party one, or the reverse, without an explicit `remove` — and a
/// foreign target directory (one without the provenance marker) refuses
/// unless `--force`.
///
/// The reconciliation of §3's "a marked install of the same name replaces
/// cleanly (reinstall or version change)" with §7's collision refusal:
/// the same *tier* reinstalling over its own marked target is the clean
/// replace; a different tier over a marked target is the shadow §7
/// forbids; an installed extension whose package directory is not the
/// target is a true collision (a hand-copied package claiming the name
/// from another directory).
pub(super) fn check_target(
    extensions_dir: &Path,
    name: &str,
    incoming: Tier,
    force: bool,
    registry: &crate::extensions::registry::ActionRegistry,
) -> Result<(), InstallError> {
    let target = package_target(extensions_dir, name);
    if let Some(entry) = registry.find(name) {
        if entry.package_dir != target {
            // A true collision: another installed package claims the name
            // from a different directory — the load-time refusal's text.
            return Err(InstallError::Runtime(
                RegistryError::Collision {
                    name: name.to_string(),
                    other: CollidesWith::InstalledExtension(package_name(&entry.package_dir)),
                }
                .to_string(),
            ));
        }
        // The installed package claiming this name is the target itself:
        // its marker decides below.
    }
    if !target.exists() {
        return Ok(()); // fresh install
    }
    // The target directory exists: its marker decides.
    decide_marked_dir(name, &target, &target, incoming, force)
}

/// The one marker decision both the pre-fetch snapshot ([`check_target`])
/// and the land-time re-decision ([`recheck_parked_install`]) apply: a
/// broken marker is a runtime error, a marked package of another tier is
/// the cross-tier refusal, the same tier replaces cleanly, and an unmarked
/// directory is foreign (needs `--force`). `marker_dir` is where the marker
/// is read (the target, or the package parked aside); `foreign_dir` is the
/// directory a foreign refusal names.
fn decide_marked_dir(
    name: &str,
    marker_dir: &Path,
    foreign_dir: &Path,
    incoming: Tier,
    force: bool,
) -> Result<(), InstallError> {
    match Provenance::read(marker_dir).map_err(|e| e.to_string()) {
        Err(text) => Err(InstallError::Runtime(text)),
        Ok(Some(provenance)) if provenance.tier != incoming => Err(cross_tier_refusal(
            name,
            provenance.tier,
            incoming,
            &provenance.source,
        )),
        // Same tier, same name: the clean replace (§3).
        Ok(Some(_)) => Ok(()),
        Ok(None) => foreign_target(foreign_dir, force),
    }
}

/// The foreign-target refusal (§3, the `skill install` refusal ADR-0261):
/// a target without the provenance marker is not ours to touch — exit 1,
/// pinned text — unless `--force`.
fn foreign_target(target: &Path, force: bool) -> Result<(), InstallError> {
    if force {
        Ok(())
    } else {
        Err(InstallError::Runtime(format!(
            "refusing to touch {}: no {} marker present (the directory was not installed by \
             relate); pass --force to override",
            target.display(),
            MARKER_FILE
        )))
    }
}

/// The cross-tier refusal text (ADR-0431 §7): one spelling for the
/// pre-fetch decision ([`check_target`]) and the land-time re-decision
/// ([`recheck_parked_install`]), so the two cannot drift.
fn cross_tier_refusal(name: &str, installed: Tier, incoming: Tier, source: &str) -> InstallError {
    InstallError::Runtime(format!(
        "extension '{name}' is installed {} (source: {}); refusing a {} \
         install over it — remove it first: relate extension remove {name}",
        installed.as_str(),
        source,
        incoming.as_str()
    ))
}

/// Re-decide the replace against the package actually parked aside.
/// `check_target`'s snapshot is taken before the fetch, so a concurrent
/// install of the same name can land in between; the parked package's own
/// marker is the authority at the moment of the clobber. A tier-crossing
/// replace refuses (the shadow §7 forbids), a foreign target needs
/// `--force` (re-verified here, not only in the snapshot), and the same
/// tier replaces cleanly. The caller restores the parked package when this
/// refuses.
pub(super) fn recheck_parked_install(
    name: &str,
    target: &Path,
    parked: &Path,
    incoming: Tier,
    force: bool,
) -> Result<(), InstallError> {
    decide_marked_dir(name, parked, target, incoming, force)
}
