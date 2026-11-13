//! End-to-end tests for the `upgrade` / `update` positional lifecycle
//! commands (ADR-0069): `relate upgrade [--yes] [--check]`.
//!
//! The argument-grammar tests are validated before any network access and
//! never open a socket. The release-discovery tests point the metadata URL
//! override ([`ReleasesFixture::envs`]) at a loopback fixture (ADR-0250 §1),
//! so they exercise the real fetch/parse path without leaving the machine. The
//! download-route suite lives in `e2e_upgrade_route.rs`; the fixtures both
//! share live in `support::releases`.

#![cfg(feature = "upgrade")]

mod support;

use relate_test_support::loopback::header_values;
use support::releases::{ReleasesFixture, NEWER_RELEASES_JSON};
use support::{run_cli, tempdir, CliRunOptions as Opts};

/// The committed `releases.json` fixture, served as the website's document.
const RELEASES_JSON: &str = include_str!("fixtures/releases.json");

/// The release-workflow publisher's committed expected output
/// (`.github/scripts/tests/fixtures/publish-releases-json-expected.json`),
/// produced by its bats suite against stubbed `gh` data. The parser must
/// accept the real producer's document, not only the CLI's own fixture, so
/// the two sides of the ADR-0250 §2 contract stay pinned together.
const GENERATOR_EXPECTED_RELEASES_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../.github/scripts/tests/fixtures/publish-releases-json-expected.json"
));

#[test]
fn test_upgrade_unknown_flag_is_usage_error() {
    let home = tempdir();
    let (status, _, stderr) = run_cli(Opts::new(&["upgrade", "--bogus"]).in_home(home.path()));
    assert_eq!(status.code(), Some(2), "expected usage exit 2: {stderr}");
    assert!(
        stderr.contains("unexpected argument to upgrade: --bogus")
            && stderr.contains("relate upgrade [--yes] [--check]"),
        "stderr should reject the flag and show the upgrade usage line: {stderr}"
    );
}

#[test]
fn test_upgrade_extra_positional_is_usage_error() {
    let home = tempdir();
    let (status, _, stderr) =
        run_cli(Opts::new(&["upgrade", "extra-positional"]).in_home(home.path()));
    assert_eq!(status.code(), Some(2), "expected usage exit 2: {stderr}");
    assert!(
        stderr.contains("unexpected argument to upgrade: extra-positional")
            && stderr.contains("relate upgrade [--yes] [--check]"),
        "stderr should reject the extra argument: {stderr}"
    );
}

#[test]
fn test_update_alias_unknown_flag_is_usage_error() {
    let home = tempdir();
    let (status, _, stderr) = run_cli(Opts::new(&["update", "--bogus"]).in_home(home.path()));
    assert_eq!(status.code(), Some(2), "expected usage exit 2: {stderr}");
    assert!(
        stderr.contains("unexpected argument to upgrade: --bogus")
            && stderr.contains("relate upgrade [--yes] [--check]"),
        "update alias must share upgrade's usage errors: {stderr}"
    );
}

// --- `--database upgrade` names a store, not the upgrade command ---

#[test]
fn test_database_flag_accepts_upgrade_as_store_path() {
    let home = tempdir();
    let dir = tempdir();
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["--database", "upgrade", "RETURN 1 AS n;"])
            .in_home(home.path())
            .in_dir(dir.path()),
    );
    assert!(
        status.success(),
        "--database upgrade should open as a database path, got code {:?}, stderr: {stderr}",
        status.code()
    );
    assert!(stdout.contains('1'), "expected query output, got: {stdout}");
    assert!(
        dir.path().join("upgrade").is_dir(),
        "`--database upgrade` should have created a database directory named upgrade"
    );
    assert!(
        !stderr.contains("unexpected argument to upgrade"),
        "`--database upgrade` must not enter upgrade command mode: {stderr}"
    );
}

// --- release discovery reads the website's releases.json (ADR-0250) ---

#[test]
fn test_upgrade_check_reads_releases_json_without_a_token() {
    let home = tempdir();
    let fixture = ReleasesFixture::spawn(RELEASES_JSON);
    // ADR-0250 §5: even with a token set, the metadata request must not
    // carry an Authorization header.
    let mut envs = fixture.envs();
    envs.push(("GITHUB_TOKEN", "ghp_test_token"));
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["upgrade", "--check"])
            .in_home(home.path())
            .with_envs(&envs),
    );
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");
    // The fixture's latest tag (v0.0.9) predates every released build, so
    // --check reports both versions and stops before any download.
    assert!(
        stdout.contains("latest release:  0.0.9"),
        "the fixture's latest tag must be reported: {stdout}"
    );
    assert!(
        stdout.contains("relate is up to date"),
        "a stale latest release is not an error: {stdout}"
    );
    let heads = fixture.heads();
    assert_eq!(heads.len(), 1, "one metadata request only: {heads:?}");
    assert!(
        header_values(&heads[0], "authorization").is_empty(),
        "the releases.json request must carry no Authorization header: {heads:?}"
    );
    assert!(
        heads[0].starts_with("GET /releases.json HTTP/1.1\r\n"),
        "unexpected request: {:?}",
        heads[0]
    );
}

#[test]
fn test_upgrade_check_parses_the_generator_expected_fixture() {
    // The publisher's committed expected output must parse through the real
    // fetch/parse path: the CLI consumes the generator's document (latest
    // v0.0.11, a prerelease in the reserved slot), not a CLI-shaped copy of
    // it. Its latest stable release predates every released build, so
    // `--check` reports it and exits 0.
    let home = tempdir();
    let fixture = ReleasesFixture::spawn(GENERATOR_EXPECTED_RELEASES_JSON);
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["upgrade", "--check"])
            .in_home(home.path())
            .with_envs(&fixture.envs()),
    );
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");
    assert!(
        stdout.contains("latest release:  0.0.11"),
        "the generator fixture's latest tag must be reported: {stdout}"
    );
    assert!(
        stdout.contains("relate is up to date"),
        "a stale latest release is not an error: {stdout}"
    );
    let heads = fixture.heads();
    assert_eq!(heads.len(), 1, "one metadata request only: {heads:?}");
    assert!(
        heads[0].starts_with("GET /releases.json HTTP/1.1\r\n"),
        "unexpected request: {:?}",
        heads[0]
    );
}

/// Not on Windows: a plain `upgrade` there refuses before release discovery
/// (`test_upgrade_on_windows_refuses_with_the_powershell_installer`).
#[cfg(not(windows))]
#[test]
fn test_upgrade_up_to_date_stops_before_any_download() {
    // The plain (non-`--check`) up-to-date site: the stale latest release
    // stops the run before any download.
    let home = tempdir();
    let fixture = ReleasesFixture::spawn(RELEASES_JSON);
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["upgrade"])
            .in_home(home.path())
            .with_envs(&fixture.envs()),
    );
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");
    assert!(
        stdout.contains("is up to date (latest release: 0.0.9)"),
        "the plain up-to-date line must name the stale release: {stdout}"
    );
    let heads = fixture.heads();
    assert_eq!(heads.len(), 1, "only the metadata request: {heads:?}");
}

/// Not on Windows: a plain `upgrade` there refuses before release discovery,
/// and a Windows build's reinstall pointer is the PowerShell one-liner.
#[cfg(not(windows))]
#[test]
fn test_upgrade_fails_closed_when_releases_json_is_missing() {
    let home = tempdir();
    let fixture = ReleasesFixture::missing();
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["upgrade"])
            .in_home(home.path())
            .with_envs(&fixture.envs()),
    );
    assert_eq!(status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stdout.is_empty(),
        "the failure path prints nothing on stdout: {stdout}"
    );
    assert_eq!(stderr.lines().count(), 1, "single-line error: {stderr}");
    assert!(stderr.starts_with("Error: upgrade: "), "stderr: {stderr}");
    assert!(
        stderr.contains(&fixture.url),
        "the error must name the metadata URL: {stderr}"
    );
    assert!(
        stderr.contains("http status: 404"),
        "the error must carry the typed status detail from the shared channel client: {stderr}"
    );
    assert!(
        stderr.contains("install.sh"),
        "the error must point at the reinstall one-liner: {stderr}"
    );
    // One scripted response: a second request (an asset download) would find
    // no listener and could not have been served.
    let heads = fixture.heads();
    assert_eq!(heads.len(), 1, "only the metadata request: {heads:?}");
}

#[test]
fn test_upgrade_fails_closed_on_malformed_releases_json() {
    let home = tempdir();
    let fixture = ReleasesFixture::spawn("{\"schema_version\": 2}");
    let (status, _stdout, stderr) = run_cli(
        Opts::new(&["upgrade", "--check"])
            .in_home(home.path())
            .with_envs(&fixture.envs()),
    );
    assert_eq!(status.code(), Some(1), "stderr: {stderr}");
    assert_eq!(stderr.lines().count(), 1, "single-line error: {stderr}");
    assert!(
        stderr.contains("schema_version") && stderr.contains(&fixture.url),
        "the error must name the offending key and the URL: {stderr}"
    );
    let heads = fixture.heads();
    assert_eq!(heads.len(), 1, "only the metadata request: {heads:?}");
}

// --- --check against a newer release, and the Windows refusal (ADR-0586) ---

/// `--check` against a newer latest release reports the availability line,
/// then returns before any download: one metadata request reaches
/// the loopback and nothing else is fetched.
#[cfg(unix)]
#[test]
fn test_upgrade_check_upgrade_available_stops_at_the_metadata_request() {
    let home = tempdir();
    let fixture = ReleasesFixture::spawn(NEWER_RELEASES_JSON);
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["upgrade", "--check"])
            .in_home(home.path())
            .with_envs(&fixture.envs()),
    );
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");
    assert!(
        stdout.contains("upgrade available: run `relate upgrade` to install 99.0.0"),
        "the newer latest release must be reported: {stdout}"
    );
    let heads = fixture.heads();
    assert_eq!(
        heads.len(),
        1,
        "--check stops at the metadata request: {heads:?}"
    );
}

/// On Windows `--check` still reports (ADR-0586): only the replace is
/// unsupported there, so the availability line names the newer release and
/// advises the PowerShell installer instead of a `relate upgrade` that
/// would refuse — and the run stops at the one metadata request.
#[cfg(windows)]
#[test]
fn test_upgrade_check_on_windows_advises_the_powershell_installer() {
    let home = tempdir();
    let fixture = ReleasesFixture::spawn(NEWER_RELEASES_JSON);
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["upgrade", "--check"])
            .in_home(home.path())
            .with_envs(&fixture.envs()),
    );
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");
    assert!(
        stdout.contains("latest release:  99.0.0"),
        "the newer latest release must be reported: {stdout}"
    );
    assert!(
        stdout.contains("upgrade available: 99.0.0; self-upgrade is not supported on Windows"),
        "the availability line must carry the Windows refusal: {stdout}"
    );
    assert!(
        stdout.contains("https://relatedb.ai/install.ps1"),
        "the advice must name the PowerShell installer: {stdout}"
    );
    assert!(
        !stdout.contains("run `relate upgrade`"),
        "never advise an upgrade that refuses: {stdout}"
    );
    let heads = fixture.heads();
    assert_eq!(
        heads.len(),
        1,
        "--check stops at the metadata request: {heads:?}"
    );
}

/// A Windows `relate upgrade` (no `--check`) refuses before release
/// discovery, pointing at the PowerShell installer — no README anchor.
#[cfg(windows)]
#[test]
fn test_upgrade_on_windows_refuses_with_the_powershell_installer() {
    let home = tempdir();
    let (status, _stdout, stderr) = run_cli(Opts::new(&["upgrade"]).in_home(home.path()));
    assert_eq!(status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains("self-upgrade is not supported on Windows"),
        "{stderr}"
    );
    assert!(
        stderr.contains("https://relatedb.ai/install.ps1"),
        "{stderr}"
    );
}
