//! `upgrade` command (ADR-0069, ADR-0250, ADR-0358): read the latest release
//! from the static `releases.json` published on the project website, download
//! the platform binary together with the `SHA256SUMS` manifest and its
//! `SHA256SUMS.sig` signature, verify the manifest against the pinned signing
//! key, bind the version inside that signed manifest to the release the
//! metadata requested (ADR-0457), atomically replace the running binary, then
//! refresh the installed agent-skill bundle. The `SHA256SUMS` request probes
//! the non-API release-download route first; only a typed 404 there, with `GITHUB_TOKEN`
//! set, falls back to the authenticated GitHub API asset route for all three
//! assets (ADR-0358).
//!
//! The replace's platform scope matches the ADR-0068 installer
//! (macOS/Linux): a running Windows `.exe` cannot replace itself, so a
//! Windows upgrade gets a not-supported error pointing at the PowerShell
//! installer (ADR-0586). `--check` reads no asset and replaces nothing, so
//! it reports current-vs-latest on every platform, Windows included.
//!
//! Every network operation lives behind
//! [`releases_json::fetch_latest_release`] and the [`download_route`] probe
//! (with the API route's own fetches in [`api_assets`]); the one HTTP client
//! every asset download goes through is [`http`]. Everything else (metadata
//! validation, version comparison, `SHA256SUMS` parsing, asset naming,
//! download-URL building, the route decision, the skill-refresh decision) is
//! a pure function with unit tests that never open a socket.
//! `RELATE_RELEASES_JSON_URL`, `RELATE_RELEASE_DOWNLOAD_URL` and
//! `RELATE_GITHUB_API_URL` override the metadata URL, the direct-route
//! base and the GitHub API base through the one [`seam`] policy helper (its
//! core is [`env_nonempty`], shared with the `GITHUB_TOKEN` read), so tests
//! can point the fetches at a fixture server.

use std::path::{Path, PathBuf};

use crate::parse_error::ParseError;
use crate::release_channel::download_route::{probe_route_at, release_download_base};
use crate::release_channel::signing::{verify, SigningError};
use crate::release_channel::{env_nonempty, platform_for, sha256_hex, Platform};

/// The signed `SHA256SUMS` manifest contract (ADR-0115, ADR-0457): the
/// version header, its binding to the requested release, and the entry
/// lookup.
mod manifest;

/// The `releases.json` document contract (ADR-0250 §2): metadata validation,
/// metadata-URL selection, download-URL selection, and the one metadata
/// fetch.
mod releases_json;

use manifest::{bind_signed_version, legacy_ceiling_label, sha256sums_entry, ManifestBinding};
use releases_json::fetch_latest_release;

/// Single-sourced from the crate's `Cargo.toml` (`version.workspace = true`).
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The manifest asset every upgrade downloads (ADR-0115), and its signature:
/// the two fixed names of the three assets, the third being the platform
/// binary. Every route, lookup and download consults these, never a re-typed
/// literal.
const SUMS_ASSET: &str = "SHA256SUMS";
const SUMS_SIG_ASSET: &str = "SHA256SUMS.sig";

/// The documented reinstall one-liner (README / install.sh header), offered
/// whenever `upgrade` cannot proceed: release discovery failed (ADR-0250 §4),
/// the signed manifest's version did not bind to the requested release
/// (ADR-0457 §2), or the in-place replace failed. A Windows build offers the
/// PowerShell installer's one-liner instead (ADR-0586): `install.sh` installs
/// nothing there.
#[cfg(not(windows))]
const INSTALL_ONE_LINER: &str = "curl -sSfL https://relatedb.ai/install.sh | bash";
#[cfg(windows)]
const INSTALL_ONE_LINER: &str = WINDOWS_INSTALL_ONE_LINER;

/// The PowerShell installer's documented one-liner (README, `install.ps1`
/// header; ADR-0586), in the `powershell -c` form that runs from any shell
/// — PowerShell, `cmd`, or Git Bash. Spelled once: the Windows refusal
/// names it on every host, and a Windows build's [`INSTALL_ONE_LINER`] is
/// it.
const WINDOWS_INSTALL_ONE_LINER: &str =
    "powershell -ExecutionPolicy ByPass -c \"irm https://relatedb.ai/install.ps1 | iex\"";

/// Options for `relate upgrade`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpgradeOpts {
    /// Auto-accept the post-upgrade skill refresh (`--yes`).
    pub yes: bool,
    /// Report current-vs-latest only; download and modify nothing (`--check`).
    pub check: bool,
}

/// Parse the argument tail after the `upgrade` (or `update`) verb: exactly
/// `[--yes] [--check]` in any order. Pure — returns the usage-error message
/// instead of exiting, so the grammar is unit-testable; `main` turns `Err`
/// into the hand-rolled parser's exit-2 convention.
pub fn parse_upgrade_args(rest: &[String]) -> Result<UpgradeOpts, ParseError> {
    let mut opts = UpgradeOpts {
        yes: false,
        check: false,
    };
    for arg in rest {
        match arg.as_str() {
            "--yes" => opts.yes = true,
            "--check" => opts.check = true,
            other => return Err(ParseError::unexpected("upgrade", other)),
        }
    }
    Ok(opts)
}

/// Run `relate upgrade [--yes] [--check]`. Returns the process exit
/// code; the caller (main) owns `std::process::exit`.
pub fn run(opts: UpgradeOpts) -> i32 {
    match run_inner(&opts) {
        Ok(()) => 0,
        Err(e) => {
            // Single-line message; route through the shared sanitizer like
            // every other interpolated stderr diagnostic.
            crate::print_error(e.stderr_message());
            1
        }
    }
}

fn run_inner(opts: &UpgradeOpts) -> Result<(), UpgradeError> {
    if opts.check {
        return run_check();
    }
    // Only the replace needs a supported os/arch pair; an unsupported one
    // refuses here, before any network request.
    let platform = platform_support()?;

    // GITHUB_TOKEN is read exactly once and used only as an
    // `Authorization: Bearer` header value on the asset requests — never on
    // the releases.json request (ADR-0250 §5) — and it is what selects the
    // API asset route when the direct probe answers 404 (ADR-0358 §3). It is
    // never printed. Parity with install.sh (ADR-0068): needed while the
    // repository is internal. A set but non-UTF-8 value aborts naming the
    // variable instead of reading as tokenless.
    let token = env_nonempty("GITHUB_TOKEN")?;

    // Release discovery reads the website's releases.json (ADR-0250): no
    // token, no retry, no GitHub API fallback. The version triple was proven
    // and the tag sanitized at the parse boundary, so nothing below
    // re-parses or re-strips it.
    let (release, latest_version) = fetch_latest_release()?;
    let tag = release.tag.as_str();
    let latest = strip_v_prefix(tag);
    if !version_is_newer(latest_version, VERSION) {
        println!("relate {VERSION} is up to date (latest release: {latest})");
        return Ok(());
    }

    let asset = asset_name(&platform);
    release.require_asset(&asset)?;
    release.require_asset(SUMS_ASSET)?;
    release.require_asset(SUMS_SIG_ASSET)?;

    println!("upgrading relate {VERSION} -> {latest} ({asset})");
    // Cheap first: the manifest and its signature are small; the binary is
    // the 512 MiB-capped download and must not start until the manifest is
    // authenticated. That one manifest request doubles as the route probe
    // (ADR-0358 §1): the route it selects serves all three assets. The
    // needed set is the three upgrade downloads, probe asset included.
    // The direct-route base is resolved here (the env seam, ADR-0358 §8)
    // and threaded into the probe — the one entry point both channel
    // flows share.
    let direct_base = release_download_base()?;
    let needed = [SUMS_ASSET, SUMS_SIG_ASSET, asset.as_str()];
    let probe = probe_route_at(
        crate::release_channel::REPO,
        &direct_base,
        tag,
        SUMS_ASSET,
        &needed,
        token.as_deref(),
        Some(&format!("reinstall manually: {INSTALL_ONE_LINER}")),
    )?;
    if let Some(note) = &probe.note {
        // ADR-0358 §6: the one fallback note, through the shared sanitized
        // stderr writer like every other interpolated diagnostic. The route
        // module returns the note instead of printing it, so all console
        // output stays here.
        crate::print_error(note);
    }
    let sig_bytes = probe.route.download(SUMS_SIG_ASSET)?;
    // Authenticate the manifest before trusting any hash it carries
    // (ADR-0115): the signature must verify over the exact SHA256SUMS bytes.
    verify_sums_signature(&probe.probe_bytes, &sig_bytes)?;
    let sums = String::from_utf8(probe.probe_bytes)
        // release_json: the manifest did not have the expected shape.
        .map_err(|e| UpgradeError::new(format!("SHA256SUMS is not UTF-8: {e}")))?;
    // Bind the authenticated manifest to the release the metadata asked for
    // (ADR-0457 §2), before trusting any entry or paying for the binary. A
    // legacy manifest gets a warning; it was only accepted because the
    // requested version predates the header (ADR-0457 §3).
    match bind_signed_version(&sums, tag, latest_version)? {
        ManifestBinding::Authenticated => {}
        ManifestBinding::Legacy => crate::print_error(format!(
            "warning: SHA256SUMS from {tag} has no signed version header (legacy manifest, \
             accepted only at or below {})",
            legacy_ceiling_label()
        )),
    }

    let expected = sha256sums_entry(&sums, &asset).ok_or_else(|| {
        // missing_checksum: SHA256SUMS has no line for the asset.
        UpgradeError::new(format!("SHA256SUMS from {tag} has no entry for {asset}"))
    })?;
    // The manifest is authenticated and carries an entry for this asset;
    // only now is the binary download worth paying for.
    let binary = probe.route.download(&asset)?;
    let actual = sha256_hex(&binary);
    if actual != expected {
        // Change nothing: the running binary stays untouched.
        // checksum_mismatch: downloaded bytes do not match the SHA256SUMS entry.
        return Err(UpgradeError::new(format!(
            "checksum verification failed for {asset} (expected {expected}, got {actual}); not upgrading"
        )));
    }

    let exe = replace_current_exe(&binary)?;
    println!("upgraded relate {VERSION} -> {latest} at {}", exe.display());

    crate::skill::refresh::skill_refresh(opts.yes, &exe);
    Ok(())
}

/// Error from the upgrade flow whose stderr text is user-facing. Every
/// failure collapses to one message; the exit code is always 1, so a plain
/// string suffices. The failure-site label lives at the call site, as a
/// comment on each `UpgradeError::new`.
#[derive(Debug)]
struct UpgradeError(String);

impl UpgradeError {
    fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }

    /// The one cannot-proceed pointer (ADR-0250 §4, ADR-0457 §2): the
    /// failure detail plus the documented reinstall one-liner, in the
    /// pinned sentence shape every abort shares — the single source of
    /// the `; reinstall manually instead: …` suffix.
    fn reinstall(detail: impl std::fmt::Display) -> Self {
        Self::new(format!(
            "{detail}; reinstall manually instead: {INSTALL_ONE_LINER}"
        ))
    }

    /// The exact text `run` writes to stderr before returning exit code 1.
    fn stderr_message(&self) -> String {
        format!("Error: upgrade: {}", self.0)
    }
}

/// The bare message text, no prefix: how a child module rewraps an
/// `UpgradeError` (e.g. `releases_json` adding the metadata URL) without
/// reaching into the tuple field.
impl std::fmt::Display for UpgradeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The shared release-channel machinery's failure, wrapped with this flow's
/// identity: the bare text (a download-route abort, an env-seam refusal) is
/// unchanged, only the `Error: upgrade:` prefix is added at the exit.
impl From<crate::release_channel::ChannelError> for UpgradeError {
    fn from(err: crate::release_channel::ChannelError) -> Self {
        UpgradeError(err.to_string())
    }
}

/// `relate upgrade --check`: report current-vs-latest and the advice, then
/// stop. It downloads no asset and replaces nothing, so it runs on every
/// platform (ADR-0586), and an unsupported pair's refusal becomes its
/// advice rather than its error.
fn run_check() -> Result<(), UpgradeError> {
    let (release, latest_version) = fetch_latest_release()?;
    let label = strip_v_prefix(&release.tag);
    let newer = version_is_newer(latest_version, VERSION);
    println!("current version: {VERSION}");
    println!("latest release:  {label}");
    println!("{}", check_verdict(&platform_support(), newer, label));
    Ok(())
}

/// The `--check` report's last line: up to date, or the upgrade advice —
/// `relate upgrade` where the replace is supported, else the platform's
/// refusal (on Windows, the PowerShell installer one-liner), so the report
/// never advises a command that would refuse.
fn check_verdict(platform: &Result<Platform, UpgradeError>, newer: bool, latest: &str) -> String {
    match (newer, platform) {
        (false, _) => "relate is up to date".to_string(),
        (true, Ok(_)) => format!("upgrade available: run `relate upgrade` to install {latest}"),
        (true, Err(refusal)) => format!("upgrade available: {latest}; {refusal}"),
    }
}

/// The host platform for this flow, or why self-upgrade cannot run here:
/// the shared release-channel table (ADR-0068) decides the os/arch pair —
/// the same vocabulary the extension index's per-platform rows speak
/// (ADR-0431 §1), so the os/arch picking is one shared table, not one per
/// flow — and this flow renders its own refusal over the table's `None`:
/// the Windows arm points at the PowerShell installer (ADR-0586), every
/// other unsupported pair at the README's install section.
fn platform_support() -> Result<Platform, UpgradeError> {
    platform_support_for(std::env::consts::OS, std::env::consts::ARCH)
}

/// [`platform_support`] over a given os/arch pair, pure so the refusal
/// texts are unit-testable from any host. The four supported pairs and
/// the `None` vocabulary are pinned once, in
/// `release_channel::platform_for`'s tests — the one table.
fn platform_support_for(os: &str, arch: &str) -> Result<Platform, UpgradeError> {
    match platform_for(os, arch) {
        Some(platform) => Ok(platform),
        // unsupported_platform: self-upgrade on this OS/arch is out of scope
        // (ADR-0069 §6) — Windows gets the PowerShell installer pointer
        // (ADR-0586), which installs the verified relate-windows-amd64.exe.
        None if os == "windows" => Err(UpgradeError::new(format!(
            "self-upgrade is not supported on Windows (a running .exe cannot replace itself); \
             reinstall with the PowerShell installer to upgrade: {WINDOWS_INSTALL_ONE_LINER}"
        ))),
        // unsupported_platform: an OS/arch pair with no release asset.
        None => Err(UpgradeError::new(format!(
            "self-upgrade is not supported on {os}/{arch} (supported: macOS and Linux, \
             amd64 and arm64); see the README for manual downloads: \
             https://github.com/relatedb/relate#install"
        ))),
    }
}

/// The release asset name for a supported platform, e.g.
/// `relate-darwin-arm64`. Windows never reaches here: `platform_for`
/// rejects it before any download is attempted (the refusal points at the
/// PowerShell installer, which downloads the `.exe` asset itself).
fn asset_name(platform: &Platform) -> String {
    format!("relate-{}-{}", platform.os, platform.arch)
}

/// Strip the leading `v` from a release tag (`v0.5.0` -> `0.5.0`).
fn strip_v_prefix(tag: &str) -> &str {
    tag.strip_prefix('v').unwrap_or(tag)
}

/// Parse an `X.Y.Z` numeric triple by hand (no semver crate, ADR-0069 §3).
/// Anything else — missing or extra components, non-numeric text,
/// pre-release suffixes — is `None`.
fn parse_version_triple(s: &str) -> Option<(u64, u64, u64)> {
    let mut parts = s.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// Is the `latest` triple (proven at the `releases.json` parse boundary)
/// newer than `current`, the crate's own version? Numeric triples compare
/// lexicographically. An unparseable `current` — never the case for the
/// compile-time `CARGO_PKG_VERSION` — fails safe: no self-upgrade from a
/// build whose own version is unknown.
fn version_is_newer(latest: (u64, u64, u64), current: &str) -> bool {
    match parse_version_triple(strip_v_prefix(current)) {
        Some(current) => latest > current,
        None => false,
    }
}

/// Verify the OpenSSH SSHSIG `sig_pem` over the exact `sums` bytes
/// (ADR-0115): the shared signing root (`release_channel::signing`) runs
/// the verification against the one pinned release key; this wrapper maps
/// the typed failure onto the upgrade's pinned wording, which names the
/// artifact that failed (`SHA256SUMS.sig`). Pure: no network, so the whole
/// authentication path is unit-testable. The hash algorithm is carried
/// inside the signature (`ssh-keygen`'s default SHA-512).
fn verify_sums_signature(sums: &[u8], sig_pem: &[u8]) -> Result<(), UpgradeError> {
    verify(sums, sig_pem).map_err(sums_signing_failure)
}

/// The typed-signing-failure → upgrade-wording mapping (ADR-0431: every
/// failure path has a pinned, tested text): each arm names the artifact
/// the upgrade verified. The verify mechanics (valid signature, tampered
/// document, wrong key, malformed blob) are pinned against fixture keys in
/// `release_channel::signing`'s tests; the real-key path is pinned end to
/// end by `e2e_upgrade`'s committed `fixtures/release/` pair.
fn sums_signing_failure(err: SigningError) -> UpgradeError {
    match err {
        // bad_signature: the pinned key itself failed to parse.
        SigningError::InvalidKey(e) => {
            UpgradeError::new(format!("release signing key is invalid: {e}"))
        }
        // bad_signature: the SHA256SUMS.sig asset is not a valid SSHSIG.
        SigningError::MalformedSignature(e) => {
            UpgradeError::new(format!("SHA256SUMS.sig is not a valid SSHSIG: {e}"))
        }
        // bad_signature: the signature did not verify against the pinned
        // key (wrong key, tampered manifest, or wrong namespace).
        SigningError::VerificationFailed(e) => {
            UpgradeError::new(format!("SHA256SUMS.sig verification failed: {e}"))
        }
    }
}

/// Stage `bytes` at a non-predictable temp path next to the running binary and
/// rename over it via the shared [`crate::atomic_file`] helper: the staging
/// path sits in `current_exe()`'s directory (guaranteeing the same filesystem),
/// the staged binary is created `0755` (unix) and with `create_new`, so the
/// rename is atomic and an interrupted upgrade never leaves a partial binary
/// at the final path. Returns the replaced binary's path.
fn replace_current_exe(bytes: &[u8]) -> Result<PathBuf, UpgradeError> {
    let exe = std::env::current_exe()
        // replace: resolving the running binary's path failed.
        .map_err(|e| UpgradeError::new(format!("resolving current exe: {e}")))?;
    crate::atomic_file::write_atomic(&exe, bytes, 0o755)
        .map_err(|e| replace_err("replacing", &exe, e))?;
    Ok(exe)
}

/// A replace failure is usually a permissions problem on the install
/// directory; point at the documented reinstall path.
fn replace_err(what: &str, path: &Path, e: std::io::Error) -> UpgradeError {
    // replace: staging or renaming over the running binary failed.
    UpgradeError::reinstall(format!("{what} {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args;

    #[test]
    fn parse_upgrade_args_reads_the_two_flags_in_any_order() {
        assert_eq!(
            parse_upgrade_args(&args(&[])).unwrap(),
            UpgradeOpts {
                yes: false,
                check: false
            }
        );
        assert_eq!(
            parse_upgrade_args(&args(&["--check", "--yes"])).unwrap(),
            UpgradeOpts {
                yes: true,
                check: true
            }
        );
    }

    #[test]
    fn parse_upgrade_args_rejects_anything_else() {
        assert_eq!(
            parse_upgrade_args(&args(&["--bogus"]))
                .unwrap_err()
                .to_string(),
            "unexpected argument to upgrade: --bogus"
        );
        assert_eq!(
            parse_upgrade_args(&args(&["now"])).unwrap_err().to_string(),
            "unexpected argument to upgrade: now"
        );
    }

    #[test]
    fn version_triple_parses_x_y_z() {
        assert_eq!(parse_version_triple("0.4.0"), Some((0, 4, 0)));
        assert_eq!(parse_version_triple("1.22.333"), Some((1, 22, 333)));
        assert_eq!(parse_version_triple("0.4"), None);
        assert_eq!(parse_version_triple("0.4.0.1"), None);
        assert_eq!(parse_version_triple("0.4.0-beta"), None);
        assert_eq!(parse_version_triple("a.b.c"), None);
    }

    #[test]
    fn strip_v_prefix_strips_only_leading_v() {
        assert_eq!(strip_v_prefix("v0.5.0"), "0.5.0");
        assert_eq!(strip_v_prefix("0.5.0"), "0.5.0");
        assert_eq!(strip_v_prefix("vv0.5.0"), "v0.5.0");
    }

    #[test]
    fn version_is_newer_orders_triples_numerically() {
        // newer / older / equal
        assert!(version_is_newer((0, 5, 0), "0.4.0"));
        assert!(!version_is_newer((0, 4, 0), "0.5.0"));
        assert!(!version_is_newer((0, 4, 0), "0.4.0"));
        // numeric, not string, ordering per component
        assert!(version_is_newer((0, 10, 0), "0.9.9"));
        assert!(!version_is_newer((2, 0, 0), "10.0.0"));
        // an unparseable current version fails safe, never a panic
        assert!(!version_is_newer((0, 5, 0), "latest"));
    }

    #[test]
    fn version_is_newer_strips_v_prefix() {
        assert!(version_is_newer((0, 5, 0), "v0.4.0"));
        assert!(version_is_newer((0, 5, 0), "0.4.0"));
        assert!(!version_is_newer((0, 4, 0), "v0.4.0"));
    }

    #[test]
    fn asset_name_maps_platforms() {
        let p = |os, arch| Platform { os, arch };
        assert_eq!(asset_name(&p("darwin", "arm64")), "relate-darwin-arm64");
        assert_eq!(asset_name(&p("linux", "amd64")), "relate-linux-amd64");
    }

    #[test]
    fn platform_support_resolves_the_host_through_the_shared_table() {
        // The four supported pairs are pinned once, over the shared table
        // (`release_channel::platform_for`'s tests); here the delegation is
        // pinned: a supported host resolves, an unsupported one renders
        // this flow's own refusal texts.
        assert_eq!(
            platform_support_for("macos", "aarch64").unwrap(),
            Platform {
                os: "darwin",
                arch: "arm64"
            }
        );
        assert_eq!(
            platform_support_for("linux", "x86_64").unwrap(),
            Platform {
                os: "linux",
                arch: "amd64"
            }
        );
    }

    #[test]
    fn platform_support_for_windows_is_not_supported() {
        // Any Windows arch is unsupported; the message must point at the
        // PowerShell installer, never at a README anchor that can rot.
        for arch in ["x86_64", "aarch64"] {
            let err = platform_support_for("windows", arch).unwrap_err();
            let msg = err.stderr_message();
            assert!(
                msg.contains("not supported"),
                "missing not-supported: {msg}"
            );
            assert!(
                msg.contains(WINDOWS_INSTALL_ONE_LINER),
                "missing the install.ps1 one-liner: {msg}"
            );
            assert!(!msg.contains('#'), "no README anchor: {msg}");
        }
    }

    #[test]
    fn windows_one_liner_names_the_powershell_installer() {
        assert!(WINDOWS_INSTALL_ONE_LINER.contains("https://relatedb.ai/install.ps1"));
        // A Windows build's reinstall advice is the PowerShell one-liner;
        // every other build keeps install.sh's.
        if cfg!(windows) {
            assert_eq!(INSTALL_ONE_LINER, WINDOWS_INSTALL_ONE_LINER);
        } else {
            assert!(INSTALL_ONE_LINER.contains("https://relatedb.ai/install.sh"));
        }
    }

    #[test]
    fn check_verdict_never_advises_a_refused_upgrade() {
        let supported = Ok(Platform {
            os: "linux",
            arch: "amd64",
        });
        let windows = platform_support_for("windows", "x86_64");
        assert_eq!(
            check_verdict(&supported, false, "1.2.3"),
            "relate is up to date"
        );
        assert_eq!(
            check_verdict(&windows, false, "1.2.3"),
            "relate is up to date"
        );
        assert_eq!(
            check_verdict(&supported, true, "1.2.3"),
            "upgrade available: run `relate upgrade` to install 1.2.3"
        );
        let advice = check_verdict(&windows, true, "1.2.3");
        assert!(
            advice
                .starts_with("upgrade available: 1.2.3; self-upgrade is not supported on Windows"),
            "{advice}"
        );
        assert!(advice.contains(WINDOWS_INSTALL_ONE_LINER), "{advice}");
        assert!(!advice.contains("run `relate upgrade`"), "{advice}");
    }

    #[test]
    fn platform_support_for_any_other_unsupported_pair_names_the_pair() {
        // The generic refusal names the os/arch pair and the supported set
        // — the shared table's `None`, rendered as this flow's wording.
        let err = platform_support_for("freebsd", "x86_64").unwrap_err();
        let msg = err.stderr_message();
        assert!(
            msg.contains("not supported on freebsd/x86_64"),
            "missing the pair: {msg}"
        );
        assert!(
            msg.contains("supported: macOS and Linux, amd64 and arm64"),
            "missing the supported set: {msg}"
        );
        assert!(msg.contains("README"), "missing README pointer: {msg}");
        assert!(
            msg.contains("https://github.com/relatedb/relate#install"),
            "the README pointer must name the Install section: {msg}"
        );
    }

    #[test]
    fn sha256_hex_known_answers() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    /// The flow-level signature mapping (ADR-0431: every failure path has a
    /// pinned, tested text): the shared signing root's typed failures render
    /// through the upgrade's wording, which names the artifact
    /// (`SHA256SUMS.sig`). The verify mechanics (valid signature, tampered
    /// document, wrong key, malformed blob) are pinned against fixture keys
    /// in `release_channel::signing`'s tests; the real-key path is pinned
    /// end to end by `e2e_upgrade`'s committed `fixtures/release/` pair.
    #[test]
    fn the_signing_failure_mapping_names_the_artifact() {
        // A parse failure from the ssh-key crate is all each arm needs; the
        // variant, not the cause, is what the mapping reads.
        for (case, err, needle) in [
            (
                "invalid key",
                SigningError::InvalidKey(
                    ssh_key::PublicKey::from_openssh("not a key").unwrap_err(),
                ),
                "release signing key is invalid",
            ),
            (
                "malformed signature",
                SigningError::MalformedSignature(
                    ssh_key::PublicKey::from_openssh("not a key").unwrap_err(),
                ),
                "SHA256SUMS.sig is not a valid SSHSIG",
            ),
            (
                "verification failed",
                SigningError::VerificationFailed(
                    ssh_key::PublicKey::from_openssh("not a key").unwrap_err(),
                ),
                "SHA256SUMS.sig verification failed",
            ),
        ] {
            let msg = sums_signing_failure(err).stderr_message();
            assert_eq!(msg.lines().count(), 1, "case {case}: one line: {msg:?}");
            assert!(msg.contains(needle), "case {case}: {msg:?}");
        }
    }
}
