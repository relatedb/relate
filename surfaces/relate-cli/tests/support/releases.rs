//! The `upgrade` e2e harness (ADR-0250 §1): the loopback that serves a
//! `releases.json` document through the metadata URL override, and the
//! newer-release document the check and download-route suites both serve —
//! shared by `e2e_upgrade.rs` and `e2e_upgrade_route.rs`, so the fixture
//! has one copy.

use relate_test_support::loopback::{spawn_single_response_capturing, CapturingFixture};

/// The releases.json loopback fixture the release-discovery tests share, and
/// the download-route fixture composes: the served document, its captured
/// request heads, and the metadata URL override derived from its port.
/// Spawned by [`ReleasesFixture::spawn`] or [`ReleasesFixture::missing`].
pub struct ReleasesFixture {
    /// The one scripted metadata response: joining it yields the captured
    /// request heads. Private to the impl behind [`ReleasesFixture::heads`].
    server: CapturingFixture,
    /// The metadata URL override [`ReleasesFixture::envs`] pairs with its
    /// name; suites that name it in an asserted message read it directly.
    pub url: String,
}

impl ReleasesFixture {
    /// Spawn the loopback serving `body` as the website's published
    /// `releases.json` (ADR-0250 §1): one metadata fetch per run.
    pub fn spawn(body: &'static str) -> Self {
        Self::scripted("200 OK", body)
    }

    /// Spawn the loopback answering the metadata request with a typed 404 and
    /// no body: the missing-document shape the fail-closed test serves.
    pub fn missing() -> Self {
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

    /// The upgrade env pairs every release-discovery test runs with: the one
    /// loopback base. Everything else stays at the harness default.
    pub fn envs(&self) -> Vec<(&str, &str)> {
        vec![("RELATE_RELEASES_JSON_URL", self.url.as_str())]
    }

    /// Wait for the metadata fixture to serve its one response and return the
    /// request heads it captured.
    pub fn heads(self) -> Vec<String> {
        self.server.join()
    }
}

/// A `releases.json` document whose `latest` tag (v99.0.0) is newer than the
/// workspace version, so `upgrade` takes the download path. All four platform
/// binaries are listed so the document is host-independent.
pub const NEWER_RELEASES_JSON: &str = r#"{
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
