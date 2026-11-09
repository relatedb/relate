//! End-to-end tests for the `upgrade` / `update` positional lifecycle
//! commands (ADR-0069): `relate upgrade [--yes] [--check]`.
//!
//! The argument-grammar tests are validated before any network access and
//! never open a socket. The release-discovery tests point the metadata URL
//! override ([`ReleasesFixture::envs`]) at a loopback fixture (ADR-0250 §1),
//! so they exercise the real fetch/parse path without leaving the machine. The
//! download-route tests point `RELATE_RELEASE_DOWNLOAD_URL` and
//! `RELATE_GITHUB_API_URL` at loopback fixtures (ADR-0358 §8) and serve
//! the real release manifest and its signature, so the direct route and the
//! API fallback run end to end offline.

#![cfg(feature = "upgrade")]

mod support;

use relate_test_support::loopback::{
    header_values, spawn_single_response_capturing, CapturingFixture,
};
#[cfg(unix)]
use relate_test_support::loopback::{
    spawn_capture_server, spawn_scripted_responses_capturing, CaptureSink,
};
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

// --- the rename notice every success site prints (LABS-973) ---

/// The two notice lines every `upgrade` success site prints after its status
/// line, re-typed here on purpose: the end-to-end test pins the user-visible
/// text, so it must not read it back from the CLI's own constant.
const RENAME_NOTICE_LINES: [&str; 2] = [
    "cypherlite is now relate — this is the final cypherlite release",
    "install relate with: curl -sSfL https://relatedb.ai/install.sh | GITHUB_TOKEN=$(gh auth token) bash",
];

/// Assert `stdout` carries the two notice lines as one contiguous block, in
/// order: the notice reads as a block, so a run that split it (another line
/// between them) must not pass.
fn assert_rename_notice(stdout: &str) {
    let block = RENAME_NOTICE_LINES.join("\n");
    assert!(
        stdout.contains(&block),
        "missing the contiguous rename notice block {block:?}: {stdout}"
    );
}

// --- usage guards for the positional grammar ---

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

/// The releases.json loopback fixture the release-discovery tests share, and
/// the download-route fixture composes: the served document, its captured
/// request heads, and the metadata URL override derived from its port.
/// Spawned by [`ReleasesFixture::spawn`] or [`ReleasesFixture::missing`].
struct ReleasesFixture {
    /// The one scripted metadata response: joining it yields the captured
    /// request heads. Private to the impl behind [`ReleasesFixture::heads`].
    server: CapturingFixture,
    /// The metadata URL override [`ReleasesFixture::envs`] pairs with its
    /// name.
    url: String,
}

impl ReleasesFixture {
    /// Spawn the loopback serving `body` as the website's published
    /// `releases.json` (ADR-0250 §1): one metadata fetch per run.
    fn spawn(body: &'static str) -> Self {
        Self::scripted("200 OK", body)
    }

    /// Spawn the loopback answering the metadata request with a typed 404 and
    /// no body: the missing-document shape the fail-closed test serves.
    fn missing() -> Self {
        Self::scripted("404 Not Found", "")
    }

    /// Spawn the one-response loopback; `body` is served under `status`.
    fn scripted(status: &'static str, body: &'static str) -> Self {
        let (server, port) = spawn_single_response_capturing(status, body);
        Self {
            server,
            url: format!("http://127.0.0.1:{port}/releases.json"),
        }
    }

    /// The upgrade env pairs every release-discovery test runs with: the
    /// metadata fixture as the primary, and a refused loopback port as the
    /// fallback (ADR-0550) — the two seams stay distinguishable, so a run
    /// that read the fallback where the primary was meant shows up as a
    /// failed fetch, the fail-closed cases stay fail-closed, and no test
    /// ever leaves loopback. Everything else stays at the harness default.
    fn envs(&self) -> [(&str, &str); 2] {
        [
            ("RELATE_RELEASES_JSON_URL", &self.url),
            (
                "RELATE_RELEASES_JSON_FALLBACK_URL",
                "http://127.0.0.1:9/releases.json",
            ),
        ]
    }

    /// Wait for the metadata fixture to serve its one response and return the
    /// request heads it captured.
    fn heads(self) -> Vec<String> {
        self.server.join()
    }
}

#[test]
fn test_upgrade_check_reads_releases_json_without_a_token() {
    let home = tempdir();
    let fixture = ReleasesFixture::spawn(RELEASES_JSON);
    // ADR-0250 §5: even with a token set, the metadata request must not
    // carry an Authorization header.
    let mut envs = fixture.envs().to_vec();
    envs.push(("GITHUB_TOKEN", "ghp_test_token"));
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["upgrade", "--check"])
            .in_home(home.path())
            .with_envs(&envs),
    );
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");
    // The fixture's latest tag (v0.10.0) predates every released build, so
    // --check reports both versions and stops before any download.
    assert!(
        stdout.contains("latest release:  0.10.0"),
        "the fixture's latest tag must be reported: {stdout}"
    );
    assert!(
        stdout.contains("relate is up to date"),
        "a stale latest release is not an error: {stdout}"
    );
    assert_rename_notice(&stdout);
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
    // v0.10.1, a prerelease in the reserved slot), not a CLI-shaped copy of
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
        stdout.contains("latest release:  0.10.1"),
        "the generator fixture's latest tag must be reported: {stdout}"
    );
    assert!(
        stdout.contains("relate is up to date"),
        "a stale latest release is not an error: {stdout}"
    );
    assert_rename_notice(&stdout);
    let heads = fixture.heads();
    assert_eq!(heads.len(), 1, "one metadata request only: {heads:?}");
    assert!(
        heads[0].starts_with("GET /releases.json HTTP/1.1\r\n"),
        "unexpected request: {:?}",
        heads[0]
    );
}

#[test]
fn test_upgrade_up_to_date_prints_the_rename_notice() {
    // The plain (non-`--check`) up-to-date site: the stale latest release
    // stops the run before any download and prints the notice as well.
    let home = tempdir();
    let fixture = ReleasesFixture::spawn(RELEASES_JSON);
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["upgrade"])
            .in_home(home.path())
            .with_envs(&fixture.envs()),
    );
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");
    assert!(
        stdout.contains("is up to date (latest release: 0.10.0)"),
        "the plain up-to-date line must name the stale release: {stdout}"
    );
    assert_rename_notice(&stdout);
    let heads = fixture.heads();
    assert_eq!(heads.len(), 1, "only the metadata request: {heads:?}");
}

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

// --- the download route probe: direct, fallback, tokenless 404 (ADR-0358) ---

/// A `releases.json` document whose `latest` tag (v99.0.0) is newer than the
/// workspace version, so `upgrade` takes the download path. All four platform
/// binaries are listed so the document is host-independent.
#[cfg(unix)]
const NEWER_RELEASES_JSON: &str = r#"{
  "schema_version": 1,
  "generated_at": "2026-10-18T12:00:00Z",
  "latest": {
    "tag": "v99.0.0",
    "published_at": "2026-10-18T12:00:00Z",
    "prerelease": false,
    "notes": "",
    "assets": [
      "relate-darwin-amd64",
      "relate-darwin-arm64",
      "relate-linux-amd64",
      "relate-linux-arm64",
      "SHA256SUMS",
      "SHA256SUMS.sig"
    ]
  },
  "latest_prerelease": null,
  "releases": []
}"#;

/// `--check` against a newer latest release reports the availability line and
/// the notice, then returns before any download: one metadata request reaches
/// the loopback and nothing else is fetched.
#[cfg(unix)]
#[test]
fn test_upgrade_check_upgrade_available_prints_the_rename_notice() {
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
    assert_rename_notice(&stdout);
    let heads = fixture.heads();
    assert_eq!(
        heads.len(),
        1,
        "--check stops at the metadata request: {heads:?}"
    );
}

/// The committed release-manifest pair (`fixtures/release/`): a renamed
/// manifest without the ADR-0457 version header and an SSHSIG by the
/// committed fixture signer (`test-signing-key.pub`), not the pinned release
/// key. The route tests therefore drive the real route and the real pinned-key
/// refusal: a manifest this channel did not sign never installs.
#[cfg(unix)]
const RELEASE_SUMS: &str = include_str!("fixtures/release/SHA256SUMS");
#[cfg(unix)]
const RELEASE_SUMS_SIG: &str = include_str!("fixtures/release/SHA256SUMS.sig");

/// The bearer token every fallback request must carry.
#[cfg(unix)]
const TOKEN: &str = "ghp_test_token";

/// The tokenless run's `GITHUB_TOKEN` value: an empty value means no token,
/// so a host token cannot leak into the run.
#[cfg(unix)]
const NO_TOKEN: &str = "";

/// The bytes served as the platform binary asset: deliberately not the
/// released binary, so the run stops at the checksum check before any
/// replace.
#[cfg(unix)]
const NOT_THE_BINARY: &str = "relate upgrade e2e fixture: not the released binary\n";

/// The tag-lookup id of the first asset [`NEWER_RELEASES_JSON`] lists; the
/// assets that follow take the next ids in document order. The fallback
/// lookup numbers that same list, so the two fixture documents cannot drift.
#[cfg(unix)]
const FIRST_FIXTURE_ASSET_ID: u64 = 101;

/// The assets [`NEWER_RELEASES_JSON`] lists, in document order, each with the
/// tag-lookup id the fallback fixture assigns it: one list behind both the
/// releases document and the API lookup.
#[cfg(unix)]
fn fixture_assets() -> Vec<(String, u64)> {
    let doc: serde_json::Value =
        serde_json::from_str(NEWER_RELEASES_JSON).expect("the newer releases fixture is JSON");
    doc["latest"]["assets"]
        .as_array()
        .expect("the fixture's latest.assets is an array")
        .iter()
        .enumerate()
        .map(|(position, name)| {
            let name = name.as_str().expect("every fixture asset is a string");
            (name.to_string(), FIRST_FIXTURE_ASSET_ID + position as u64)
        })
        .collect()
}

/// The tag-lookup id of the fixture asset named `name`, resolved from the
/// same list the fallback lookup is built from: the run's announced asset
/// name selects its id, so the test never re-tables the platform mapping.
#[cfg(unix)]
fn fixture_asset_id(name: &str) -> u64 {
    fixture_assets()
        .into_iter()
        .find(|(asset, _)| asset == name)
        .unwrap_or_else(|| panic!("the releases.json fixture does not list {name}"))
        .1
}

/// The asset name the run announced on its `upgrading relate …
/// ({asset})` stdout line: the command's own platform → asset mapping, read
/// back instead of re-tabled here. The `upgrade` command supports macOS and
/// Linux only, so the route tests are `#[cfg(unix)]`; any other host is a
/// broken test environment, not a skip.
#[cfg(unix)]
fn announced_asset(stdout: &str) -> &str {
    let line = stdout
        .lines()
        .find(|line| line.starts_with("upgrading relate "))
        .unwrap_or_else(|| panic!("the run never announced the upgrade: {stdout}"));
    let (_, asset) = line
        .rsplit_once('(')
        .unwrap_or_else(|| panic!("the announcement names no asset: {line:?}"));
    asset
        .strip_suffix(')')
        .unwrap_or_else(|| panic!("the announced asset is not parenthesized: {line:?}"))
}

/// The direct download route's scripted scenario: the one thing that varies
/// per route test.
#[cfg(unix)]
enum DirectRoute {
    /// A 2xx probe: the direct route serves the manifest, its signature and
    /// the platform binary.
    ServesAllThreeAssets,
    /// A typed 404 on the probe: the token decides fallback vs abort
    /// (ADR-0358 §4).
    Probe404,
}

/// The API route's fixture, in the shape the run's token selects.
#[cfg(unix)]
enum ApiRoute {
    /// A recording sink: the run must leave the API route untouched.
    Sink(CaptureSink),
    /// The scripted fallback responses: the tag lookup, then the three
    /// assets.
    Fallback(CapturingFixture),
}

#[cfg(unix)]
impl ApiRoute {
    /// The recording sink; the tokenless route tests assert its request
    /// count. Panics when this fixture serves the fallback instead.
    fn sink(&self) -> &CaptureSink {
        match self {
            ApiRoute::Sink(sink) => sink,
            ApiRoute::Fallback(_) => panic!("this fixture serves the API fallback"),
        }
    }

    /// The scripted fallback fixture; the fallback test joins it for the
    /// captured heads. Panics when this fixture records the API route.
    fn fallback(self) -> CapturingFixture {
        match self {
            ApiRoute::Fallback(fixture) => fixture,
            ApiRoute::Sink(_) => panic!("this fixture records the API route instead"),
        }
    }
}

/// The loopback fixtures the three download-route tests share: the
/// releases.json document, the direct download route, and the API route,
/// with the three base URLs and the four upgrade env pairs derived from
/// them. Spawned by [`spawn_route_fixture`].
#[cfg(unix)]
struct RouteFixture {
    /// The releases.json document fixture: one metadata fetch per run.
    releases: ReleasesFixture,
    /// The direct download route, scripted by the test's [`DirectRoute`].
    direct: CapturingFixture,
    /// The API route, in the shape the run's token selects.
    api: ApiRoute,
    /// `RELATE_RELEASE_DOWNLOAD_URL`.
    direct_base: String,
    /// `RELATE_GITHUB_API_URL`.
    api_base: String,
    /// The run's `GITHUB_TOKEN` ([`NO_TOKEN`] means no token).
    token: String,
}

#[cfg(unix)]
impl RouteFixture {
    /// The five upgrade env pairs every route test runs with: the two
    /// discovery pairs [`ReleasesFixture::envs`] seeds (ADR-0550), plus the
    /// two loopback bases and the token. Everything else stays at the
    /// harness default.
    fn envs(&self) -> [(&str, &str); 5] {
        let [(releases_key, releases_url), (fallback_key, fallback_url)] = self.releases.envs();
        [
            (releases_key, releases_url),
            (fallback_key, fallback_url),
            ("RELATE_RELEASE_DOWNLOAD_URL", &self.direct_base),
            ("RELATE_GITHUB_API_URL", &self.api_base),
            ("GITHUB_TOKEN", &self.token),
        ]
    }
}

/// The three asset bodies in the run's download order — the release
/// manifest, its signature, then the platform binary — shared by the
/// direct-route fixture and the API fallback tail so the two routes cannot
/// drift (ADR-0358 §2, §3).
#[cfg(unix)]
fn asset_bodies_in_download_order() -> Vec<String> {
    vec![
        RELEASE_SUMS.to_string(),
        RELEASE_SUMS_SIG.to_string(),
        NOT_THE_BINARY.to_string(),
    ]
}

/// The scripted API fallback responses (ADR-0358 §3): the release-tag lookup
/// the API route resolves its asset ids from — every asset the releases.json
/// fixture lists, so the lookup is host-independent — then the three asset
/// bodies in the run's download order.
#[cfg(unix)]
fn fallback_api_responses() -> Vec<String> {
    let assets = fixture_assets()
        .iter()
        .map(|(name, id)| serde_json::json!({"id": id, "name": name}))
        .collect::<Vec<_>>();
    let mut responses =
        vec![serde_json::json!({"tag_name": "v99.0.0", "assets": assets}).to_string()];
    responses.extend(asset_bodies_in_download_order());
    responses
}

/// Spawn the loopback fixtures the three download-route tests share: the
/// releases.json document, the direct download route scripted by `direct`,
/// and the API route. `token` is written to `GITHUB_TOKEN` verbatim and
/// selects the API route's shape exactly as it selects the CLI's route
/// (ADR-0358 §4): without one the fallback cannot be entered, so the API
/// side is a recording sink; with one it is the scripted fallback responses.
/// The three base URLs and the four env pairs derive from the returned
/// fixture.
#[cfg(unix)]
fn spawn_route_fixture(direct: DirectRoute, token: &str) -> RouteFixture {
    let releases = ReleasesFixture::spawn(NEWER_RELEASES_JSON);
    let (direct_fixture, direct_port) = match direct {
        DirectRoute::ServesAllThreeAssets => {
            spawn_scripted_responses_capturing("200 OK", asset_bodies_in_download_order())
        }
        DirectRoute::Probe404 => spawn_single_response_capturing("404 Not Found", ""),
    };
    let (api, api_port) = if token.is_empty() {
        let (sink, port) = spawn_capture_server("200 OK", "1");
        (ApiRoute::Sink(sink), port)
    } else {
        let (fixture, port) =
            spawn_scripted_responses_capturing("200 OK", fallback_api_responses());
        (ApiRoute::Fallback(fixture), port)
    };
    RouteFixture {
        releases,
        direct: direct_fixture,
        api,
        direct_base: format!("http://127.0.0.1:{direct_port}"),
        api_base: format!("http://127.0.0.1:{api_port}"),
        token: token.to_string(),
    }
}

/// The direct route serves the manifest and its signature (ADR-0358 §2): a
/// 2xx probe keeps the whole upgrade on the browser download route — no API
/// request, no token — and the pinned release key decides the signature. The
/// committed pair is signed by a fixture signer, not the release key, so the
/// pinned key refuses it **before** the binary download: a manifest this
/// channel did not sign is never installable. (Pre-cutover manifests cannot
/// verify either — they carry the retired pre-cutover
/// SSHSIG namespace, ADR-0517 §6 — so the version-binding refusals themselves are
/// pinned by `upgrade_cmd::manifest`'s unit tests.)
#[cfg(unix)]
#[test]
fn test_upgrade_direct_route_refuses_a_non_release_signer() {
    let home = tempdir();
    let route = spawn_route_fixture(DirectRoute::ServesAllThreeAssets, NO_TOKEN);
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["upgrade"])
            .in_home(home.path())
            .with_envs(&route.envs()),
    );
    assert_eq!(status.code(), Some(1), "stdout: {stdout}\nstderr: {stderr}");
    // The run's own announcement names the host's asset: assert it is one
    // the release document lists (the platform → asset mapping, read back).
    let asset = announced_asset(&stdout);
    assert!(
        fixture_assets().iter().any(|(name, _)| name == asset),
        "the announced asset must be listed by the releases fixture: {asset}"
    );
    assert!(
        stderr.contains("SHA256SUMS.sig verification failed"),
        "the run must refuse a manifest the release key did not sign: {stderr}"
    );
    assert!(
        !stderr.contains("checksum verification failed"),
        "the run must stop before the checksum/binary download: {stderr}"
    );
    assert!(
        !stderr.contains("falling back"),
        "a 2xx probe must not fall back to the API route: {stderr}"
    );
    let heads = route.direct.join();
    assert_eq!(
        heads.len(),
        2,
        "probe + signature only; the binary must not be downloaded: {heads:?}"
    );
    assert!(
        heads[0].starts_with("GET /v99.0.0/SHA256SUMS HTTP/1.1\r\n"),
        "the first request is the manifest probe: {:?}",
        heads[0]
    );
    assert!(
        heads[1].starts_with("GET /v99.0.0/SHA256SUMS.sig HTTP/1.1\r\n"),
        "the signature follows the probe: {:?}",
        heads[1]
    );
    for head in &heads {
        assert!(
            header_values(head, "authorization").is_empty(),
            "the direct route needs no token: {head:?}"
        );
    }
    assert_eq!(
        route.api.sink().request_count(),
        0,
        "the direct route must not touch the API base"
    );
    assert_eq!(
        route.releases.heads().len(),
        1,
        "the metadata document is fetched exactly once"
    );
}

/// A typed 404 on the direct probe with `GITHUB_TOKEN` set falls back to the
/// authenticated GitHub API asset route (ADR-0358 §3): one stderr note, the
/// release-tag lookup, then the manifest and its signature by id, every
/// request carrying the bearer token. The pinned release key then decides the
/// signature over the fetched manifest: the committed pair is fixture-signed,
/// so the pinned key refuses it before the binary download.
#[cfg(unix)]
#[test]
fn test_upgrade_falls_back_to_api_assets_on_probe_404() {
    let home = tempdir();
    let route = spawn_route_fixture(DirectRoute::Probe404, TOKEN);
    let (status, stdout, stderr) = run_cli(
        Opts::new(&["upgrade"])
            .in_home(home.path())
            .with_envs(&route.envs()),
    );
    assert_eq!(status.code(), Some(1), "stdout: {stdout}\nstderr: {stderr}");
    // The run's own announcement names the host's asset: assert it is one
    // the release document lists (the platform → asset mapping, read back).
    let asset = announced_asset(&stdout);
    assert!(
        fixture_assets().iter().any(|(name, _)| name == asset),
        "the announced asset must be listed by the releases fixture: {asset}"
    );
    assert!(
        stderr.contains("SHA256SUMS.sig verification failed"),
        "the run must refuse a manifest the release key did not sign: {stderr}"
    );
    assert!(
        !stderr.contains("checksum verification failed"),
        "the run must stop before the checksum/binary download: {stderr}"
    );
    // Exactly the one fallback note, naming the direct probe URL.
    let probe_url = format!("{}/v99.0.0/SHA256SUMS", route.direct_base);
    let notes: Vec<&str> = stderr
        .lines()
        .filter(|line| line.starts_with("note: "))
        .collect();
    assert_eq!(notes.len(), 1, "one fallback note line: {stderr:?}");
    assert_eq!(
        notes[0],
        format!(
            "note: {probe_url} returned 404; falling back to the authenticated API asset route"
        )
    );
    // The direct fixture saw exactly the manifest probe.
    let direct_heads = route.direct.join();
    assert_eq!(direct_heads.len(), 1, "only the probe: {direct_heads:?}");
    assert!(
        direct_heads[0].starts_with("GET /v99.0.0/SHA256SUMS HTTP/1.1\r\n"),
        "unexpected probe request: {:?}",
        direct_heads[0]
    );
    // The API fixture saw the tag lookup plus the manifest and its
    // signature; the binary download never starts.
    let heads = route.api.fallback().join();
    assert_eq!(
        heads.len(),
        3,
        "tag lookup + manifest + signature: {heads:?}"
    );
    let expected_requests = [
        "GET /repos/relatedb/relate/releases/tags/v99.0.0 HTTP/1.1\r\n".to_string(),
        format!(
            "GET /repos/relatedb/relate/releases/assets/{} HTTP/1.1\r\n",
            fixture_asset_id("SHA256SUMS")
        ),
        format!(
            "GET /repos/relatedb/relate/releases/assets/{} HTTP/1.1\r\n",
            fixture_asset_id("SHA256SUMS.sig")
        ),
    ];
    for (head, expected) in heads.iter().zip(&expected_requests) {
        assert!(
            head.starts_with(expected.as_str()),
            "expected {expected:?}, got {head:?}"
        );
    }
    for head in &heads {
        assert_eq!(
            header_values(head, "authorization"),
            vec![format!("Bearer {TOKEN}")],
            "every API request must carry the token: {head:?}"
        );
    }
    assert_eq!(
        header_values(&heads[0], "accept"),
        vec!["application/vnd.github+json"],
        "the tag lookup asks for the release JSON: {:?}",
        heads[0]
    );
    for head in &heads[1..] {
        assert_eq!(
            header_values(head, "accept"),
            vec!["application/octet-stream"],
            "asset downloads ask for the raw bytes: {head:?}"
        );
    }
    assert_eq!(
        route.releases.heads().len(),
        1,
        "the metadata document is fetched exactly once"
    );
}

/// A typed 404 on the direct probe without `GITHUB_TOKEN` aborts with the
/// single actionable line naming `GITHUB_TOKEN` (ADR-0358 §4), and no
/// request beyond the probe reaches either fixture.
#[cfg(unix)]
#[test]
fn test_upgrade_probe_404_without_token_names_github_token() {
    let home = tempdir();
    let route = spawn_route_fixture(DirectRoute::Probe404, NO_TOKEN);
    let (status, _stdout, stderr) = run_cli(
        Opts::new(&["upgrade"])
            .in_home(home.path())
            .with_envs(&route.envs()),
    );
    assert_eq!(status.code(), Some(1), "stderr: {stderr}");
    assert_eq!(stderr.lines().count(), 1, "single-line abort: {stderr:?}");
    assert!(
        stderr.contains("GITHUB_TOKEN"),
        "the abort must name the token: {stderr}"
    );
    assert!(
        stderr.contains(&format!("{}/v99.0.0/SHA256SUMS", route.direct_base)),
        "the abort must name the probe URL: {stderr}"
    );
    assert!(
        stderr.contains("http status: 404"),
        "the abort must name the status: {stderr}"
    );
    assert!(
        !stderr.contains("falling back"),
        "a tokenless 404 must not fall back: {stderr}"
    );
    let heads = route.direct.join();
    assert_eq!(
        heads.len(),
        1,
        "only the probe reaches the direct fixture: {heads:?}"
    );
    assert!(
        heads[0].starts_with("GET /v99.0.0/SHA256SUMS HTTP/1.1\r\n"),
        "unexpected probe request: {:?}",
        heads[0]
    );
    assert_eq!(
        route.api.sink().request_count(),
        0,
        "a tokenless 404 must not touch the API base"
    );
    assert_eq!(
        route.releases.heads().len(),
        1,
        "the metadata document is fetched exactly once"
    );
}
