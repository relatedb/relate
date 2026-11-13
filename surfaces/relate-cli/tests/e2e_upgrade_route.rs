//! End-to-end tests for the `upgrade` download route (ADR-0358): the direct
//! release-download route, the authenticated API fallback on a typed probe
//! 404, and the tokenless 404 refusal. `RELATE_RELEASE_DOWNLOAD_URL` and
//! `RELATE_GITHUB_API_URL` point at loopback fixtures (ADR-0358 §8) serving
//! the real release manifest and its signature, so both routes run end to
//! end offline. Unix-only: the fixtures serve the Unix release assets.

#![cfg(all(feature = "upgrade", unix))]

mod support;

use relate_test_support::loopback::{
    header_values, spawn_capture_server, spawn_scripted_responses_capturing,
    spawn_single_response_capturing, CaptureSink, CapturingFixture,
};
use support::releases::{ReleasesFixture, NEWER_RELEASES_JSON};
use support::{run_cli, tempdir, CliRunOptions as Opts};

/// The committed release-manifest pair (`fixtures/release/`): a renamed
/// manifest without the ADR-0457 version header and an SSHSIG by the
/// committed fixture signer (`test-signing-key.pub`), not the pinned release
/// key. The route tests therefore drive the real route and the real pinned-key
/// refusal: a manifest this channel did not sign never installs.
const RELEASE_SUMS: &str = include_str!("fixtures/release/SHA256SUMS");
const RELEASE_SUMS_SIG: &str = include_str!("fixtures/release/SHA256SUMS.sig");

/// The bearer token every fallback request must carry.
const TOKEN: &str = "ghp_test_token";

/// The tokenless run's `GITHUB_TOKEN` value: an empty value means no token,
/// so a host token cannot leak into the run.
const NO_TOKEN: &str = "";

/// The bytes served as the platform binary asset: deliberately not the
/// released binary, so the run stops at the checksum check before any
/// replace.
const NOT_THE_BINARY: &str = "relate upgrade e2e fixture: not the released binary\n";

/// The tag-lookup id of the first asset [`NEWER_RELEASES_JSON`] lists; the
/// assets that follow take the next ids in document order. The fallback
/// lookup numbers that same list, so the two fixture documents cannot drift.
const FIRST_FIXTURE_ASSET_ID: u64 = 101;

/// The assets [`NEWER_RELEASES_JSON`] lists, in document order, each with the
/// tag-lookup id the fallback fixture assigns it: one list behind both the
/// releases document and the API lookup.
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
enum DirectRoute {
    /// A 2xx probe: the direct route serves the manifest, its signature and
    /// the platform binary.
    ServesAllThreeAssets,
    /// A typed 404 on the probe: the token decides fallback vs abort
    /// (ADR-0358 §4).
    Probe404,
}

/// The API route's fixture, in the shape the run's token selects.
enum ApiRoute {
    /// A recording sink: the run must leave the API route untouched.
    Sink(CaptureSink),
    /// The scripted fallback responses: the tag lookup, then the three
    /// assets.
    Fallback(CapturingFixture),
}

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

impl RouteFixture {
    /// The four upgrade env pairs every route test runs with: the metadata
    /// pair [`ReleasesFixture::envs`] seeds, plus the two loopback bases and
    /// the token. Everything else stays at the harness default.
    fn envs(&self) -> Vec<(&str, &str)> {
        let mut envs = self.releases.envs();
        envs.extend([
            ("RELATE_RELEASE_DOWNLOAD_URL", self.direct_base.as_str()),
            ("RELATE_GITHUB_API_URL", self.api_base.as_str()),
            ("GITHUB_TOKEN", self.token.as_str()),
        ]);
        envs
    }
}

/// The three asset bodies in the run's download order — the release
/// manifest, its signature, then the platform binary — shared by the
/// direct-route fixture and the API fallback tail so the two routes cannot
/// drift (ADR-0358 §2, §3).
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
