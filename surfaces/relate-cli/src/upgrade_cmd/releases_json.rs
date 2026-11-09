//! The `releases.json` document contract (ADR-0250 §2): the static release
//! metadata published on the project website — its validation, the
//! asset-membership check on a parsed [`Release`], its URL selection, and the
//! fetch path that reads it. Everything here is pure except the fetch path
//! ([`read_releases_json`] and its caller [`fetch_latest_release`]); the
//! SHA256SUMS/signature/platform machinery, the
//! route probe, and the upgrade orchestration live in the parent module.

use serde::Deserialize;

use super::{parse_version_triple, UpgradeError, INSTALL_ONE_LINER};
use crate::release_channel::{seam, REPO};

/// Canonical release-metadata URL: the static `releases.json` published to
/// the project website (ADR-0250 §1).
const RELEASES_JSON_URL: &str = "https://relatedb.ai/releases.json";

/// Environment override for the metadata URL (a documented dev/test seam):
/// the one way a test can point release discovery at a fixture or mirror.
const RELEASES_JSON_URL_ENV: &str = "RELATE_RELEASES_JSON_URL";

/// Temporary fallback host (ADR-0550): the relate-web preview site, read
/// when the canonical URL does not yield a valid document — pre-launch,
/// the canonical host serves no `releases.json`. Removed with the preview
/// host after launch.
const RELEASES_JSON_FALLBACK_URL: &str = "https://r31473.relatedb.ai/releases.json";

/// Environment override for the fallback URL (the same dev/test seam as
/// the primary's); an empty value means the default fallback host.
const RELEASES_JSON_FALLBACK_URL_ENV: &str = "RELATE_RELEASES_JSON_FALLBACK_URL";

/// A `releases.json` release object (ADR-0250 §2), reduced to the fields
/// `upgrade` consumes. The `notes` body is validated at parse time but never
/// held: `upgrade` does not print it.
#[derive(Debug)]
pub(super) struct Release {
    pub(super) tag: String,
    assets: Vec<String>,
}

impl Release {
    fn has_asset(&self, name: &str) -> bool {
        self.assets.iter().any(|asset| asset == name)
    }

    /// Fail closed when the release's asset list does not list `name`
    /// (ADR-0250 §2, ADR-0115): the platform binary, `SHA256SUMS`, and
    /// `SHA256SUMS.sig` must all be present before anything is downloaded.
    /// Pure, so the missing-asset aborts are unit-testable without a socket.
    pub(super) fn require_asset(&self, name: &str) -> Result<(), UpgradeError> {
        if self.has_asset(name) {
            return Ok(());
        }
        // missing_asset / missing_signature: releases.json does not list an
        // asset with this name (platform binary, SHA256SUMS manifest, or its
        // signature).
        Err(UpgradeError::new(format!(
            "release {} from {REPO} does not list the asset {name}",
            self.tag
        )))
    }
}

/// A malformed-document failure (ADR-0250 §4): the fetch boundary appends
/// the URL and the reinstall pointer; the text here names the offending key.
fn malformed(detail: impl std::fmt::Display) -> UpgradeError {
    UpgradeError::new(format!("releases.json is malformed: {detail}"))
}

/// A release-discovery failure (ADR-0250 §4, ADR-0550): one line naming
/// both metadata URLs with each one's failure detail, and pointing at the
/// documented reinstall one-liner.
fn discovery_err(
    url: &str,
    primary: impl std::fmt::Display,
    fallback_url: &str,
    fallback: impl std::fmt::Display,
) -> UpgradeError {
    UpgradeError::new(format!(
        "could not read the release list from {url} ({primary}) or the fallback {fallback_url} ({fallback}); reinstall manually instead: {INSTALL_ONE_LINER}"
    ))
}

/// A required key as staged by the derive: presence is explicit so the
/// validation pass — not the derive — owns check order and the pinned §4
/// wording, and so `Missing` stays distinct from `Present(Value::Null)`
/// (ADR-0250 §2's required-but-nullable rule; an `Option<Value>` field
/// would conflate the two).
#[derive(Default)]
enum Staged {
    #[default]
    Missing,
    Present(serde_json::Value),
}

/// Deserialize a present key as [`Staged::Present`]; paired with
/// `#[serde(default)]` so a missing key is [`Staged::Missing`] instead of a
/// derive error.
fn staged<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Staged, D::Error> {
    Ok(Staged::Present(serde_json::Value::deserialize(
        deserializer,
    )?))
}

/// A `releases.json` release object as serde derives it (ADR-0250 §2): every
/// required key staged as a [`Staged`] so the validation pass in
/// [`parse_release_object`] runs the checks in the pinned §4 order and
/// wording at the site that knows the dotted key path — serde's own
/// diagnostics carry neither. Unknown keys are ignored (the §2
/// forward-compat rule).
#[derive(Deserialize)]
struct ReleaseObject {
    #[serde(default, deserialize_with = "staged")]
    tag: Staged,
    #[serde(default, deserialize_with = "staged")]
    published_at: Staged,
    #[serde(default, deserialize_with = "staged")]
    prerelease: Staged,
    #[serde(default, deserialize_with = "staged")]
    notes: Staged,
    #[serde(default, deserialize_with = "staged")]
    assets: Staged,
}

/// The `releases.json` document as serde derives it (ADR-0250 §2), with the
/// same staging conventions as [`ReleaseObject`]. `latest` and
/// `latest_prerelease` are required but nullable (§2): [`Staged`] keeps a
/// missing key distinct from a `null` one, and the `null` cases are checked
/// explicitly in [`parse_releases`].
#[derive(Deserialize)]
struct ReleasesDocument {
    #[serde(default, deserialize_with = "staged")]
    schema_version: Staged,
    #[serde(default, deserialize_with = "staged")]
    generated_at: Staged,
    #[serde(default, deserialize_with = "staged")]
    latest_prerelease: Staged,
    #[serde(default, deserialize_with = "staged")]
    releases: Staged,
    #[serde(default, deserialize_with = "staged")]
    latest: Staged,
}

/// A JSON field type serde can check, paired with its ADR-0250 §2 schema
/// wording so the type and its error text cannot drift apart.
trait FieldType: serde::de::DeserializeOwned {
    /// The schema's name for the type, as in "`key` must be …".
    const WHAT: &'static str;
}

impl FieldType for String {
    const WHAT: &'static str = "a string";
}

impl FieldType for bool {
    const WHAT: &'static str = "a boolean";
}

impl FieldType for Vec<String> {
    const WHAT: &'static str = "an array of strings";
}

impl FieldType for Vec<serde_json::Value> {
    const WHAT: &'static str = "an array of release objects";
}

impl FieldType for u64 {
    const WHAT: &'static str = "the integer 1";
}

/// The staged presence check (ADR-0250 §2): a required key absent from the
/// document fails with the pinned §4 wording for the dotted key path
/// `key_path`; a present key — `null` included — yields its value.
fn present(staged: Staged, key_path: &str) -> Result<serde_json::Value, UpgradeError> {
    match staged {
        Staged::Present(value) => Ok(value),
        Staged::Missing => Err(malformed(format!("missing required key {key_path}"))),
    }
}

/// The two checks the schema makes on a required key — presence, then type —
/// in the pinned ADR-0250 §4 wording, with serde performing the type
/// discrimination.
fn required<T: FieldType>(staged: Staged, key_path: &str) -> Result<T, UpgradeError> {
    let what = T::WHAT;
    serde_json::from_value(present(staged, key_path)?)
        .map_err(|_| malformed(format!("{key_path} must be {what}")))
}

/// A derive failure here can only be a duplicated key — presence is staged,
/// never derive-enforced; every staged value deserializes as
/// [`serde_json::Value`]; and the document was checked to be an object — so
/// serde's own text is kept.
fn derive_err(e: serde_json::Error) -> UpgradeError {
    malformed(e.to_string())
}

/// Validate one release object (ADR-0250 §2) and extract the fields
/// `upgrade` consumes. `path` names the object in error messages (`latest`,
/// `releases[0]`, `latest_prerelease`).
fn parse_release_object(value: &serde_json::Value, path: &str) -> Result<Release, UpgradeError> {
    if !value.is_object() {
        return Err(malformed(format!("{path} is not a JSON object")));
    }
    let wire = ReleaseObject::deserialize(value).map_err(derive_err)?;
    // Check order is the pinned §4 behavior: each key's presence and type
    // checks complete before the next key is looked at.
    let tag = required::<String>(wire.tag, &format!("{path}.tag"))?;
    if tag.is_empty() {
        return Err(malformed(format!("{path}.tag is empty")));
    }
    // `published_at`, `prerelease`, and `notes` are required by the schema
    // but not consumed by `upgrade`: a document missing one or carrying one
    // with the wrong type is still malformed (ADR-0250 §4). `notes` is
    // validated and dropped — `Release` never holds the body.
    required::<String>(wire.published_at, &format!("{path}.published_at"))?;
    required::<bool>(wire.prerelease, &format!("{path}.prerelease"))?;
    required::<String>(wire.notes, &format!("{path}.notes"))?;
    let assets = required::<Vec<String>>(wire.assets, &format!("{path}.assets"))?;
    Ok(Release { tag, assets })
}

/// Parse and validate a `releases.json` body (ADR-0250 §2) and return its
/// `latest` release together with the version triple proven from its tag at
/// this parse boundary — the caller compares triples directly and never
/// re-parses the tag. Pure: the HTTP fetch is a separate boundary. Unknown
/// keys are ignored at every level (serde's derive default), so the website
/// schema can grow without breaking older CLIs (ADR-0250 §2).
pub(super) fn parse_releases(json: &str) -> Result<(Release, (u64, u64, u64)), UpgradeError> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| malformed(format!("invalid JSON: {e}")))?;
    if !value.is_object() {
        return Err(malformed("the document is not a JSON object"));
    }
    let doc = ReleasesDocument::deserialize(&value).map_err(derive_err)?;
    // Check order is the pinned §4 behavior: each key's presence, type, and
    // value checks complete before the next key is looked at.
    let schema_version = required::<u64>(doc.schema_version, "schema_version")?;
    if schema_version != 1 {
        return Err(malformed(format!(
            "unsupported schema_version {schema_version} (expected 1)"
        )));
    }
    required::<String>(doc.generated_at, "generated_at")?;
    // latest_prerelease is a reserved slot the CLI does not consume, but a
    // document missing it is malformed (ADR-0250 §4); when present and
    // non-null it must still be a well-formed release object.
    let latest_prerelease = present(doc.latest_prerelease, "latest_prerelease")?;
    if !latest_prerelease.is_null() {
        parse_release_object(&latest_prerelease, "latest_prerelease")?;
    }
    let releases = required::<Vec<serde_json::Value>>(doc.releases, "releases")?;
    for (i, item) in releases.iter().enumerate() {
        parse_release_object(item, &format!("releases[{i}]"))?;
    }
    // ADR-0250 §4: a valid document with no stable release published still
    // has nothing to compare against, so it aborts.
    let latest = present(doc.latest, "latest")?;
    if latest.is_null() {
        return Err(malformed("latest is null: no stable release is published"));
    }
    let latest = parse_release_object(&latest, "latest")?;
    // ADR-0250 §2: latest.tag is the exact stable tag — `v` plus an `X.Y.Z`
    // numeric triple; anything else is a malformed document. The version
    // comparison itself keeps its ADR-0069 rules.
    let Some(version) = latest.tag.strip_prefix('v').and_then(parse_version_triple) else {
        return Err(malformed(format!(
            "latest.tag is not a vX.Y.Z release tag: '{}'",
            latest.tag
        )));
    };
    Ok((latest, version))
}

/// The metadata URL to read: `RELATE_RELEASES_JSON_URL` when set and
/// non-empty (the documented dev/test seam), else the canonical website URL
/// (ADR-0250 §1). A set but non-UTF-8 value is a typed error naming the
/// variable.
fn releases_json_url() -> Result<String, UpgradeError> {
    Ok(seam(RELEASES_JSON_URL_ENV, RELEASES_JSON_URL)?)
}

/// The fallback metadata URL (ADR-0550): `RELATE_RELEASES_JSON_FALLBACK_URL`
/// when set and non-empty, else the preview host.
fn releases_json_fallback_url() -> Result<String, UpgradeError> {
    Ok(seam(
        RELEASES_JSON_FALLBACK_URL_ENV,
        RELEASES_JSON_FALLBACK_URL,
    )?)
}

/// Fetch and parse one host's `releases.json`. The error is the bare
/// failure detail; the caller owns naming the URL.
fn read_releases_json(url: &str) -> Result<(Release, (u64, u64, u64)), UpgradeError> {
    let mut body = ureq::get(url)
        .call()
        .map_err(|e| UpgradeError::new(format!("GET failed: {e}")))?
        .into_body();
    let json = body
        .read_to_string()
        .map_err(|e| UpgradeError::new(format!("reading the response failed: {e}")))?;
    parse_releases(&json)
}

/// Fetch and parse the release metadata from the website's `releases.json`
/// (ADR-0250), falling back to the temporary preview host when the
/// canonical URL does not yield a valid document (ADR-0550 — any primary
/// failure: transport, read, or a malformed document such as the
/// pre-launch lander page). The request never carries an `Authorization`
/// header — the file is public and lives on a different origin than the
/// asset downloads (ADR-0250 §5) — and there is no retry, cache, or
/// GitHub API fallback.
pub(super) fn fetch_latest_release() -> Result<(Release, (u64, u64, u64)), UpgradeError> {
    let url = releases_json_url()?;
    match read_releases_json(&url) {
        Ok(found) => Ok(found),
        Err(primary) => {
            let fallback_url = releases_json_fallback_url()?;
            read_releases_json(&fallback_url)
                .map_err(|fallback| discovery_err(&url, &primary, &fallback_url, &fallback))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed `releases.json` fixture (ADR-0250 §2): the same shape
    /// the website publisher writes. The parser tests below read this file,
    /// not an inline copy.
    const FIXTURE: &str = include_str!("../../tests/fixtures/releases.json");

    /// The fixture's `latest` object, on one line, as the mutation anchor for
    /// the malformed-document cases.
    const MINIMAL_LATEST: &str = r#"{"tag": "v0.10.0", "published_at": "2026-10-15T11:58:00Z", "notes": "", "prerelease": false, "assets": ["SHA256SUMS"]}"#;

    /// A minimal valid `releases.json` document for mutation-based tests
    /// (the committed fixture stays the realistic one).
    fn minimal_doc() -> String {
        format!(
            "{{\n  \"schema_version\": 1,\n  \"generated_at\": \"2026-10-15T12:00:00Z\",\n  \"latest\": {MINIMAL_LATEST},\n  \"latest_prerelease\": null,\n  \"releases\": []\n}}"
        )
    }

    /// The `latest.notes` of a `releases.json` document, parsed directly:
    /// the production `Release` deliberately does not hold the body, so the
    /// fixture's notes content is asserted through this test-local helper.
    fn latest_notes(json: &str) -> String {
        let value: serde_json::Value = serde_json::from_str(json).unwrap();
        value["latest"]["notes"].as_str().unwrap().to_string()
    }

    #[test]
    fn parse_releases_reads_the_committed_fixture() {
        let (release, version) = parse_releases(FIXTURE).unwrap();
        assert_eq!(release.tag, "v0.10.0");
        // The tag's version triple is proven at the parse boundary.
        assert_eq!(version, (0, 10, 0));
        // The notes body is required by the document contract and validated
        // at parse time, but never held on `Release` (the CLI never prints
        // it); assert its content directly from the fixture.
        let notes = latest_notes(FIXTURE);
        assert!(
            notes.starts_with("### Highlights"),
            "unexpected notes: {notes:?}"
        );
        assert!(
            notes.contains("Faster multi-hop aggregates"),
            "unexpected notes: {notes:?}"
        );
        // The three assets `upgrade` needs are listed in the fixture.
        for asset in ["relate-darwin-arm64", "SHA256SUMS", "SHA256SUMS.sig"] {
            release.require_asset(asset).unwrap();
        }
        // An asset the release does not list fails closed.
        let err = release.require_asset("relate-linux-riscv64").unwrap_err();
        assert!(
            err.stderr_message().contains("relate-linux-riscv64"),
            "unexpected error: {}",
            err.stderr_message()
        );
    }

    #[test]
    fn parse_releases_accepts_a_null_latest_prerelease() {
        // ADR-0250 §2: `latest_prerelease` is nullable. `null` (no published
        // prerelease) is a valid document, not a malformed one — the
        // committed fixture exercises the non-null variant.
        let (release, _) = parse_releases(&minimal_doc()).unwrap();
        assert_eq!(release.tag, "v0.10.0");
    }

    #[test]
    fn parse_releases_ignores_unknown_fields_at_both_levels() {
        // Forward compatibility (ADR-0250 §2): additive keys — at the top
        // level and inside a release object — must not break an older CLI.
        let with_unknown = FIXTURE
            .replacen(
                "\"schema_version\": 1,",
                "\"schema_version\": 1,\n  \"website_build\": \"2026-10-15\",",
                1,
            )
            .replacen(
                "\"prerelease\": false,",
                "\"changelog_url\": \"https://example.test/changelog\", \"prerelease\": false,",
                1,
            );
        assert_ne!(
            with_unknown, FIXTURE,
            "the fixture must carry both mutation anchors"
        );
        let (release, _) = parse_releases(&with_unknown).unwrap();
        assert_eq!(release.tag, "v0.10.0");
        assert!(latest_notes(&with_unknown).starts_with("### Highlights"));
    }

    #[test]
    fn parse_releases_rejects_malformed_documents() {
        let cases: &[(&str, String, &str)] = &[
            ("invalid JSON", "not json".to_string(), "invalid JSON"),
            ("not a JSON object", "[]".to_string(), "not a JSON object"),
            ("empty object", "{}".to_string(), "schema_version"),
            (
                "schema_version 2",
                minimal_doc().replace("\"schema_version\": 1", "\"schema_version\": 2"),
                "schema_version 2",
            ),
            (
                "schema_version is a string",
                minimal_doc().replace("\"schema_version\": 1", "\"schema_version\": \"1\""),
                "schema_version",
            ),
            (
                "missing generated_at",
                minimal_doc().replace("  \"generated_at\": \"2026-10-15T12:00:00Z\",\n", ""),
                "missing required key generated_at",
            ),
            (
                "missing latest",
                minimal_doc().replace(&format!("  \"latest\": {MINIMAL_LATEST},\n"), ""),
                "missing required key latest",
            ),
            (
                "null latest",
                minimal_doc().replace(MINIMAL_LATEST, "null"),
                "latest is null",
            ),
            (
                "latest is not an object",
                minimal_doc().replace(MINIMAL_LATEST, "5"),
                "latest is not a JSON object",
            ),
            (
                "malformed latest_prerelease",
                minimal_doc().replace(
                    "\"latest_prerelease\": null",
                    "\"latest_prerelease\": {\"tag\": \"v0.11.0-rc.1\"}",
                ),
                "latest_prerelease.published_at",
            ),
            (
                "missing latest_prerelease",
                minimal_doc().replace("  \"latest_prerelease\": null,\n", ""),
                "missing required key latest_prerelease",
            ),
            (
                "missing releases",
                minimal_doc().replace(",\n  \"releases\": []", ""),
                "missing required key releases",
            ),
            (
                "releases is not an array",
                minimal_doc().replace("\"releases\": []", "\"releases\": {}"),
                "releases must be an array",
            ),
            (
                "latest.tag without the v prefix",
                minimal_doc().replace("v0.10.0", "0.10.0"),
                "not a vX.Y.Z release tag",
            ),
            (
                "latest.tag is not a triple",
                minimal_doc().replace("v0.10.0", "v0.10"),
                "not a vX.Y.Z release tag",
            ),
            (
                "latest.tag is empty",
                minimal_doc().replace("v0.10.0", ""),
                "latest.tag is empty",
            ),
            (
                "latest.tag is not a string",
                minimal_doc().replace("\"tag\": \"v0.10.0\"", "\"tag\": 10"),
                "latest.tag must be a string",
            ),
            (
                "latest.notes is null",
                minimal_doc().replace("\"notes\": \"\"", "\"notes\": null"),
                "latest.notes must be a string",
            ),
            (
                "latest.assets is not an array",
                minimal_doc().replace("\"assets\": [\"SHA256SUMS\"]", "\"assets\": \"SHA256SUMS\""),
                "latest.assets must be an array",
            ),
            (
                "latest.prerelease is not a boolean",
                minimal_doc().replace("\"prerelease\": false", "\"prerelease\": \"false\""),
                "latest.prerelease must be a boolean",
            ),
            (
                "releases entry is malformed",
                minimal_doc().replace("\"releases\": []", "\"releases\": [{\"tag\": \"v0.9.0\"}]"),
                "missing required key releases[0].published_at",
            ),
        ];
        for (name, json, needle) in cases {
            let err = parse_releases(json).unwrap_err().stderr_message();
            assert!(
                err.starts_with("Error: upgrade: releases.json is malformed:"),
                "case {name}: unexpected error: {err:?}"
            );
            assert!(
                err.contains(needle),
                "case {name}: expected {needle:?} in {err:?}"
            );
        }
    }

    #[test]
    fn discovery_error_names_both_urls_and_the_reinstall_one_liner() {
        // ADR-0250 §4 + ADR-0550: with both hosts failing, the abort is one
        // line naming the primary and the fallback URL with each one's
        // detail, and pointing at the documented reinstall one-liner.
        let err = discovery_err(
            "https://example.test/releases.json",
            "GET failed: connection refused",
            "https://preview.example.test/releases.json",
            "releases.json is malformed: invalid JSON",
        )
        .stderr_message();
        assert_eq!(err.lines().count(), 1, "one line: {err:?}");
        assert!(err.starts_with("Error: upgrade: "), "{err:?}");
        assert!(
            err.contains("https://example.test/releases.json"),
            "{err:?}"
        );
        assert!(
            err.contains("https://preview.example.test/releases.json"),
            "{err:?}"
        );
        assert!(err.contains("connection refused"), "{err:?}");
        assert!(err.contains("invalid JSON"), "{err:?}");
        assert!(err.contains(INSTALL_ONE_LINER), "{err:?}");
    }

    /// Serializes the env-mutating fetch tests (one process, one
    /// environment).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// One fetch test's environment: the two seam guards (captured before
    /// the sets, restored on drop) behind the serializing lock.
    struct SeamGuard {
        _primary: relate_test_support::EnvRestore,
        _fallback: relate_test_support::EnvRestore,
    }

    impl SeamGuard {
        fn set(primary: &str, fallback: &str) -> SeamGuard {
            let guard = SeamGuard {
                _primary: relate_test_support::EnvRestore::new(RELEASES_JSON_URL_ENV),
                _fallback: relate_test_support::EnvRestore::new(RELEASES_JSON_FALLBACK_URL_ENV),
            };
            std::env::set_var(RELEASES_JSON_URL_ENV, primary);
            std::env::set_var(RELEASES_JSON_FALLBACK_URL_ENV, fallback);
            guard
        }
    }

    /// Serve one body once on loopback; returns the join handle (joined by
    /// the caller so a server-side panic propagates) and the URL to GET.
    fn serve_once(body: &'static str) -> (std::thread::JoinHandle<Vec<String>>, String) {
        let (server, port) = relate_test_support::loopback::spawn_single_response("200 OK", body);
        (server, format!("http://127.0.0.1:{port}/releases.json"))
    }

    #[test]
    fn fetch_falls_back_when_the_primary_document_is_invalid() {
        // ADR-0550: the pre-launch case — the canonical host serves the
        // lander page (not a releases.json), the preview host serves the
        // document.
        let _lock = ENV_LOCK.lock().unwrap();
        let (lander, lander_url) = serve_once("<!DOCTYPE html><html><head></head></html>");
        let (preview, preview_url) = serve_once(FIXTURE);
        let _seams = SeamGuard::set(&lander_url, &preview_url);
        let (release, version) = fetch_latest_release().unwrap();
        assert_eq!(release.tag, "v0.10.0");
        assert_eq!(version, (0, 10, 0));
        lander.join().unwrap();
        preview.join().unwrap();
    }

    #[test]
    fn fetch_falls_back_on_a_primary_transport_failure() {
        let _lock = ENV_LOCK.lock().unwrap();
        let (preview, preview_url) = serve_once(FIXTURE);
        // Port 9 (discard) refuses: the primary read never happens.
        let _seams = SeamGuard::set("http://127.0.0.1:9/releases.json", &preview_url);
        let (release, _) = fetch_latest_release().unwrap();
        assert_eq!(release.tag, "v0.10.0");
        preview.join().unwrap();
    }

    #[test]
    fn fetch_does_not_consult_the_fallback_when_the_primary_is_valid() {
        let _lock = ENV_LOCK.lock().unwrap();
        let (primary, primary_url) = serve_once(FIXTURE);
        // The fallback refuses; a consult would fail the fetch.
        let _seams = SeamGuard::set(&primary_url, "http://127.0.0.1:9/releases.json");
        let (release, _) = fetch_latest_release().unwrap();
        assert_eq!(release.tag, "v0.10.0");
        primary.join().unwrap();
    }

    #[test]
    fn fetch_reports_both_urls_when_both_fail() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _seams = SeamGuard::set(
            "http://127.0.0.1:9/releases.json",
            "http://127.0.0.1:9/releases.json",
        );
        let err = fetch_latest_release().unwrap_err().stderr_message();
        assert_eq!(err.lines().count(), 1, "one line: {err:?}");
        assert!(err.contains("http://127.0.0.1:9/releases.json"), "{err:?}");
        assert!(err.contains("or the fallback"), "{err:?}");
        assert!(err.contains(INSTALL_ONE_LINER), "{err:?}");
    }
}
