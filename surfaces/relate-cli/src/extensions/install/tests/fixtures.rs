//! The GitHub tier's fixture data (ADR-0449): the loopback API response
//! documents, the package artifact builders and the commit consts. The
//! flow drivers (`install_from`, `install_from_remote`, `install_github`)
//! live with `World` in `tests/mod.rs`; this module is data only.

use super::*;
use crate::extensions::source::{GithubPin, GithubSpec};
use relate_test_support::loopback::spawn_scripted_responses_capturing;

/// The two commits the fixture releases resolve to.
pub(super) const COMMIT_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub(super) const COMMIT_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// The fixture release tag and the host platform's package asset name.
pub(super) const GITHUB_TAG: &str = "v1.2.3";

pub(super) fn github_asset() -> String {
    format!("greet-1.2.3-{}-{}.tar.gz", host().os, host().arch)
}

/// The greet package artifact (manifest + executable) the GitHub release
/// serves. `payload` varies the executable's bytes, so a test can move
/// the asset's digest without touching the manifest.
pub(super) fn github_artifact_with(payload: &[u8]) -> Vec<u8> {
    let manifest = manifest_text("greet", "1.2.3");
    crate::extensions::testtar::tarball(&[
        ("extension.json", 0o644, manifest.as_bytes()),
        ("run.sh", 0o755, payload),
    ])
}

pub(super) fn github_artifact() -> Vec<u8> {
    github_artifact_with(b"#!/bin/sh\n")
}

/// A commit lookup document.
pub(super) fn commit_doc(sha: &str) -> String {
    format!(r#"{{"sha":"{sha}","commit":{{"message":"release"}}}}"#)
}

/// A release document carrying the host platform's greet asset with
/// `digest` (the `sha256:` value GitHub records).
pub(super) fn release_doc(tag: &str, digest: &str) -> String {
    format!(
        r#"{{"tag_name":"{tag}","name":"{tag}","assets":[{{"id":7,"name":"{}","size":4096,"digest":"sha256:{digest}"}}]}}"#,
        github_asset()
    )
}

/// A `--github` spec pinning `owner/repo` at `tag`.
pub(super) fn spec(tag: &str) -> GithubSpec {
    GithubSpec {
        repo: "owner/repo".to_string(),
        pin: GithubPin::Tag(tag.to_string()),
    }
}

pub(super) fn base(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

/// The API fixture for one resolution: the commit lookup and the release
/// lookup, in the order `resolve_tag` fetches them.
pub(super) fn tag_api(
    commit: &str,
    digest: &str,
) -> (relate_test_support::loopback::CapturingFixture, u16) {
    spawn_scripted_responses_capturing(
        "200 OK",
        vec![commit_doc(commit), release_doc(GITHUB_TAG, digest)],
    )
}
