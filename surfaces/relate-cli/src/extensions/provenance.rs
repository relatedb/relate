//! The provenance marker (ADR-0431 §7): the small JSON file every verb
//! install writes beside the manifest, recording the tier the package
//! arrived at and the source reference — the channel tag, the URL, or the
//! directory path. `extension list` reads it (`extensions`-feature,
//! dependency-free); `remove` requires it; `install` writes it
//! (`extension-install`). The marker is the `skill install` marker's
//! (ADR-0261) data-carrying sibling: presence proves the directory was
//! installed by the verb, content records how.
//!
//! Compiled under `extensions` alone so the dependency-free verbs can
//! read it in every bundle that has the registry (ADR-0431 §3).

use serde::Deserialize;

/// The provenance marker's filename, beside `extension.json` in the
/// package directory (ADR-0431 §7).
pub(crate) const MARKER_FILE: &str = ".relate-provenance.json";

/// A refused marker read (ADR-0431 §7): typed variants with pinned
/// texts, so the verbs that read the marker render the same refusal for
/// the same corruption — never a stringly error each caller words its
/// own way.
#[derive(Debug)]
pub(crate) enum ProvenanceError {
    /// The marker exists but could not be read.
    Unreadable {
        /// The marker's path, as the refusal names it.
        path: std::path::PathBuf,
        /// The read failure.
        source: std::io::Error,
    },
    /// The marker's content does not decode: corruption must not wear
    /// the honest copied-in state ([`Provenance::read`]'s rule).
    Invalid {
        /// The marker's path, as the refusal names it.
        path: std::path::PathBuf,
        /// The decode failure, naming the offending value.
        source: serde_json::Error,
    },
}

impl std::fmt::Display for ProvenanceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable { path, source } => {
                write!(
                    f,
                    "cannot read the provenance marker {}: {source}",
                    path.display()
                )
            }
            Self::Invalid { path, source } => {
                write!(f, "invalid provenance marker {}: {source}", path.display())
            }
        }
    }
}

/// The trust tier a package arrived by (ADR-0431 §7, ADR-0449): one ladder
/// — first-party (the signed channel, automatic verification §1),
/// github-pin (`--github`: the resolved tag/commit/asset-SHA256 recorded
/// and enforced by the per-user trust store), third-party URL (the
/// explicit act, §6), local directory (the explicit local act, §5). The
/// closed kebab-case vocabulary the marker stores; an unknown value fails
/// the decode, never a silent tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Tier {
    /// The signed first-party channel (§1): verified automatically against
    /// the release signing root.
    FirstParty,
    /// A GitHub release pinned to a commit (ADR-0449): the resolved pin is
    /// recorded and a moved tag/asset refuses without re-approval.
    GithubPin,
    /// A third-party URL (§6): the explicit trust act — the invocation
    /// itself; loud output and recorded provenance carry the record.
    ThirdParty,
    /// A local directory (§5): the explicit local act — whoever provisions
    /// the directory owns what is in it.
    Local,
}

impl Tier {
    /// The tier's listing label (ADR-0431 §6: `extension list` shows every
    /// installed package's tier, so a third-party install is never mistaken
    /// for a first-party one).
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Tier::FirstParty => "first-party",
            Tier::GithubPin => "github-pin",
            Tier::ThirdParty => "third-party",
            Tier::Local => "local",
        }
    }
}

/// A GitHub install's resolved pin (ADR-0449): the repository, the release
/// tag, the commit the tag resolved to, and the platform asset's SHA256 —
/// recorded in the provenance marker and in the per-user trust store.
/// `camelCase` on disk (`assetSha256`), the marker's committed format.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Pin {
    /// `owner/repo` the release belongs to.
    pub(crate) repo: String,
    /// The release tag whose asset was installed.
    pub(crate) tag: String,
    /// The commit the tag resolved to (40 lowercase hex).
    pub(crate) commit: String,
    /// The platform asset's SHA256 (64 lowercase hex) the downloaded bytes
    /// verified against.
    pub(crate) asset_sha256: String,
}

impl Pin {
    /// The pin's stable on-disk rendering (the marker and the trust
    /// store share it) — asserted on decode, so a field rename cannot
    /// drift the format.
    pub(crate) fn write_json(&self) -> String {
        format!(
            "{{\"repo\":{},\"tag\":{},\"commit\":{},\"assetSha256\":{}}}",
            serde_json::json!(self.repo),
            serde_json::json!(self.tag),
            serde_json::json!(self.commit),
            serde_json::json!(self.asset_sha256)
        )
    }
}

/// The marker's content: the tier and the source reference — the channel
/// tag, the GitHub pin, the URL, or the directory path (ADR-0431 §7,
/// ADR-0449).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Provenance {
    /// The tier the package arrived by.
    pub(crate) tier: Tier,
    /// The source reference: the release tag for a channel install,
    /// `repo@tag` for a GitHub install, the URL for a `--url` install, the
    /// directory path for a `--from` install — recorded verbatim as the
    /// source named it.
    pub(crate) source: String,
    /// The GitHub install's resolved pin (ADR-0449); absent for every
    /// other tier, whose source reference is the whole record. Older
    /// markers decode unchanged (the field defaults to absent).
    #[serde(default)]
    pub(crate) pin: Option<Pin>,
}

impl Provenance {
    /// The provenance of the package at `package_dir`, from its marker:
    /// `Ok(None)` when the package carries no marker — the copied-in state
    /// (ADR-0431 §5: an installed extension is just a directory, so an
    /// image can `COPY` one in; such a package runs but is unmanaged —
    /// `remove` refuses it, `list` shows it unmarked). A malformed marker
    /// is a refusal naming the package, never an unmarked read: corruption
    /// must not wear the honest copied-in state.
    pub(crate) fn read(package_dir: &std::path::Path) -> Result<Option<Self>, ProvenanceError> {
        let marker_path = package_dir.join(MARKER_FILE);
        let text = match std::fs::read_to_string(&marker_path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(ProvenanceError::Unreadable {
                    path: marker_path,
                    source,
                })
            }
        };
        let provenance: Provenance =
            serde_json::from_str(&text).map_err(|source| ProvenanceError::Invalid {
                path: marker_path,
                source,
            })?;
        Ok(Some(provenance))
    }

    /// Write the marker into `package_dir` (the staged directory, before the
    /// rename that lands the package — ADR-0431 §4 step 5): only a fully
    /// written package is marked, so a half-finished install never claims
    /// a directory it may not later be allowed to clean up (the
    /// `skill install` marker rule, ADR-0261).
    pub(crate) fn write(&self, package_dir: &std::path::Path) -> std::io::Result<()> {
        // A flat object, not the raw struct: the marker is a committed
        // on-disk format, so it renders explicitly and never rides a
        // derive's Serialize (a field rename cannot drift it). The pin
        // rides only for a github-pin install; every other tier's marker
        // is byte-for-byte the two-field object it always was.
        let pin = self
            .pin
            .as_ref()
            .map(|pin| format!(",\"pin\":{}", pin.write_json()))
            .unwrap_or_default();
        let text = format!(
            "{{\"tier\":\"{}\",\"source\":{}{pin}}}\n",
            self.tier.as_str(),
            serde_json::json!(self.source)
        );
        std::fs::write(package_dir.join(MARKER_FILE), text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The listing label and the marker's serde spelling are one
    /// vocabulary: each tier's `as_str` decodes back to the same tier, so
    /// the hand table cannot drift from the derive.
    #[test]
    fn a_tiers_label_is_its_serde_spelling() {
        for tier in [
            Tier::FirstParty,
            Tier::GithubPin,
            Tier::ThirdParty,
            Tier::Local,
        ] {
            let decoded: Tier = serde_json::from_str(&format!("\"{}\"", tier.as_str())).unwrap();
            assert_eq!(decoded, tier);
        }
    }

    /// A package directory under `tmp` with a manifest and optional marker.
    fn package(tmp: &tempfile::TempDir, marker: Option<&str>) -> std::path::PathBuf {
        let dir = tmp.path().join("pkg");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("extension.json"),
            r#"{"kind":"command","name":"greet","version":"1.2.3","protocol":"2025-06-18","flags":[],"commit":"per-batch","executable":"greet.sh"}"#,
        )
        .unwrap();
        if let Some(marker) = marker {
            std::fs::write(dir.join(MARKER_FILE), marker).unwrap();
        }
        dir
    }

    #[test]
    fn the_marker_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let provenance = Provenance {
            tier: Tier::ThirdParty,
            source: "https://example.test/greet.tar.gz".to_string(),
            pin: None,
        };
        provenance.write(&package(&tmp, None)).unwrap();
        assert_eq!(
            Provenance::read(&tmp.path().join("pkg")).unwrap(),
            Some(provenance)
        );
    }

    #[test]
    fn the_written_marker_is_the_flat_two_field_object() {
        // The on-disk format is pinned: kebab-case tier, JSON-escaped
        // source, one trailing newline.
        let tmp = tempfile::tempdir().unwrap();
        Provenance {
            tier: Tier::FirstParty,
            source: "relate-extensions-greet-1.0.0".to_string(),
            pin: None,
        }
        .write(&package(&tmp, None))
        .unwrap();
        let text = std::fs::read_to_string(tmp.path().join("pkg").join(MARKER_FILE)).unwrap();
        assert_eq!(
            text,
            "{\"tier\":\"first-party\",\"source\":\"relate-extensions-greet-1.0.0\"}\n"
        );
    }

    #[test]
    fn a_missing_marker_is_the_copied_in_state() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = package(&tmp, None);
        assert_eq!(Provenance::read(&dir).unwrap(), None);
    }

    #[test]
    fn a_malformed_marker_is_a_refusal_naming_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = package(&tmp, Some("{ nope"));
        let err = Provenance::read(&dir).unwrap_err().to_string();
        assert!(err.starts_with("invalid provenance marker "), "{err}");
        assert!(err.contains(MARKER_FILE), "{err}");
    }

    #[test]
    fn an_unknown_tier_fails_the_decode() {
        // The closed tier vocabulary: an unknown value is corruption, not a
        // future tier (the code-shape rules: no silent fallback).
        let tmp = tempfile::tempdir().unwrap();
        let dir = package(&tmp, Some("{\"tier\":\"enterprise\",\"source\":\"x\"}"));
        let err = Provenance::read(&dir).unwrap_err().to_string();
        assert!(err.contains("unknown variant"), "{err}");
        assert!(err.contains("enterprise"), "{err}");
    }

    #[test]
    fn an_unknown_marker_field_fails_the_decode() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = package(
            &tmp,
            Some("{\"tier\":\"local\",\"source\":\"x\",\"extra\":1}"),
        );
        let err = Provenance::read(&dir).unwrap_err().to_string();
        assert!(
            err.contains("unknown field") && err.contains("extra"),
            "{err}"
        );
    }

    /// A pre-pin marker (the two-field format every earlier tier wrote)
    /// decodes with no pin — the compatibility the `#[serde(default)]`
    /// states.
    #[test]
    fn an_older_two_field_marker_decodes_with_no_pin() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = package(
            &tmp,
            Some("{\"tier\":\"third-party\",\"source\":\"https://x.test/p.tar.gz\"}"),
        );
        assert_eq!(
            Provenance::read(&dir).unwrap(),
            Some(Provenance {
                tier: Tier::ThirdParty,
                source: "https://x.test/p.tar.gz".to_string(),
                pin: None,
            })
        );
    }

    /// The github-pin marker carries the resolved pin (repo, tag, commit,
    /// assetSha256) in the flat on-disk format, and roundtrips.
    #[test]
    fn the_github_pin_marker_records_the_resolved_pin() {
        let tmp = tempfile::tempdir().unwrap();
        let pin = Pin {
            repo: "owner/repo".to_string(),
            tag: "v1.0.0".to_string(),
            commit: "0123456789abcdef0123456789abcdef01234567".to_string(),
            asset_sha256: "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210"
                .to_string(),
        };
        let provenance = Provenance {
            tier: Tier::GithubPin,
            source: "owner/repo@v1.0.0".to_string(),
            pin: Some(pin.clone()),
        };
        provenance.write(&package(&tmp, None)).unwrap();
        let text = std::fs::read_to_string(tmp.path().join("pkg").join(MARKER_FILE)).unwrap();
        assert_eq!(
            text,
            format!(
                "{{\"tier\":\"github-pin\",\"source\":\"owner/repo@v1.0.0\",\
                 \"pin\":{{\"repo\":\"owner/repo\",\"tag\":\"v1.0.0\",\
                 \"commit\":\"{}\",\"assetSha256\":\"{}\"}}}}\n",
                pin.commit, pin.asset_sha256
            )
        );
        assert_eq!(
            Provenance::read(&tmp.path().join("pkg")).unwrap(),
            Some(provenance)
        );
    }
}
