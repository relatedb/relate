//! The `--from URL` layout rules (ADR-0431 §5's remote form, REL8-2381):
//! the pure string decisions that turn the URL the invoker pasted into
//! the URLs the install fetches — which URL is the manifest, which URL
//! the executable rides from, and the one GitHub rewrite. No I/O here;
//! `sources.rs`'s adapter fetches through these decisions and
//! `tests/from_url.rs` drives them end to end over a loopback route.

use std::path::Path;

use crate::extensions::registry::MANIFEST_FILE;
use crate::github_url::{self, GithubWebUrl, WebVerb};
use crate::plaintext::path_suffix;

use super::error::InstallError;

/// The install policy over the crate's one GitHub web-URL vocabulary
/// (REL8-2381's hoist — the mechanism is shared with `--load`, the
/// policy is this package rule's alone): a **file** page (the `blob` link
/// the invoker browsed — it serves HTML, not the file — or GitHub's own
/// `raw` verb) and a **folder** page (the `tree` form, the manifest
/// filename appended to it) both name content, so both rewrite to their
/// raw-content form. A bare repository URL and a content page whose path
/// stops at the verb are refused with the form that works — the same
/// refusals `--load` words over the same vocabulary. Any other shape
/// (another host, a releases page) passes through and the fetch itself
/// answers for it.
pub(super) fn github_raw_url(url: &str) -> Result<String, InstallError> {
    match github_url::parse(url) {
        GithubWebUrl::Web {
            owner,
            repo,
            verb,
            tail,
        } => match verb {
            // The three content verbs, each naming a file or a folder: a
            // page whose path stops at the verb names neither and refuses
            // with the form that works.
            WebVerb::Blob | WebVerb::Raw | WebVerb::Tree => {
                if tail.is_empty() {
                    return Err(InstallError::Usage(format!(
                        "the GitHub {} URL '{url}' names no file or folder; point --from at the \
                         manifest or the folder that holds it, e.g. \
                         https://github.com/{owner}/{repo}/blob/REF/PATH/{MANIFEST_FILE}",
                        verb.as_str()
                    )));
                }
                Ok(github_url::raw_content_url(&owner, &repo, &tail))
            }
            // Any other page (a releases listing, say): the fetch answers.
            WebVerb::Other(_) => Ok(url.to_string()),
        },
        GithubWebUrl::Repo { owner, repo } => Err(InstallError::Usage(format!(
            "the GitHub repository URL '{url}' is a page, not a package; point --from at the \
             manifest or the folder that holds it, e.g. \
             https://github.com/{}/{}/blob/REF/PATH/{MANIFEST_FILE}",
            owner.as_deref().unwrap_or("OWNER"),
            repo.as_deref().unwrap_or("REPO")
        ))),
        // Any other host — a raw URL, a loopback server, another file
        // host: the URL passes through and the fetch answers for it.
        GithubWebUrl::Other => Ok(url.to_string()),
    }
}

/// The manifest URL a `--from URL` install fetches (ADR-0431 §5): a URL
/// whose last path segment ends in `.json` names the manifest file
/// itself; any other URL names a directory and the channel layout's
/// manifest filename (`extension.json`) is appended to it — trailing
/// slash or not, the one join, with any query or fragment kept after it.
pub(super) fn manifest_url(base: &str) -> String {
    let (path, suffix) = path_suffix(base);
    if crate::plaintext::last_segment(base).ends_with(".json") {
        base.to_string()
    } else {
        format!("{}/{}{suffix}", path.trim_end_matches('/'), MANIFEST_FILE)
    }
}

/// The executable's URL: the folder the manifest was fetched from — the
/// manifest URL's path part, so the manifest's own query never rides onto
/// its sibling — joined with the manifest's relative `executable` path,
/// its platform components spelled as the URL's `/`-separated segments.
/// `check_manifest` has already proven that path non-empty and inside the
/// package folder, so the join is the same-folder-or-relative-subfolder
/// rule by construction, and the folder split's invariant (a manifest URL
/// always carries a path — [`manifest_url`] guarantees one) is stated
/// rather than defaulted.
pub(super) fn executable_url(manifest_url: &str, executable: &Path) -> String {
    let (path, _) = path_suffix(manifest_url);
    let (folder, _) = path
        .rsplit_once('/')
        .expect("a manifest URL carries a path");
    // The segments are UTF-8 by construction — the manifest's executable
    // is a JSON string's path — so the conversion states its invariant
    // instead of coercing (a lossy cast would silently name a different
    // URL).
    let relative: Vec<&str> = executable
        .iter()
        .map(|segment| {
            segment
                .to_str()
                .expect("a manifest executable path is UTF-8 (it came from a JSON string)")
        })
        .collect();
    format!("{folder}/{}", relative.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file page (blob, or GitHub's own raw verb) and a folder page
    /// (tree — the directory form) rewrite to the raw-content URL; the
    /// vocabulary's host spellings (www., mixed case, :port) and a
    /// slashed ref rewrite alike, because the mechanism is the shared
    /// one.
    #[test]
    fn content_pages_rewrite_to_their_raw_form() {
        assert_eq!(
            github_raw_url(
                "https://github.com/relatedb/relate/blob/main/docs/api/extensions/examples/extension.bash.json"
            )
            .unwrap(),
            "https://raw.githubusercontent.com/relatedb/relate/main/docs/api/extensions/examples/extension.bash.json"
        );
        assert_eq!(
            github_raw_url(
                "https://github.com/relatedb/relate/tree/main/docs/api/extensions/examples"
            )
            .unwrap(),
            "https://raw.githubusercontent.com/relatedb/relate/main/docs/api/extensions/examples"
        );
        assert_eq!(
            github_raw_url("https://www.GitHub.com:443/o/r/raw/refs/heads/main/pkg/run.sh")
                .unwrap(),
            "https://raw.githubusercontent.com/o/r/refs/heads/main/pkg/run.sh"
        );
    }

    /// The bare repository URL and a content page whose path stops at the
    /// verb are refused with the form that works (the usage class, before
    /// any fetch) — the commonest paste must not die as a download 404.
    /// Every other shape passes through unchanged and the fetch answers.
    #[test]
    fn the_repository_and_truncated_pages_refuse_and_everything_else_passes() {
        for url in [
            "https://github.com/relatedb/relate",
            "https://github.com/o/r/blob",
            "https://github.com/o/r/tree",
        ] {
            let err = github_raw_url(url).expect_err("a page that names no content refuses");
            assert_eq!(err.exit_code(), 2, "{url}: {}", err.stderr_message());
            assert!(
                err.stderr_message().contains("point --from at"),
                "{url}: {}",
                err.stderr_message()
            );
        }
        for url in [
            "https://raw.githubusercontent.com/relatedb/relate/main/pkg/extension.json",
            "https://example.test/pkg/extension.json",
            "http://127.0.0.1:9/pkg",
            "https://github.com/relatedb/relate/releases/tag/v1",
            "https://github.com/relatedb/relate/commits/main",
        ] {
            assert_eq!(github_raw_url(url).unwrap(), url, "{url}");
        }
    }

    /// The manifest-or-directory rule: a last path segment ending in
    /// `.json` names the manifest itself; any other URL gets
    /// `extension.json` appended — trailing slash or not, one join, with
    /// the query kept after the appended name and the last-segment
    /// decision made on the path alone (the `?ref=main` case would
    /// otherwise fetch `pkg?ref=main/extension.json`).
    #[test]
    fn a_json_last_segment_is_the_manifest_and_anything_else_appends_it() {
        assert_eq!(
            manifest_url("https://example.test/pkg/extension.bash.json"),
            "https://example.test/pkg/extension.bash.json"
        );
        assert_eq!(
            manifest_url("https://example.test/pkg/extension.json"),
            "https://example.test/pkg/extension.json"
        );
        assert_eq!(
            manifest_url("https://example.test/pkg"),
            "https://example.test/pkg/extension.json"
        );
        assert_eq!(
            manifest_url("https://example.test/pkg/"),
            "https://example.test/pkg/extension.json"
        );
        assert_eq!(
            manifest_url("https://example.test"),
            "https://example.test/extension.json"
        );
        assert_eq!(
            manifest_url("https://example.test/x.json/"),
            "https://example.test/x.json/extension.json"
        );
        // A query or fragment rides after the appended name, and the
        // decision is made on the path alone.
        assert_eq!(
            manifest_url("https://example.test/pkg?ref=main"),
            "https://example.test/pkg/extension.json?ref=main"
        );
        assert_eq!(
            manifest_url("https://example.test/pkg/extension.json?raw=1"),
            "https://example.test/pkg/extension.json?raw=1"
        );
    }

    /// The executable joins onto the manifest's folder — the path part
    /// only, so the manifest's own query never rides onto its sibling.
    #[test]
    fn the_executable_url_joins_onto_the_manifests_folder() {
        assert_eq!(
            executable_url(
                "https://example.test/pkg/extension.json",
                Path::new("run.sh")
            ),
            "https://example.test/pkg/run.sh"
        );
        assert_eq!(
            executable_url(
                "https://example.test/pkg/extension.json",
                Path::new("bin/run.sh")
            ),
            "https://example.test/pkg/bin/run.sh"
        );
        assert_eq!(
            executable_url(
                "https://example.test/pkg/extension.json?raw=1",
                Path::new("run.sh")
            ),
            "https://example.test/pkg/run.sh"
        );
    }
}
