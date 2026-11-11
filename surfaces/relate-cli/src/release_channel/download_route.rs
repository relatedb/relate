//! The release-channel download route (ADR-0358): the direct browser
//! download route's base and URL shape, the one probe request that decides
//! it, and the route it selects for every asset the caller downloads. The
//! route decision is typed — only an HTTP 404 status is a route signal,
//! never an error message — so it is a pure function with unit tests that
//! never open a socket; [`probe_route_at`] is the one network operation here,
//! and the module writes nothing to the console (the fallback note
//! [`RouteProbe`] returns is printed by the caller). The verification
//! chain and each flow's orchestration live in the caller (`upgrade_cmd`,
//! `extensions::install`), the API route's own fetches in
//! [`super::api_assets`], and the shared HTTP download helper in
//! [`super::http`].
//!
//! The probe asset and the needed set are the caller's: `upgrade` probes
//! `SHA256SUMS` and needs its three assets (ADR-0358 §1); `extension
//! install` probes the tarball the channel index named and needs exactly
//! it (ADR-0431 §1). The needed set must include the probe asset — the
//! fallback re-downloads it by id over the selected route.

use super::api_assets::{download_asset_by_id, fetch_asset_ids, AssetIds};
use super::http::{download_asset, AssetRequest, DownloadError};
use super::{seam, ChannelError, REPO};

/// Environment override for the direct-route download base (a documented
/// dev/test seam, ADR-0358 §8): the way a test can point the direct asset
/// downloads at a fixture or mirror. Empty means the canonical base.
const DOWNLOAD_URL_ENV: &str = "RELATE_RELEASE_DOWNLOAD_URL";

/// The canonical direct-route download base for `repo` (ADR-0250 §3),
/// composed from the slug: the default [`release_download_base`] and the
/// `--github` tier's base (ADR-0449) fall back to it.
fn canonical_download_base_for(repo: &str) -> String {
    format!("https://github.com/{repo}/releases/download")
}

/// The direct-route download base for `repo`: `RELATE_RELEASE_DOWNLOAD_URL`
/// when set and non-empty (the documented dev/test seam), else the canonical
/// `https://github.com/<repo>/releases/download` (ADR-0250 §3, ADR-0358
/// §8). There is no fallback between the two; a set but non-UTF-8 value is
/// a typed error naming the variable. Read once per flow by the callers
/// that thread the base into [`probe_route_at`] (the env-resolved value
/// as a parameter, the `load`/`load_with` pattern), so the post-decision
/// steps are drivable against a loopback fixture without racing the
/// process environment. The seam is the fixture/mirror hook for every
/// repository, not only the first-party one.
pub(crate) fn github_download_base(repo: &str) -> Result<String, ChannelError> {
    seam(DOWNLOAD_URL_ENV, &canonical_download_base_for(repo))
}

/// The first-party repository's direct-route base (ADR-0250 §3): the
/// `upgrade` flow's one call into [`github_download_base`], so the slug
/// has one source ([`REPO`]).
pub(crate) fn release_download_base() -> Result<String, ChannelError> {
    github_download_base(REPO)
}

/// The ADR-0250 §3 download URL for `asset` in release `tag` under `base`:
/// the exact tag (leading `v` included) and asset name appended. Pure, so
/// the URL shape is unit-testable.
fn download_url(base: &str, tag: &str, asset: &str) -> String {
    format!("{base}/{tag}/{asset}")
}

/// The route selected by the probe (ADR-0358 §1): one route serves every
/// asset the caller downloads, so the transport never changes mid-flow.
///
/// The `download` half serves the probe's own fallback re-download and
/// `upgrade`'s follow-up assets (its signature and binary follow the
/// manifest over the selected route, ADR-0358 §1) — every bundle that
/// compiles the probe compiles it.
pub(crate) enum AssetRoute {
    /// The direct browser download route (ADR-0250 §3): the probe answered
    /// 2xx, so no GitHub API request is made at all.
    Direct {
        /// Route base, `RELATE_RELEASE_DOWNLOAD_URL`-overridable.
        base: String,
        /// Release tag with its leading `v` (the `{tag}` path segment).
        tag: String,
        /// The optional bearer token (ADR-0250 §5).
        token: Option<String>,
    },
    /// The authenticated GitHub API asset route (ADR-0358 §3), entered only
    /// after a 404 on the direct probe with `GITHUB_TOKEN` set.
    Api {
        /// API base, `RELATE_GITHUB_API_URL`-overridable.
        base: String,
        /// The repository slug the release-tag lookup and every asset
        /// request resolve under.
        repo: String,
        /// The asset ids resolved from the release-tag lookup, for the
        /// caller's needed set.
        ids: AssetIds,
        /// The bearer token: required on this route.
        token: String,
    },
}

impl AssetRoute {
    /// Download the asset named `name` over the selected route
    /// (ADR-0358 §1): one route serves every asset — the name is the one
    /// key both halves of the request derive from (the direct route's URL
    /// segment and the error label; the API route's resolved id), so a
    /// call site can neither pair one asset's name with another's id nor
    /// name an asset outside the set the route resolved.
    pub(crate) fn download(&self, name: &str) -> Result<Vec<u8>, ChannelError> {
        match self {
            AssetRoute::Direct { base, tag, token } => {
                download_direct(base, tag, name, token.as_deref())
            }
            AssetRoute::Api {
                base,
                repo,
                ids,
                token,
            } => download_asset_by_id(base, repo, ids.id(name), name, token),
        }
    }
}

/// Download `name` from the direct browser download route (ADR-0250 §3),
/// mapping the typed status into the pinned one-line error text.
fn download_direct(
    base: &str,
    tag: &str,
    name: &str,
    token: Option<&str>,
) -> Result<Vec<u8>, ChannelError> {
    download_asset(AssetRequest {
        url: &download_url(base, tag, name),
        name,
        token,
        accept: None,
    })
    .map_err(DownloadError::into_channel_error)
}

/// The probe's verdict (ADR-0358 §2, §4, §5), decided from the typed download
/// result alone — never from an error message. Both failure variants carry
/// the transport's [`DownloadError`]; [`probe_route_at`] owns the conversion
/// to the user-facing [`ChannelError`].
#[derive(Debug)]
enum ProbeOutcome<'a> {
    /// The probe answered 2xx and here is the asset it fetched.
    Direct(Vec<u8>),
    /// The probe answered 404 and a token is available: fall back.
    Fallback(&'a str),
    /// The probe answered 404 and no token is available: abort, appending the
    /// `GITHUB_TOKEN` advice to this typed status error's own pinned text.
    Unauthenticated(DownloadError),
    /// Any other failure: abort exactly as the direct route always did.
    Failed(DownloadError),
}

/// Decide the route from the probe result (ADR-0358 §2, §4, §5). Pure, so
/// the fallback trigger is unit-testable without a socket: only the typed
/// HTTP 404 status is a route signal, and only with `GITHUB_TOKEN` set. The
/// outcome stays in the transport's own error vocabulary; [`probe_route_at`]
/// converts it.
fn probe_outcome<'a>(
    probe: Result<Vec<u8>, DownloadError>,
    token: Option<&'a str>,
) -> ProbeOutcome<'a> {
    match probe {
        Ok(probe_bytes) => ProbeOutcome::Direct(probe_bytes),
        Err(err @ DownloadError::Status { code: 404, .. }) => match token {
            Some(token) => ProbeOutcome::Fallback(token),
            None => ProbeOutcome::Unauthenticated(err),
        },
        Err(err) => ProbeOutcome::Failed(err),
    }
}

/// What the one probe request decided (ADR-0358 §1–§6): the route that
/// serves every asset, the probe asset's bytes — downloaded exactly once
/// on either arm: the probe's own 2xx response on the direct route, one
/// download over the selected route on the fallback — and the fallback
/// note line to print when a typed 404 selected the API route — `None` on
/// the direct route. The note is returned rather than printed: this
/// module writes nothing to the console, the caller owns user-facing
/// output.
pub(crate) struct RouteProbe {
    /// The route selected for every asset. Read by `upgrade`'s follow-up
    /// downloads; the extension channel's install consumes only the
    /// probe asset's bytes, so the `agent` bundle (extension-install
    /// without upgrade) ships the field unread — the same
    /// dead-until-its-consumer shape the registry's mount-pending views
    /// carry, allowed rather than cfg'd so the field is the same code in
    /// every bundle.
    #[allow(dead_code)]
    pub(crate) route: AssetRoute,
    /// The probe asset's bytes: the probe's own response on the direct
    /// route, the selected route's download on the fallback — never
    /// downloaded twice either way.
    pub(crate) probe_bytes: Vec<u8>,
    /// The one fallback note line (ADR-0358 §6), when the probe fell back.
    pub(crate) note: Option<String>,
}

/// Run the one probe request that decides the route (ADR-0358 §1–§5) and
/// return the selected route with the probe asset's bytes, downloaded
/// exactly once: the probe's own 2xx response serves the direct route,
/// while on the fallback — the probe's 404 fetched nothing — the selected
/// route fetches the probe asset itself. The direct route is probed first;
/// a 2xx serves the whole flow from it, a typed 404 with `GITHUB_TOKEN`
/// set switches to the authenticated API asset route for every asset and
/// returns the one fallback note for the caller to print, a 404 without a
/// token aborts with the actionable one-liner, and any other failure
/// aborts exactly as it did before ADR-0358.
///
/// `repo` is the repository slug both arms resolve under — `upgrade` and
/// the first-party channel pass [`REPO`], `--github` installs pass the
/// requested `owner/repo` (ADR-0449). `base` is the direct-route base the
/// caller resolved — usually [`release_download_base`] (or
/// [`github_download_base`] for another repository), the env seam's
/// value, arrived here as a parameter (the `load`/`load_with` injectable
/// pattern) so the channel install's post-signature steps (ADR-0431 §4
/// steps 3-4) drive against a loopback fixture without touching the
/// process environment; the fallback arm's API base stays env-resolved
/// (its own e2e drives it), and a 2xx probe — the arm every
/// injected-base caller drives — never reads it. `probe_asset` names the asset the probe requests; `needed`
/// is every asset the caller will download over the route, the probe
/// asset included. `manual_advice` — `Some(text)` — is appended to the
/// tokenless-404 abort, where `upgrade` points at its documented
/// reinstall one-liner; a flow with no manual remedy passes `None`.
pub(crate) fn probe_route_at(
    repo: &str,
    base: &str,
    tag: &str,
    probe_asset: &str,
    needed: &[&str],
    token: Option<&str>,
    manual_advice: Option<&str>,
) -> Result<RouteProbe, ChannelError> {
    let url = download_url(base, tag, probe_asset);
    let probe = download_asset(AssetRequest {
        url: &url,
        name: probe_asset,
        token,
        accept: None,
    });
    match probe_outcome(probe, token) {
        ProbeOutcome::Direct(probe_bytes) => Ok(RouteProbe {
            route: AssetRoute::Direct {
                base: base.to_string(),
                tag: tag.to_string(),
                token: token.map(str::to_owned),
            },
            probe_bytes,
            note: None,
        }),
        ProbeOutcome::Fallback(token) => {
            let api_base = super::api_assets::github_api_url()?;
            let ids = fetch_asset_ids(&api_base, repo, tag, needed, token)?;
            let route = AssetRoute::Api {
                base: api_base,
                repo: repo.to_string(),
                ids,
                token: token.to_string(),
            };
            // The probe's 404 fetched nothing, so the selected route
            // fetches the probe asset itself — once, through the route's
            // own download, the same path every later asset takes —
            // before the route is handed back.
            let probe_bytes = route.download(probe_asset)?;
            Ok(RouteProbe {
                route,
                probe_bytes,
                // ADR-0358 §6: the one stderr note line, returned for the
                // caller to print through its sanitized writer.
                note: Some(fallback_note(&url)),
            })
        }
        ProbeOutcome::Unauthenticated(status) => {
            Err(direct_route_unavailable(status, manual_advice))
        }
        ProbeOutcome::Failed(e) => Err(e.into_channel_error()),
    }
}

/// The 404-without-a-token abort (ADR-0358 §4): the typed status error's
/// own pinned text — the one source of `GET {url}: http status: {code}` —
/// plus the `GITHUB_TOKEN` advice and, when the caller has one, its manual
/// remedy (`upgrade`'s reinstall one-liner; the extension installer's
/// local `--from` pointer).
fn direct_route_unavailable(status: DownloadError, manual: Option<&str>) -> ChannelError {
    let manual = manual
        .map(|advice| format!(", or {advice}"))
        .unwrap_or_default();
    ChannelError::new(format!(
        "{}; the release assets may require authentication — set GITHUB_TOKEN and retry{manual}",
        status.into_channel_error()
    ))
}

/// The one fallback note line (ADR-0358 §6), written to stderr when the
/// probe's 404 selects the API asset route.
fn fallback_note(url: &str) -> String {
    format!("note: {url} returned 404; falling back to the authenticated API asset route")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_url_appends_the_exact_tag_and_asset() {
        // ADR-0250 §3: the base, the exact tag (leading `v` included) and
        // the asset name, path-appended.
        assert_eq!(
            download_url("http://127.0.0.1:9", "v0.10.0", "SHA256SUMS"),
            "http://127.0.0.1:9/v0.10.0/SHA256SUMS"
        );
    }

    #[test]
    fn the_canonical_base_is_the_adr_0250_route() {
        let base = canonical_download_base_for(REPO);
        assert_eq!(
            base, "https://github.com/relatedb/relate/releases/download",
            "the canonical base is the ADR-0250 §3 route"
        );
        // ADR-0449: any repository slug composes its own base.
        assert_eq!(
            canonical_download_base_for("owner/repo"),
            "https://github.com/owner/repo/releases/download"
        );
    }

    /// A typed status failure for the probe tests, with the URL the direct
    /// route builds.
    fn probe_status(code: u16) -> DownloadError {
        DownloadError::Status {
            url: "https://example.test/v0.10.0/SHA256SUMS".to_string(),
            code,
        }
    }

    #[test]
    fn probe_outcome_only_a_typed_404_with_a_token_falls_back() {
        // ADR-0358 §2: a 2xx probe serves the whole flow from the direct
        // route, token or no token.
        for token in [None, Some("ghp_test_token")] {
            match probe_outcome(Ok(b"sums".to_vec()), token) {
                ProbeOutcome::Direct(bytes) => assert_eq!(bytes.as_slice(), b"sums"),
                other => panic!("a 2xx probe must keep the direct route: {other:?}"),
            }
        }
        // ADR-0358 §3: the typed 404 with a token set falls back, carrying
        // that token.
        assert!(matches!(
            probe_outcome(Err(probe_status(404)), Some("ghp_test_token")),
            ProbeOutcome::Fallback("ghp_test_token")
        ));
        // ADR-0358 §4: the typed 404 without a token aborts, carrying the
        // status error whose pinned text the abort appends to.
        assert!(matches!(
            probe_outcome(Err(probe_status(404)), None),
            ProbeOutcome::Unauthenticated(DownloadError::Status { code: 404, .. })
        ));
        // ADR-0358 §5: every other status aborts exactly as before — never a
        // fallback, even with a token set — and keeps the pinned text. The
        // outcome carries the transport error; probe_route_at applies the one
        // into_channel_error conversion asserted here.
        for code in [401, 403, 429, 500, 503] {
            match probe_outcome(Err(probe_status(code)), Some("ghp_test_token")) {
                ProbeOutcome::Failed(err) => assert_eq!(
                    err.into_channel_error().to_string(),
                    format!("GET https://example.test/v0.10.0/SHA256SUMS: http status: {code}")
                ),
                other => panic!("status {code} must abort, not fall back: {other:?}"),
            }
        }
        // A non-status failure (transport, protocol, body read) is not a
        // route signal either.
        let transport = DownloadError::Other(ChannelError::new(
            "GET https://example.test/v0.10.0/SHA256SUMS: connection failed",
        ));
        match probe_outcome(Err(transport), Some("ghp_test_token")) {
            ProbeOutcome::Failed(err) => assert_eq!(
                err.into_channel_error().to_string(),
                "GET https://example.test/v0.10.0/SHA256SUMS: connection failed"
            ),
            other => panic!("a transport failure must abort: {other:?}"),
        }
    }

    #[test]
    fn the_status_keeps_the_pinned_error_text() {
        // The direct route's user-facing text is unchanged by the typed
        // status (ADR-0358 §5); the calling flow prefixes its command.
        assert_eq!(
            probe_status(404).into_channel_error().to_string(),
            "GET https://example.test/v0.10.0/SHA256SUMS: http status: 404"
        );
        assert_eq!(
            probe_status(500).into_channel_error().to_string(),
            "GET https://example.test/v0.10.0/SHA256SUMS: http status: 500"
        );
    }

    #[test]
    fn the_unauthenticated_404_abort_is_one_line_and_actionable() {
        // ADR-0358 §4: one line naming GITHUB_TOKEN and the caller's manual
        // remedy, built by appending to the typed status error's own
        // pinned text. A flow with no manual remedy omits the tail.
        for (manual, needle) in [
            (
                Some("reinstall manually: curl -sSfL https://example.test | bash"),
                "reinstall manually",
            ),
            (None, "retry"),
        ] {
            let msg = direct_route_unavailable(probe_status(404), manual).to_string();
            assert_eq!(msg.lines().count(), 1, "single line: {msg:?}");
            assert!(
                msg.starts_with("GET https://example.test/v0.10.0/SHA256SUMS: http status: 404;"),
                "the abort must carry the status error's own text: {msg:?}"
            );
            assert!(msg.contains("GITHUB_TOKEN"), "{msg:?}");
            assert!(msg.contains(needle), "{msg:?}");
        }
        // The advice tail is appended, never spliced into the status text.
        let with = direct_route_unavailable(
            probe_status(404),
            Some("reinstall manually: curl -sSfL https://example.test | bash"),
        )
        .to_string();
        assert!(
            with.ends_with("or reinstall manually: curl -sSfL https://example.test | bash"),
            "{with:?}"
        );
    }

    #[test]
    fn the_fallback_note_is_one_line_naming_the_fallback() {
        // ADR-0358 §6: the successful fallback prints one stderr note line.
        let url = "https://github.com/relatedb/relate/releases/download/v0.12.5/SHA256SUMS";
        let note = fallback_note(url);
        assert_eq!(note.lines().count(), 1, "single line: {note:?}");
        assert!(note.starts_with("note: "), "{note:?}");
        assert!(note.contains(url), "{note:?}");
        assert!(note.contains("returned 404"), "{note:?}");
        assert!(
            note.contains("falling back to the authenticated API asset route"),
            "{note:?}"
        );
    }
}
