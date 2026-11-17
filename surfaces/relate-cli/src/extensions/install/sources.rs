//! The install flow's source adapters (ADR-0431 §4-§6, ADR-0449): each
//! trust tier's fetch-and-stage step. The channel verifies the signed
//! index against the pinned key, resolves the row, downloads through the
//! route machinery and checks the signed row's SHA256; the GitHub pin
//! tier resolves the tag's commit and the asset's SHA256 and compares
//! them against the per-user trust store before any fetch; the URL tier's
//! transport gate is the act's one integrity leg; the local directory
//! tier copies exactly the accounted set. Every adapter unpacks into the
//! staging dir through the one closed member contract and runs the same
//! preflight checks.

use std::path::Path;

use crate::extensions::github;
use crate::extensions::manifest::Manifest;
use crate::extensions::provenance::{Pin, Provenance, Tier};
use crate::extensions::source::{GithubSpec, InstallArgs};
use crate::extensions::trust::TrustStore;
use crate::extensions::{channel, unpack};
use crate::release_channel::download_route::probe_route_at;
use crate::release_channel::http::{approved_transport, download_asset, AssetRequest};
use crate::release_channel::sha256_hex;

use super::checks::{check_manifest, check_source_writable, source_mode};
use super::error::InstallError;
use super::{io, GithubResolver};

/// The default source (ADR-0431 §4): fetch the channel index and its
/// signature, verify the SSHSIG against the pinned key, resolve the row
/// (name, version, platform asset — the protocol pre-check fires before
/// the download), download the tarball through the route machinery,
/// verify its SHA256 against the signed row, and unpack into staging.
/// Steps 2-4 are [`install_from_index`] over the just-verified document;
/// the split is the registry's injectable shape (the `load`/`load_with`
/// pattern) — the channel e2e stops at the signature gate (the pinned
/// release key never verifies a fixture-key signature, and the ADR pins
/// no key seam), so the post-signature steps are unit-driven with the
/// fixture index against a loopback route.
pub(super) fn install_from_channel(
    name: &str,
    version: Option<&str>,
    staged: &Path,
) -> Result<(Provenance, Manifest), InstallError> {
    let index = channel::fetch_verified_index()?;
    let base = crate::release_channel::download_route::release_download_base()?;
    install_from_index(&index, name, version, &base, staged)
}

/// The channel install's steps 2-4 (ADR-0431 §4) over an
/// already-signature-verified `index`: resolve the row (step 2 — the name,
/// the version, the protocol pre-check, the host platform's asset row),
/// download the tarball through the route machinery at the direct-route
/// `base` (step 3 — the route the env seam resolves, threaded as the
/// parameter [`probe_route_at`] takes so the fixture-index driver can
/// point it at a loopback route), verify its SHA256 against the signed
/// row, and unpack into staging (step 4). The index arrives verified —
/// step 1 ran at the caller — so nothing here re-checks the signature:
/// the signed row's checksum is the artifact's authentication.
pub(super) fn install_from_index(
    index: &channel::ChannelDocument,
    name: &str,
    version: Option<&str>,
    base: &str,
    staged: &Path,
) -> Result<(Provenance, Manifest), InstallError> {
    let row = channel::resolve_row(index, name, version)?;
    // The tarball is both the probe asset and the only needed asset
    // (§1: asset downloads ride the probe-then-fallback route machinery).
    let token = crate::release_channel::env_nonempty("GITHUB_TOKEN")?;
    let needed = [row.asset];
    let probe = probe_route_at(
        crate::release_channel::REPO,
        base,
        row.tag,
        row.asset,
        &needed,
        token.as_deref(),
        Some(&format!(
            "install the package from a local directory: \
             relate extension install {name} --from DIR"
        )),
    )?;
    if let Some(note) = &probe.note {
        // ADR-0358 §6's one fallback note, like the upgrade prints it.
        crate::print_error(note);
    }
    let tarball = probe.probe_bytes;
    // Change nothing on a mismatch: the extensions dir keeps its prior state.
    verify_checksum(row.asset, &row.sha256, &tarball)?;
    let manifest = unpack_and_check(name, &tarball, staged)?;
    Ok((
        Provenance {
            tier: Tier::FirstParty,
            pin: None,
            // The tag names the channel release that hosts the assets
            // (§7's "the channel version": the version itself is the
            // manifest's own field, the loud line reads it back).
            source: row.tag.to_string(),
        },
        manifest,
    ))
}

/// The one checksum gate both pinned tiers apply to a fetched tarball: the
/// bytes' SHA256 must equal the signed (channel) or resolved (GitHub) value,
/// else the install stops before anything is unpacked.
fn verify_checksum(asset: &str, expected: &str, bytes: &[u8]) -> Result<(), InstallError> {
    let actual = sha256_hex(bytes);
    if actual == expected {
        Ok(())
    } else {
        Err(InstallError::Runtime(format!(
            "checksum verification failed for {asset} (expected {expected}, got {actual}); \
             not installing"
        )))
    }
}

/// The explicit third-party trust act (§6): the URL names the package
/// tarball at an https source the invoker chooses — no signature exists
/// to check, so the tier's transport is the act's one leg of integrity
/// and a non-https URL is an authored refusal before any fetch. The
/// trust act is the invocation itself. Fetch, unpack under the same
/// closed member contract, run the same checks, and record the URL as
/// provenance. The loud install line at the end names the package, its
/// version, and the URL it came from.
pub(super) fn install_from_url(
    name: &str,
    url: &str,
    staged: &Path,
) -> Result<(Provenance, Manifest), InstallError> {
    // The tier's transport gate: no signature covers a third-party
    // tarball (§6), so the download itself is the trust act's integrity —
    // a plaintext fetch is a man-in-the-middle's open microphone. The one
    // shared predicate (`release_channel::http::approved_transport`) is
    // the rule: https, or plain http to the loopback interface (no
    // on-path attacker; the local fixture registry shape). A flag value
    // outside the accepted vocabulary: the usage class (ADR-0431 §3),
    // refused before any fetch.
    if !approved_transport(url) {
        return Err(InstallError::Usage(format!(
            "--url accepts only https URLs (or plain http to the loopback \
             interface); '{url}' is neither"
        )));
    }
    let label = format!("the package tarball for {name}");
    let tarball = download_asset(AssetRequest {
        url,
        name: &label,
        token: None,
        accept: None,
    })
    .map_err(|e| {
        InstallError::Runtime(format!(
            "could not download the package tarball from {url}: {}",
            e.into_channel_error()
        ))
    })?;
    let manifest = unpack_and_check(name, &tarball, staged)?;
    Ok((
        Provenance {
            tier: Tier::ThirdParty,
            pin: None,
            source: url.to_string(),
        },
        manifest,
    ))
}

/// One staged-from source file's checks: a regular file (no symlink, the
/// package contract), not writable by group or other; returns its mode so
/// the executable's can ride to the staged copy.
fn check_source_file(path: &Path) -> Result<u32, InstallError> {
    let meta = std::fs::symlink_metadata(path)
        .map_err(|e| InstallError::Runtime(format!("reading {} failed: {e}", path.display())))?;
    if !meta.is_file() {
        return Err(InstallError::Runtime(format!(
            "{} is not a regular file (the package contract is regular files only)",
            path.display()
        )));
    }
    let mode = source_mode(&meta);
    check_source_writable(path, mode)?;
    Ok(mode)
}

/// The local directory trust act (§5): `--from DIR` points at an unpacked
/// package directory — the posture `pull --from FILE` pinned (ADR-0080
/// §3): whoever provisions the directory owns what is in it. The
/// manifest, protocol and collision checks run unchanged; exactly the
/// accounted files are copied into place, the executable resolving
/// inside the directory it is named in (the source directory is left
/// untouched), the provenance recorded as the local path.
///
/// The copy lands under the unpacker's landing-mode policy, not the
/// source's: the executable's mode is masked to the rwx bits through
/// [`unpack::apply_mode`] (ADR-0431 §2's one masking rule), and a source
/// file a group or another account can write is refused before the copy —
/// the source directory is the invoker's, but a writable-by-others
/// executable is a file a third account could swap between the checks and
/// the landing.
pub(super) fn install_from_dir(
    name: &str,
    dir: &str,
    staged: &Path,
) -> Result<(Provenance, Manifest), InstallError> {
    let source = Path::new(dir);
    let manifest_path = source.join("extension.json");
    let manifest_bytes = std::fs::read(&manifest_path).map_err(|e| {
        InstallError::Runtime(format!("reading {} failed: {e}", manifest_path.display()))
    })?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| InstallError::Usage(format!("extension '{name}': invalid manifest: {e}")))?;
    check_manifest(name, &manifest)?;
    let Manifest::Command(command) = &manifest;
    let executable = source.join(&command.executable);
    // The accounted files are regular files — the closed contract's
    // parity (§2: regular files only), so a directly symlinked manifest
    // or executable refuses (the regular-file check), and an
    // intermediate-directory symlink refuses at the containment below.
    // The same stat carries the source mode the executable may land with
    // (the manifest's own mode never rides: it is staged with
    // `fs::write`, which creates a fresh file).
    //
    // Ownership is deliberately NOT checked against the current uid: the
    // invoker names a local directory and the host runs the child as that
    // same user, so an install grants nothing the invoker could not get by
    // executing the source file directly; and std has no portable
    // identity source in this tree to compare against. The write bits
    // below are the property that actually widens who may rewrite the
    // installed executable, so they are the one checked.
    check_source_file(&manifest_path)?;
    let executable_mode = check_source_file(&executable)?;
    // The executable must RESOLVE inside the source directory, not
    // merely be spelled inside it — a `bin` symlink pointing elsewhere
    // would otherwise carry out-of-directory content in through a
    // lexically-inside name, past the regular-file check above (the
    // file it reaches is a regular file). Canonicalize both sides and
    // contain: the executable's fully-resolved path stays under the
    // source directory's own.
    let rooted = std::fs::canonicalize(source)
        .map_err(|e| io("resolving the package directory", source, &e))?;
    let resolved = std::fs::canonicalize(&executable)
        .map_err(|e| io("resolving the executable", &executable, &e))?;
    if !resolved.starts_with(&rooted) {
        return Err(InstallError::Runtime(format!(
            "{} resolves outside the package directory (to {}); --from copies only files \
             inside the directory it is given",
            executable.display(),
            resolved.display()
        )));
    }
    // Copy exactly the accounted set: nothing else rides — the installed
    // dir holds the manifest, the executable, and the marker, the same
    // invariant the unpacker enforces for tarballs (`fs::copy` preserves
    // the executable's mode on unix).
    std::fs::write(staged.join("extension.json"), &manifest_bytes)
        .map_err(|e| io("staging the manifest", staged, &e))?;
    let staged_executable = staged.join(&command.executable);
    if let Some(parent) = staged_executable.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io("staging the executable", staged, &e))?;
    }
    std::fs::copy(&executable, &staged_executable)
        .map_err(|e| io("staging the executable", staged, &e))?;
    // `fs::copy` carries the source's mode — including setuid/setgid/
    // sticky. Mask it exactly as the unpacker masks a tarball member's
    // mode (`apply_mode`, the one landing-mode policy, ADR-0431 §2), so a
    // `--from` install and an unpacked install land the same permission
    // shape for the same package.
    unpack::apply_mode(&staged_executable, executable_mode)
        .map_err(|e| io("staging the executable", staged, &e))?;
    Ok((
        Provenance {
            tier: Tier::Local,
            pin: None,
            source: dir.to_string(),
        },
        manifest,
    ))
}

/// The GitHub pin tier's install (ADR-0449): resolve the pin over the
/// REST API (tag → commit, release → platform asset and SHA256), compare
/// it against the recorded pins (read-only — this step never writes the
/// store) *before* any asset fetch, download through the
/// probe-then-fallback route, verify the bytes against the resolved
/// SHA256, and unpack under the closed contract. The resolved pin rides
/// back inside the returned [`Provenance`]; the caller records it in the
/// per-user trust store once the package has landed, so the trust entry
/// and the package's own marker commit together.
pub(super) fn install_from_github(
    spec: &GithubSpec,
    args: &InstallArgs,
    github_route: GithubResolver<'_>,
    trust: &TrustStore,
    staged: &Path,
) -> Result<(Provenance, Manifest), InstallError> {
    let name = args.name.as_str();
    let route = github_route(&spec.repo)?;
    let resolved = github::resolve(
        &spec.repo,
        name,
        &spec.pin,
        &route.api_base,
        route.token.as_deref(),
    )?;
    let pin = resolved.pin(&spec.repo);
    // The pin comparison, on local state plus the metadata the resolution
    // just fetched: a moved pin refuses here, before the tarball request
    // (ADR-0449's "refuses BEFORE any fetch").
    compare_pin(
        name,
        trust.get(name),
        &pin,
        args.re_approve,
        args.approve.as_deref(),
    )?;

    let needed = [resolved.asset.as_str()];
    let probe = probe_route_at(
        &spec.repo,
        &route.download_base,
        &pin.tag,
        &resolved.asset,
        &needed,
        route.token.as_deref(),
        Some(&format!(
            "install the package from a local directory: \
             relate extension install {name} --from DIR"
        )),
    )?;
    if let Some(note) = &probe.note {
        // ADR-0358 §6's one fallback note, printed sanitized like the
        // channel install's.
        crate::print_error(note);
    }
    let tarball = probe.probe_bytes;
    verify_checksum(&resolved.asset, &pin.asset_sha256, &tarball)?;
    let manifest = unpack_and_check(name, &tarball, staged)?;
    // The asset verified against the resolved SHA256 and the package passed
    // every load check; the pin rides back in the provenance and is
    // recorded by the caller after the landing (ADR-0449: a re-approval
    // updates the recorded pin only after verification — and a failed
    // install leaves the previous pin in place).
    Ok((
        Provenance {
            tier: Tier::GithubPin,
            source: format!("{}@{}", spec.repo, pin.tag),
            pin: Some(pin),
        },
        manifest,
    ))
}

/// The pin comparison (ADR-0449): a first install (no recorded pin) is
/// approved by the invocation itself; the same pin reinstall proceeds
/// silently; any difference — a moved tag, a changed asset, another
/// repository — refuses unless the invocation re-approves it, and
/// `--approve <sha>` accepts only a pin that resolves to exactly that
/// commit.
pub(super) fn compare_pin(
    name: &str,
    recorded: Option<&Pin>,
    resolved: &Pin,
    re_approve: bool,
    approve: Option<&str>,
) -> Result<(), InstallError> {
    // `--approve` names the commit the pin must resolve to — checked
    // before any "unchanged" shortcut, so a wrong approval never passes
    // silently.
    if let Some(approved) = approve {
        if approved != resolved.commit {
            return Err(InstallError::Runtime(format!(
                "extension '{name}' resolves to commit {}, not the approved commit {approved}; \
                 nothing installed",
                resolved.commit
            )));
        }
    }
    let Some(recorded) = recorded else {
        return Ok(());
    };
    // An approval that reached here named the resolved commit; a
    // re-approval or an unchanged pin accepts it too.
    if recorded == resolved || re_approve || approve.is_some() {
        return Ok(());
    }
    Err(InstallError::Runtime(format!(
        "extension '{name}' is pinned to {}@{} (commit {}, asset sha256 {}); {}@{} now resolves \
         to commit {}, asset sha256 {}; refusing before any fetch — re-approve with --re-approve \
         or --approve <commit-sha>",
        recorded.repo,
        recorded.tag,
        recorded.commit,
        recorded.asset_sha256,
        resolved.repo,
        resolved.tag,
        resolved.commit,
        resolved.asset_sha256
    )))
}

/// Unpack a tarball into `staged` and run the staged-package checks
/// (§4 steps 4-5): the closed member contract, then the manifest decode,
/// the escaping-executable rule, the member accounting, the manifest's
/// name agreement, and the protocol the host speaks — every ADR-0429
/// load refusal plus the protocol check (§8) at install time, so an
/// install that would fail at load fails here. Returns the decoded
/// manifest (the caller reads the version from it for the loud line).
fn unpack_and_check(name: &str, tarball: &[u8], staged: &Path) -> Result<Manifest, InstallError> {
    let members = unpack::unpack(tarball, staged)?;
    let manifest_bytes = std::fs::read(staged.join("extension.json")).map_err(|e| {
        InstallError::Runtime(format!(
            "the package tarball does not carry a readable extension.json: {e}"
        ))
    })?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| InstallError::Usage(format!("extension '{name}': invalid manifest: {e}")))?;
    check_manifest(name, &manifest)?;
    let Manifest::Command(command) = &manifest;
    // The member accounting (§2): exactly the files the manifest accounts
    // for — `extension.json` and the executable — nothing else, in any
    // order.
    let mut accounted = vec![
        "extension.json".to_string(),
        command.executable.display().to_string(),
    ];
    let mut carried: Vec<String> = members;
    accounted.sort();
    carried.sort();
    if carried != accounted {
        return Err(InstallError::Runtime(format!(
            "the tarball's members ({}) are not the package the manifest accounts for ({})",
            carried.join(", "),
            accounted.join(", ")
        )));
    }
    Ok(manifest)
}
