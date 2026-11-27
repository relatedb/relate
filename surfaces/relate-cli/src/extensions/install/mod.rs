//! The `extension install` verb (ADR-0431 §3-§7): verify, unpack, check —
//! never a silent fetch. The network is touched only by this verb (and
//! `extension list --available`, §9); discovery, `run`, and every failure
//! on an uninstalled name never fetch (ADR-0429 §7 stands unchanged).
//!
//! The five sources, one ladder of trust tiers (§7, ADR-0449): the
//! first-party channel (§4 — fetch the signed index, resolve the row,
//! download the tarball, verify its SHA256 against the signed row), a
//! GitHub release pinned to a commit (ADR-0449 — resolve the tag's commit
//! and the platform asset's SHA256, compare against the per-user trust
//! store before any fetch, download over the route machinery, verify,
//! record the pin and land it — the pin rolls back if the landing
//! fails; the adapters hand back the tier-less
//! [`SourceFacts`](crate::extensions::provenance::SourceFacts), and the
//! tier is stamped from `InstallSource::tier()` when the marker's
//! provenance is built), a third-party URL (§6 — the invocation is
//! the trust act: fetch, unpack, run the same checks, record the URL,
//! print the loud line), a local directory (§5 — the posture of
//! `pull --from`: whoever provisions the directory owns what is in it; the
//! checks run unchanged, the accounted files are copied into place), and
//! `--from URL` (§5's remote form — the same loose-files layout fetched,
//! third-party tier).
//!
//! The landing shape is the `skill install` staging shape (§4 step 4):
//! unpack/copy into a `.relate-stage-*` sibling of the target, run
//! every load refusal against the staged package, write the provenance
//! marker, then rename into place — a failed install never leaves a
//! half-written package.
//!
//! Exit codes follow ADR-0430 §5's split as ADR-0431 §3 pins it: flag
//! problems and manifest problems (a reserved-word package name, an
//! unknown kind, a protocol the host does not speak, an escaping
//! executable) exit 2; name-resolution, environment, network,
//! verification and unpack failures exit 1 — the
//! [`InstallError`](error::InstallError) classification, stated once on
//! the type.

use std::path::{Path, PathBuf};

use relate_formatter::sanitize_error_text;

use crate::extensions::manifest::Manifest;
use crate::extensions::provenance::{Pin, Provenance, Tier};
use crate::extensions::source::{InstallArgs, InstallSource};
use crate::extensions::trust::{trust_path, TrustStore};
use crate::release_channel::api_assets::github_api_url;
use crate::release_channel::download_route::github_download_base;
use crate::release_channel::env_nonempty;
use crate::staging;

/// The preflight predicates (name, vocabulary, manifest, source, target).
mod checks;
/// The install flow's typed failure — the leaf module the channel and
/// unpack peers import downward (ADR-0431 §3's exit classes on the
/// type).
pub(crate) mod error;
/// The per-tier source adapters (channel, GitHub pin, URL, local
/// directory).
mod sources;
/// The `--from URL` layout rules: the pure string decisions (which URL
/// is the manifest, the executable's folder join, the one GitHub
/// rewrite) the URL adapter fetches through.
mod url_layout;

use self::checks::{
    check_reserved_words, check_target, recheck_parked_install, validate_package_name,
};
use self::error::InstallError;
use self::sources::{
    install_from_channel, install_from_dir, install_from_github, install_from_url,
    install_from_url_files,
};

/// The `--github` tier's network endpoints and identity, resolved once per
/// install (ADR-0449): the REST API base (`RELATE_GITHUB_API_URL`),
/// the repository's direct download base
/// (`RELATE_RELEASE_DOWNLOAD_URL`), and the optional `GITHUB_TOKEN`.
/// The injectable-env shape (`load`/`load_with`): [`run_install`] passes
/// the real resolver, the tests pass a loopback route.
#[derive(Debug, Clone)]
pub(crate) struct GithubRoute {
    api_base: String,
    download_base: String,
    token: Option<String>,
}

impl GithubRoute {
    /// The real-environment route for `repo`.
    fn from_env(repo: &str) -> Result<Self, InstallError> {
        Ok(Self {
            api_base: github_api_url()?,
            download_base: github_download_base(repo)?,
            token: env_nonempty("GITHUB_TOKEN")?,
        })
    }

    /// An explicit route — the tests' loopback fixture.
    #[cfg(test)]
    fn at(api_base: &str, download_base: &str) -> Self {
        Self {
            api_base: api_base.to_string(),
            download_base: download_base.to_string(),
            token: None,
        }
    }
}

/// The pin resolver an install is driven with: the real environment, or
/// the tests' loopback route.
type GithubResolver<'a> = &'a dyn Fn(&str) -> Result<GithubRoute, InstallError>;

/// Run `extension install` against the real config dir: the entry point
/// `extension_cmd` calls. No config dir resolving (neither
/// `$XDG_CONFIG_HOME` nor `$HOME`) is the environment failure (exit 1).
pub(crate) fn run_install(args: &InstallArgs) -> Result<(), InstallError> {
    let Some(config_dir) = crate::config_dir::config_dir() else {
        return Err(InstallError::Runtime(
            crate::config_dir::no_config_dir_text(),
        ));
    };
    let registry = crate::extensions::registry::ActionRegistry::load()?;
    install_into(
        &config_dir.join(crate::extensions::registry::EXTENSIONS_DIR),
        &trust_path(&config_dir),
        &GithubRoute::from_env,
        args,
        &registry,
    )
}

/// The install flow against an explicit extensions directory — the real
/// entry point's core, injectable for tests (the registry over the same
/// tree is the caller's: the real-env load here, a fixture `load_with`
/// in the tests — the registry's own injectable-env pattern).
/// `extensions_dir` is the XDG walk's root (ADR-0429 §6): packages live in
/// directories directly under it, the target is
/// `<extensions_dir>/<name>`, and the staged sibling lives beside the
/// target (ADR-0431 §4 step 4).
fn install_into(
    extensions_dir: &Path,
    trust_file: &Path,
    github_route: GithubResolver<'_>,
    args: &InstallArgs,
    registry: &crate::extensions::registry::ActionRegistry,
) -> Result<(), InstallError> {
    let InstallArgs {
        name,
        source,
        force,
        ..
    } = args;

    // Cheap and local first, so a bad name refuses without touching the
    // network: the name shape (it becomes a directory name under the
    // extensions dir), the reserved vocabulary, and the target's state.
    // The installed set decides the target check — an install that would
    // fail at load fails here (ADR-0431 §4 step 5); a broken set is the
    // environment failure, the registry's own pinned text (exit 1).
    validate_package_name(name)?;
    check_reserved_words(name)?;
    check_target(extensions_dir, name, source.tier(), *force, registry)?;
    // The recorded pins are read before any source work — and, for a
    // `--github` install, before any fetch — so a moved pin refuses on
    // local state plus the API's resolution, never after a tarball was
    // downloaded (ADR-0449). A malformed store is a refusal, never an
    // empty one — and it refuses before the staging dir exists, so the
    // failure leaves no residue.
    let mut trust = TrustStore::load(trust_file)?;

    // Stage → source work → marker → rename: a failed install never
    // leaves a half-written package, and the staged dir is cleaned on
    // every error path below.
    std::fs::create_dir_all(extensions_dir)
        .map_err(|e| io("creating the extensions directory", extensions_dir, &e))?;
    staging::sweep_stale(extensions_dir);
    let staged = staging::staging_dir(extensions_dir);
    std::fs::create_dir(&staged).map_err(|e| io("staging the package", &staged, &e))?;

    let landed = stage_and_land(extensions_dir, github_route, args, &mut trust, &staged);
    match landed {
        Ok((provenance, version)) => {
            println!("{}", loud_line(name, &version, &provenance));
            Ok(())
        }
        Err(err) => {
            let _ = std::fs::remove_dir_all(&staged);
            Err(err)
        }
    }
}

/// One source's work: verify and stage the package into `staged`, write
/// the provenance marker, then rename the complete package into place.
/// Returns the provenance and the staged manifest's version (the loud
/// line reports what actually landed).
///
/// A `--github` install's pin commits with the package (ADR-0449): the
/// source step resolves and hands the pin back inside the [`Provenance`],
/// and the trust store is written here, beside the provenance marker —
/// before the rename, with the store's prior bytes restored when the
/// landing fails, so a landing failure leaves the recorded pins
/// byte-identical to before the attempt and a failed trust write leaves
/// nothing landed.
///
/// The replace re-decides at land time ([`recheck_parked_install`]):
/// `check_target`'s snapshot was taken before the fetch, so a concurrent
/// install of the same name is invisible to it.
fn stage_and_land(
    extensions_dir: &Path,
    github_route: GithubResolver<'_>,
    args: &InstallArgs,
    trust: &mut TrustStore,
    staged: &Path,
) -> Result<(Provenance, String), InstallError> {
    let name = args.name.as_str();
    let (provenance, manifest) = match &args.source {
        InstallSource::Url(url) => install_from_url(name, url, staged)?,
        InstallSource::From(dir) => install_from_dir(name, dir, staged)?,
        // The remote form of `--from` (ADR-0431 §5): the loose-files
        // package at a URL — third-party tier, the transport gate's
        // posture.
        InstallSource::FromUrl(url) => install_from_url_files(name, url, staged)?,
        InstallSource::Channel { version } => {
            install_from_channel(name, version.as_deref(), staged)?
        }
        InstallSource::Github(spec) => {
            install_from_github(spec, args, github_route, trust, staged)?
        }
    };
    // The tier is the source's own ([`InstallSource::tier`]) — the
    // adapters hand back only the pin and the source reference, so the
    // variant→tier mapping has one home.
    let provenance = Provenance {
        tier: args.source.tier(),
        pin: provenance.pin,
        source: provenance.source,
    };
    let Manifest::Command(command) = &manifest;
    provenance
        .write(staged)
        .map_err(|e| io("writing the provenance marker", staged, &e))?;
    let target = package_target(extensions_dir, name);
    // The pin commits with the landing (ADR-0449 §4): it is recorded
    // BEFORE the rename and the store's prior bytes are restored when
    // the landing fails, so the recorded pin and the package land
    // together or not at all. The order is what makes both failure
    // directions safe: a failed trust write leaves nothing landed (the
    // command reports failure and no package appeared), and a failed
    // landing leaves the store byte-identical to before — never a
    // landed-but-unpinned package, whose later reinstall would take the
    // first-install path and silently approve a moved pin.
    let committed_pin = match &provenance.pin {
        Some(pin) => {
            let prior = trust.record(name.to_string(), pin.clone())?;
            PinCommit::Committed { prior }
        }
        None => PinCommit::NotCommitted,
    };
    let landed = if target.exists() {
        land_replacing(&target, staged, name, &args.source, args.force)
    } else {
        std::fs::rename(staged, &target).map_err(|e| io("landing the package", &target, &e))
    };
    if let Err(err) = landed {
        // The landing failed: the commit state owns the compensation (and
        // its double-failure report).
        committed_pin.rollback(trust, name);
        return Err(err);
    }
    Ok((provenance, command.version.clone()))
}

/// The pin commit's state around a landing (ADR-0449): nothing was
/// committed, or the prior entry a [`TrustStore::rollback`] puts back.
/// The state owns its own transition, so a landing failure is one call.
enum PinCommit {
    /// No pin was committed — a landing failure has nothing to undo.
    NotCommitted,
    /// The pin was recorded; `prior` is the entry it replaced.
    Committed {
        /// The entry the record replaced (`None` when the name had no
        /// recorded pin).
        prior: Option<Pin>,
    },
}

impl PinCommit {
    /// Undo the commit after a failed landing: put the prior entry back.
    /// A rollback that fails too is reported beside the original error —
    /// the pin then outlives no installed package, the safe direction (a
    /// later install compares against it and refuses a moved pin instead
    /// of silently approving one).
    fn rollback(self, trust: &mut TrustStore, name: &str) {
        let Self::Committed { prior } = self else {
            return;
        };
        if let Err(restore) = trust.rollback(name, prior) {
            crate::print_error(format!(
                "rolling the extension trust store back after a failed landing failed too \
                 ({restore}); the pin for '{name}' may outlive its package — the next \
                 install compares against it and refuses a moved pin"
            ));
        }
    }
}

/// Land the staged package over an existing target (ADR-0431 §3): park the
/// installed package aside, re-decide the clobber against the package
/// actually parked, then rename the staged package into place. A refusal or
/// a failed landing restores the previous install before returning — a
/// failed replace never leaves the name uninstalled or half-written — and a
/// landed replace sweeps the parked copy best-effort.
///
/// The land-time re-decision exists because [`check_target`]'s snapshot was
/// taken before the fetch: a concurrent install of the same name is
/// invisible to it, so the package actually parked aside is the authority
/// at the moment of the clobber ([`checks::recheck_parked_install`]).
fn land_replacing(
    target: &Path,
    staged: &Path,
    name: &str,
    source: &InstallSource,
    force: bool,
) -> Result<(), InstallError> {
    // A marked install of the same name replaces cleanly (§3: reinstall or
    // version change); `--force` clobbers a foreign target. The replace is
    // atomic: the existing package is renamed aside (a parked-aside sibling
    // under its own prefix, so the sweep never mistakes it for staging
    // residue) before the new one lands, and restored if the landing fails.
    let extensions_dir = target
        .parent()
        .expect("the install target is a direct child of the extensions dir");
    let aside = crate::staging::replaced_dir(extensions_dir);
    std::fs::rename(target, &aside)
        .map_err(|e| io("replacing the installed package", target, &e))?;
    // Re-decide the clobber against the package actually parked aside:
    // `check_target` looked before the fetch, so a concurrent install of
    // the same name can have landed in between. A tier-crossing replace
    // refuses even though the pre-fetch snapshot allowed it.
    if let Err(refusal) = recheck_parked_install(name, target, &aside, source.tier(), force) {
        // The refusal's restore is the shared one: put the parked original
        // back, and a restore that fails too strands it (the text says
        // where the only copy now lives).
        crate::staging::restore_parked(target, &aside, |restore| {
            InstallError::Runtime(format!(
                "refusing to replace {} ({refusal}); restoring the previous install from \
                 {} failed too ({restore}) — the previous install is stranded there, \
                 recover it manually",
                target.display(),
                aside.display()
            ))
        })?;
        return Err(refusal);
    }
    // The landing is the shared one (staging::land_parked): a failed
    // landing puts the parked original back before refusing, a restore
    // that fails too strands it, and a landed replace sweeps the parked
    // copy best-effort.
    crate::staging::land_parked(
        target,
        staged,
        &aside,
        |e| io("landing the package", target, &e),
        |e, restore| {
            InstallError::Runtime(format!(
                "landing the package {} failed ({e}); restoring the previous \
                 install from {} failed too ({restore}) — the previous install \
                 is stranded there, recover it manually",
                target.display(),
                aside.display()
            ))
        },
    )
}

/// The package's target directory: `<extensions_dir>/<name>` — the name
/// the registry's walk keys the package directory by.
fn package_target(extensions_dir: &Path, name: &str) -> PathBuf {
    extensions_dir.join(name)
}

/// One IO failure, named with the path and cause.
fn io(what: &str, path: &Path, e: &std::io::Error) -> InstallError {
    InstallError::Runtime(format!("{what} {}: {e}", path.display()))
}

/// The one line the verb prints on a landed install (§6: "prints one line
/// naming the package, its version, and the URL it came from" — realized
/// per tier, §7's source reference; the version is the staged manifest's
/// own, read back after the rename so the line reports what landed).
/// Every package-authored fragment — the name the manifest agreed to, the
/// version, the provenance source — passes through the shared sanitizer,
/// the `print_error` doctrine applied to the verb's own output line.
fn loud_line(name: &str, version: &str, provenance: &Provenance) -> String {
    let source = match provenance.tier {
        Tier::FirstParty => format!("the signed channel (tag {})", provenance.source),
        Tier::GithubPin => match &provenance.pin {
            Some(pin) => format!("{} (commit {})", provenance.source, pin.commit),
            None => provenance.source.clone(),
        },
        Tier::ThirdParty | Tier::Local => provenance.source.clone(),
    };
    format!(
        "installed {} extension {} {} from {}",
        provenance.tier.as_str(),
        sanitize_error_text(name),
        sanitize_error_text(version),
        sanitize_error_text(&source)
    )
}

// The install flow's tests live in the `install/tests/` dir-module (the
// pattern the extension tree's `mcp/tests/` module uses) so this file
// stays under the repo's 1k-line file budget.
#[cfg(test)]
mod tests;
