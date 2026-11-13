//! The `releases.json` document contract (ADR-0250 §2): the static release
//! metadata published on the project website — its validation, the
//! asset-membership check on a parsed [`Release`], its URL selection, and the
//! one fetch that reads it. Everything here is pure except
//! [`fetch_latest_release`]; the SHA256SUMS/signature/platform machinery, the
//! route probe, and the upgrade orchestration live in the parent module.

use serde::Deserialize;

use super::{parse_version_triple, UpgradeError};
use crate::release_channel::{
    http::{download_asset, AssetRequest, DownloadError},
    seam, REPO,
};

/// Canonical release-metadata URL: the static `releases.json` published to
/// the project website (ADR-0250 §1).
const RELEASES_JSON_URL: &str = "https://relatedb.ai/releases.json";

/// Environment override for the metadata URL (a documented dev/test seam):
/// the one way a test can point release discovery at a fixture or mirror.
const RELEASES_JSON_URL_ENV: &str = "RELATE_RELEASES_JSON_URL";

/// A `releases.json` release object (ADR-0250 §2), reduced to the fields
/// `upgrade` consumes. The `notes` body is validated at parse time but never
/// held: `upgrade` does not print it.
#[derive(Debug)]
pub(super) struct Release {
    /// The tag, stripped of control characters at the parse boundary: it is
    /// the one copy every consumer reads — the console lines, the download
    /// URLs, the manifest binding, this type's own refusals — and a
    /// compromised document could embed ESC/CSI sequences to spoof output.
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

/// A release-discovery failure (ADR-0250 §4): one line naming the metadata
/// URL and pointing at the documented reinstall one-liner. Reserved for the
/// document-shape failures (UTF-8 and parse), whose detail names no URL;
/// the read failure carries the shared client's own URL-naming text
/// through [`read_err`], so each wrapper owns exactly one detail shape.
fn discovery_err(url: &str, detail: impl std::fmt::Display) -> UpgradeError {
    UpgradeError::reinstall(format!(
        "could not read the release list from {url}: {detail}"
    ))
}

/// A release-discovery read failure (ADR-0250 §4): one line carrying the
/// shared client's pinned failure text — every [`DownloadError`] arm names
/// the request URL — and pointing at the documented reinstall one-liner.
fn read_err(e: DownloadError) -> UpgradeError {
    UpgradeError::reinstall(format!(
        "could not read the release list: {}",
        e.into_channel_error()
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
    // Sanitized before any check, so validation and every consumer see the
    // same value: a tag of only control characters is an empty tag.
    let tag = relate_formatter::strip_unsafe_output_chars(&required::<String>(
        wire.tag,
        &format!("{path}.tag"),
    )?);
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
/// (ADR-0250 §1). There is no fallback between the two; a set but non-UTF-8
/// value is a typed error naming the variable.
fn releases_json_url() -> Result<String, UpgradeError> {
    Ok(seam(RELEASES_JSON_URL_ENV, RELEASES_JSON_URL)?)
}

/// Fetch and parse the release metadata from the website's `releases.json`
/// (ADR-0250). The read rides the channel's one HTTP client
/// ([`download_asset`] — its transport pins and body cap apply — with no
/// `Authorization` header: the file is public and lives on a different
/// origin than the asset downloads, ADR-0250 §5) and there is no retry,
/// cache, or GitHub API fallback.
pub(super) fn fetch_latest_release() -> Result<(Release, (u64, u64, u64)), UpgradeError> {
    let url = releases_json_url()?;
    let bytes = download_asset(AssetRequest {
        url: &url,
        name: "releases.json",
        token: None,
        accept: None,
    })
    .map_err(read_err)?;
    std::str::from_utf8(&bytes)
        .map_err(|e| malformed(format!("the document is not UTF-8: {e}")))
        .and_then(parse_releases)
        .map_err(|e| discovery_err(&url, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    // Test-only pin from its owning module: the reinstall one-liner,
    // present in every abort the constructor renders.
    use super::super::INSTALL_ONE_LINER;

    /// The committed `releases.json` fixture (ADR-0250 §2): the same shape
    /// the website publisher writes. The parser tests below read this file,
    /// not an inline copy.
    const FIXTURE: &str = include_str!("../../tests/fixtures/releases.json");

    #[test]
    fn release_tag_is_sanitized_at_the_parse_boundary() {
        // The one sanitizing site: a control character in a release tag never
        // survives into the `Release` every consumer reads.
        let object = serde_json::json!({
            "tag": "v0.5.0\u{1b}[31m",
            "published_at": "2026-10-15T11:58:00Z",
            "notes": "",
            "prerelease": false,
            "assets": [],
        });
        let release = parse_release_object(&object, "latest_prerelease").unwrap();
        assert_eq!(release.tag, "v0.5.0[31m");
        // Validation reads the sanitized value: control characters alone
        // are an empty tag.
        let mut only_controls = object.clone();
        only_controls["tag"] = serde_json::json!("\u{1b}\u{7}");
        let err = parse_release_object(&only_controls, "latest_prerelease").unwrap_err();
        assert!(
            err.to_string().contains("latest_prerelease.tag is empty"),
            "{err}"
        );
    }

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
        assert_eq!(release.tag, "v0.0.9");
        // The tag's version triple is proven at the parse boundary.
        assert_eq!(version, (0, 0, 9));
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
        // committed fixture exercises the non-null variant. The minimal
        // doc's tag is parse-only data (no version comparison here), so it
        // keeps its own v0.10.0.
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
        assert_eq!(release.tag, "v0.0.9");
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
    fn discovery_error_names_the_url_and_the_reinstall_one_liner() {
        // ADR-0250 §4: missing or malformed aborts with one line naming the
        // URL and pointing at the documented reinstall one-liner.
        let err = discovery_err(
            "https://example.test/releases.json",
            "GET failed: connection refused",
        )
        .stderr_message();
        assert_eq!(err.lines().count(), 1, "one line: {err:?}");
        assert!(err.starts_with("Error: upgrade: "), "{err:?}");
        assert!(
            err.contains("https://example.test/releases.json"),
            "{err:?}"
        );
        assert!(err.contains(INSTALL_ONE_LINER), "{err:?}");
    }

    #[test]
    fn read_error_carries_the_channel_text_and_the_reinstall_one_liner() {
        // ADR-0250 §4: the read failure is one line carrying the shared
        // client's pinned text — which names the URL, whatever the arm —
        // plus the reinstall one-liner; the URL-naming discovery_err is
        // not layered on top of it. The status refusal, the transport
        // failure, and the body-read failure are the three arm shapes
        // read_err meets, each produced through the client's own
        // construction forms so the fixtures cannot drift from the
        // producer's text.
        let cases = [
            (
                "status refusal",
                DownloadError::Status {
                    url: "https://example.test/releases.json".to_string(),
                    code: 404,
                },
                "http status: 404",
            ),
            (
                "transport failure",
                DownloadError::transport(
                    "https://example.test/releases.json",
                    "connection refused",
                ),
                "connection refused",
            ),
            (
                "body-read failure",
                DownloadError::body_read(
                    "https://example.test/releases.json",
                    "releases.json",
                    "connection closed",
                ),
                "downloading releases.json",
            ),
        ];
        for (name, error, needle) in cases {
            let err = read_err(error).stderr_message();
            assert_eq!(err.lines().count(), 1, "{name}: one line: {err:?}");
            assert!(err.starts_with("Error: upgrade: "), "{name}: {err:?}");
            assert!(
                err.contains("https://example.test/releases.json"),
                "{name}: {err:?}"
            );
            assert!(err.contains(needle), "{name}: {err:?}");
            assert!(err.contains(INSTALL_ONE_LINER), "{name}: {err:?}");
        }
    }
}
