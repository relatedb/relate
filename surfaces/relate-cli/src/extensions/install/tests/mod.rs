//! The install flow's tests (ADR-0431 §3-§7): the name checks, the
//! target-decision matrix (§3's clean replace reconciled with §7's
//! collision refusals), the `--from` landing — staged, marked, renamed,
//! loud-lined — and the signed channel's post-gate steps (§4 steps 2-4)
//! driven over the fixture index against a loopback route: the channel
//! e2e stops at the signature gate (the pinned release key never verifies
//! a fixture-key signature, and the ADR pins no key seam), so the steps
//! behind it — the row resolution, the route download, the signed-row
//! checksum, the unpack — are unit-driven here, the fixture signer's pair
//! carrying the verification shape (the TEST_SIGNER pattern). The tarball
//! paths' member contract is pinned in `unpack.rs`'s tests and the fetch
//! paths end to end in `tests/e2e_extensions.rs`; no other test here
//! opens a socket.

use std::path::{Path, PathBuf};

use super::sources::install_from_index;
use super::*;
use crate::extensions::manifest::Manifest;
use crate::extensions::provenance::MARKER_FILE;
use crate::extensions::registry::ActionRegistry;
use crate::extensions::source::{Approval, GithubSpec};
use crate::telemetry::env_of;

/// A well-formed command manifest for `name` with `version` — the common
/// fixture: the host's protocol, a top-level `run.sh` executable. The
/// one builder ([`manifest_with`]) spells the schema; every other
/// fixture spelling delegates here.
fn manifest_text(name: &str, version: &str) -> String {
    manifest_with(
        name,
        version,
        "run.sh",
        relate_wire::mcp::MCP_PROTOCOL_VERSION,
    )
}

/// The one manifest fixture builder: any executable path (the subfolder
/// and escaping cases) and any protocol (the foreign-protocol refusal) —
/// the schema's required fields live at this one site.
fn manifest_with(name: &str, version: &str, executable: &str, protocol: &str) -> String {
    format!(
        r#"{{"kind":"command","name":"{name}","version":"{version}","protocol":"{protocol}","flags":[],"commit":"per-batch","executable":"{executable}"}}"#
    )
}

/// One fixture world: `$XDG_CONFIG_HOME` pinned to a tempdir, so the
/// registry's injectable walk and the install's extensions dir view the
/// same tree (`<tmp>/relate/extensions/`).
struct World {
    tmp: tempfile::TempDir,
}

impl World {
    fn new() -> Self {
        Self {
            tmp: tempfile::tempdir().unwrap(),
        }
    }

    /// The config dir both the extensions dir and the trust store live
    /// under (`<tmp>/relate`) — the one spelling of the layout.
    fn config_dir(&self) -> PathBuf {
        self.tmp.path().join("relate")
    }

    /// The extensions dir both the registry walk and the install target —
    /// the canonical name ([`crate::extensions::registry::EXTENSIONS_DIR`]),
    /// never a re-spelled literal.
    fn extensions_dir(&self) -> PathBuf {
        let dir = self
            .config_dir()
            .join(crate::extensions::registry::EXTENSIONS_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The config dir's trust-store path — the canonical join
    /// ([`crate::extensions::trust::trust_path`]), the one the install
    /// verbs, `list --trust` and `remove --forget-pin` share.
    fn trust_file(&self) -> PathBuf {
        crate::extensions::trust::trust_path(&self.config_dir())
    }

    /// The recorded pin for `name`, when the store has one.
    fn recorded_pin(&self, name: &str) -> Option<crate::extensions::provenance::Pin> {
        crate::extensions::trust::TrustStore::load(&self.trust_file())
            .unwrap()
            .get(name)
            .cloned()
    }

    /// The registry over this world's tree.
    fn registry(&self) -> ActionRegistry {
        let xdg = self.tmp.path().to_str().expect("the world's path is UTF-8");
        ActionRegistry::load_with(&env_of(&[("XDG_CONFIG_HOME", xdg)])).unwrap()
    }

    /// Install a package directory (manifest + executable) under this
    /// world's extensions dir, optionally marked with `provenance`.
    fn install_fixture(&self, dir_name: &str, manifest: &str, provenance: Option<&Provenance>) {
        let dir = self.extensions_dir().join(dir_name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("extension.json"), manifest).unwrap();
        std::fs::write(dir.join("run.sh"), "#!/bin/sh\n").unwrap();
        if let Some(provenance) = provenance {
            provenance.write(&dir).unwrap();
        }
    }

    /// A `--from` source package directory, outside the extensions dir
    /// (the local trust act names any directory).
    fn source_package(&self, name: &str, version: &str) -> PathBuf {
        let dir = self.tmp.path().join("src").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("extension.json"), manifest_text(name, version)).unwrap();
        std::fs::write(dir.join("run.sh"), "#!/bin/sh\n").unwrap();
        // The `--from` checks judge the source's write bits, so the fixture
        // pins its files' modes instead of inheriting the test runner's
        // umask (a 002 umask would otherwise make every fixture
        // group-writable).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                dir.join("extension.json"),
                std::fs::Permissions::from_mode(0o644),
            )
            .unwrap();
            std::fs::set_permissions(dir.join("run.sh"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        dir
    }
}

/// The one `InstallArgs` literal the fixtures build: every flow entry
/// delegates here, so a field the grammar adds is threaded once.
fn install_args(name: &str, source: InstallSource, approval: Option<Approval>) -> InstallArgs {
    InstallArgs {
        name: name.to_string(),
        source,
        force: false,
        approval,
    }
}

/// The one `install_into` adapter the fixtures run: the flow's real entry
/// point is `run_install` (real env), so these tests drive `install_into`
/// against the fixture tree — every flow entry differs only by the source
/// it names and the route it injects.
fn install_with(
    world: &World,
    route: GithubResolver<'_>,
    args: InstallArgs,
) -> Result<(), InstallError> {
    install_into(
        &world.extensions_dir(),
        &world.trust_file(),
        route,
        &args,
        &world.registry(),
    )
}

/// `extension install <name> --from DIR` over a fixture world.
fn install_from(world: &World, name: &str, source: &Path) -> Result<(), InstallError> {
    install_with(
        world,
        &GithubRoute::from_env,
        install_args(
            name,
            InstallSource::From(source.display().to_string()),
            None,
        ),
    )
}

/// `extension install <name> --from URL` over a fixture world: the remote
/// form's entry — the loopback route the URL points at is each test's own
/// fixture server.
fn install_from_remote(world: &World, name: &str, url: &str) -> Result<(), InstallError> {
    install_with(
        world,
        &GithubRoute::from_env,
        install_args(name, InstallSource::FromUrl(url.to_string()), None),
    )
}

/// `extension install <name> --github …` through the real flow, with the
/// loopback API and download bases injected (the `load`/`load_with` shape
/// the channel's post-gate steps already use).
fn install_github(
    world: &World,
    name: &str,
    spec: GithubSpec,
    approval: Option<Approval>,
    api_base: &str,
    download_base: &str,
) -> Result<(), InstallError> {
    let route = GithubRoute::at(api_base, download_base);
    install_with(
        world,
        &|_| Ok(route.clone()),
        install_args(name, InstallSource::Github(spec), approval),
    )
}

/// The error of an install that must fail.
fn refusal(result: Result<(), InstallError>) -> InstallError {
    match result {
        Err(err) => err,
        Ok(()) => panic!("expected an install refusal"),
    }
}

/// The host platform, through the one shared table — the pair the resolved
/// row (and its asset name) must agree with, read back instead of
/// re-tabled.
fn host() -> crate::release_channel::Platform {
    crate::release_channel::host_platform().expect("the test hosts are darwin/linux arm64/amd64")
}

/// A staging directory under a fixture world, empty and ready.
fn staging_dir(world: &World) -> PathBuf {
    let staged = world.tmp.path().join("stage");
    std::fs::create_dir_all(&staged).unwrap();
    staged
}

mod channel;
mod fixtures;
mod from_dir;
mod from_url;
mod github;
mod target;
mod trust;
mod url;
