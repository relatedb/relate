//! The one GitHub web-URL vocabulary (REL8-2381's hoist): the host
//! recognition and the owner/repo/verb/tail split every fetcher that
//! accepts a pasted `github.com` URL shares — the **one mechanism**, with
//! each caller's policy stated on top of the typed shape. `remote_db`
//! (`--load`) refuses folder-shaped URLs (a store is one `.rl` file);
//! the extension install accepts a `tree` URL as the package directory
//! and appends the manifest filename. Before this module the two rules
//! lived in two homes with two rule sets, so the same pasted URL behaved
//! differently per flag.
//!
//! Pure string work, no I/O; the shared authority→host extraction and
//! case-insensitive host comparison come from [`crate::plaintext`] — the
//! same vocabulary the `http://` loopback gate states its policy in.

use crate::plaintext::authority_host;

/// What a GitHub page URL's verb names, typed: the one place the content
/// verbs (`blob`, `raw`, `tree`) are spelled, so neither caller re-matches
/// the literal set. Any other page verb rides verbatim
/// ([`WebVerb::Other`]) for the refusal text that names it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum WebVerb {
    /// `blob`: the shareable page URL for one file.
    Blob,
    /// `raw`: GitHub's own redirecting verb for one file's bytes.
    Raw,
    /// `tree`: the folder page.
    Tree,
    /// Any other page verb (`releases`, `commits`, …), kept as written.
    Other(String),
}

impl WebVerb {
    /// The verb a path segment spells.
    fn parse(segment: &str) -> Self {
        match segment {
            "blob" => Self::Blob,
            "raw" => Self::Raw,
            "tree" => Self::Tree,
            other => Self::Other(other.to_string()),
        }
    }

    /// The verb as written — the refusals that name it read it back.
    pub(crate) fn as_str(&self) -> &str {
        match self {
            Self::Blob => "blob",
            Self::Raw => "raw",
            Self::Tree => "tree",
            Self::Other(verb) => verb,
        }
    }
}

/// What a URL is, GitHub-wise — the one recognition both fetchers share.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum GithubWebUrl {
    /// A `github.com` (or `www.github.com`) URL with at least
    /// `owner/repo/verb` path segments: the page vocabulary's shape,
    /// split into its parts. `tail` is everything after the verb, with
    /// empty segments dropped — `refs/heads/main` refs and query strings
    /// ride inside it untouched.
    Web {
        /// The repository owner.
        owner: String,
        /// The repository name.
        repo: String,
        /// The path's third segment, typed ([`WebVerb`]) — the content
        /// verbs both policies turn on, or any other page verb kept
        /// verbatim for the refusals that name it.
        verb: WebVerb,
        /// Everything after the verb (ref and path), empty when the URL
        /// stopped at the verb.
        tail: String,
    },
    /// A `github.com` URL too short to carry `owner/repo/verb` — a bare
    /// repository or folder-shaped page. `owner`/`repo` are present when
    /// the URL carried them, so a refusal can name the repository.
    Repo {
        /// The owner segment, when the URL had one.
        owner: Option<String>,
        /// The repository segment, when the URL had one.
        repo: Option<String>,
    },
    /// Any other host — a raw URL, a loopback server, anyone else's file
    /// host: the caller passes the URL through unchanged.
    Other,
}

/// Recognize a GitHub web URL: split off the scheme, compare the host
/// ASCII-case-insensitively through the shared authority extraction (a
/// `:port` or `userinfo@` spelling of the host is the same host), and
/// split the path into owner/repo/verb/tail. Everything else is
/// [`GithubWebUrl::Other`].
pub(crate) fn parse(url: &str) -> GithubWebUrl {
    // The crate's one scheme split (ASCII-case-insensitive schemes).
    let Some((_, rest)) = crate::plaintext::split_scheme(url) else {
        return GithubWebUrl::Other;
    };
    // Host comparison is ASCII-case-insensitive, like the loopback check:
    // a mixed-case `GitHub.com` URL is the same host to a browser and
    // must not skip the rewrite.
    let host = authority_host(rest);
    if !host.eq_ignore_ascii_case("github.com") && !host.eq_ignore_ascii_case("www.github.com") {
        return GithubWebUrl::Other;
    }
    // The path is everything after the authority; empty segments drop,
    // so `owner//repo` never masquerades as a content page.
    let path = rest.split_once('/').map_or("", |(_, path)| path);
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() < 3 {
        return GithubWebUrl::Repo {
            owner: segments.first().map(|s| s.to_string()),
            repo: segments.get(1).map(|s| s.to_string()),
        };
    }
    GithubWebUrl::Web {
        owner: segments[0].to_string(),
        repo: segments[1].to_string(),
        verb: WebVerb::parse(segments[2]),
        tail: segments[3..].join("/"),
    }
}

/// The raw-content URL for a recognized web URL's parts:
/// `raw.githubusercontent.com/<owner>/<repo>/<tail>` — one request
/// straight at the bytes instead of a page or a redirect hop.
pub(crate) fn raw_content_url(owner: &str, repo: &str, tail: &str) -> String {
    format!("https://raw.githubusercontent.com/{owner}/{repo}/{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The recognition: blob, raw and tree pages split into parts, with
    /// the case, host-spelling and ref shapes the `--load` tests pin.
    #[test]
    fn a_github_web_url_splits_into_owner_repo_verb_tail() {
        for (url, expected) in [
            (
                "https://github.com/relatedb/relate/blob/main/docs/examples/extension.bash.json",
                GithubWebUrl::Web {
                    owner: "relatedb".into(),
                    repo: "relate".into(),
                    verb: WebVerb::Blob,
                    tail: "main/docs/examples/extension.bash.json".into(),
                },
            ),
            (
                "https://www.github.com/o/r/tree/v1.2.3/pkg",
                GithubWebUrl::Web {
                    owner: "o".into(),
                    repo: "r".into(),
                    verb: WebVerb::Tree,
                    tail: "v1.2.3/pkg".into(),
                },
            ),
            (
                "https://GitHub.com:443/o/r/raw/refs/heads/main/d.rl",
                GithubWebUrl::Web {
                    owner: "o".into(),
                    repo: "r".into(),
                    verb: WebVerb::Raw,
                    tail: "refs/heads/main/d.rl".into(),
                },
            ),
        ] {
            assert_eq!(parse(url), expected, "{url}");
        }
        // A query string rides inside the tail untouched.
        assert_eq!(
            parse("https://github.com/o/r/blob/main/d.rl?raw=1"),
            GithubWebUrl::Web {
                owner: "o".into(),
                repo: "r".into(),
                verb: WebVerb::Blob,
                tail: "main/d.rl?raw=1".into()
            }
        );
    }

    /// A github.com URL too short for owner/repo/verb is the repository
    /// shape, with the segments it did carry; a URL stopped at the verb
    /// is a Web with an empty tail (the caller's refusal names the form).
    #[test]
    fn short_github_urls_are_the_repository_shape() {
        assert_eq!(
            parse("https://github.com/relatedb/relate"),
            GithubWebUrl::Repo {
                owner: Some("relatedb".into()),
                repo: Some("relate".into())
            }
        );
        assert_eq!(
            parse("https://github.com"),
            GithubWebUrl::Repo {
                owner: None,
                repo: None
            }
        );
        assert_eq!(
            parse("https://github.com/o/r/blob"),
            GithubWebUrl::Web {
                owner: "o".into(),
                repo: "r".into(),
                verb: WebVerb::Blob,
                tail: String::new()
            }
        );
    }

    /// Every other host — raw URLs, other file hosts, loopback servers,
    /// and URLs with no scheme at all — is `Other`, the caller's
    /// passthrough.
    #[test]
    fn every_other_host_is_other() {
        for url in [
            "https://raw.githubusercontent.com/o/r/main/d.rl",
            "https://example.test/pkg/extension.json",
            "http://127.0.0.1:9/pkg",
            "github.com/o/r/blob/main/f",
            "",
        ] {
            assert_eq!(parse(url), GithubWebUrl::Other, "{url}");
        }
    }

    /// The typed verb: the three content words are their own variants, any
    /// other page verb rides verbatim — the one place the literal set
    /// lives, so no caller re-matches it, and each policy matches the
    /// variant directly.
    #[test]
    fn the_verb_is_typed() {
        for (url, verb) in [
            ("https://github.com/o/r/blob/main/f", WebVerb::Blob),
            ("https://github.com/o/r/raw/main/f", WebVerb::Raw),
            ("https://github.com/o/r/tree/main/d", WebVerb::Tree),
            (
                "https://github.com/o/r/releases/tag/v1",
                WebVerb::Other("releases".into()),
            ),
        ] {
            let GithubWebUrl::Web { verb: parsed, .. } = parse(url) else {
                panic!("{url} is a web URL")
            };
            assert_eq!(parsed, verb, "{url}");
        }
        // `as_str` reads the verb back for the refusals that name it.
        assert_eq!(WebVerb::Tree.as_str(), "tree");
        assert_eq!(WebVerb::Other("commits".into()).as_str(), "commits");
    }

    /// The raw-content URL is the one spelling both policies rewrite to.
    #[test]
    fn the_raw_content_url_joins_the_parts() {
        assert_eq!(
            raw_content_url("o", "r", "main/docs/examples/extension.bash.json"),
            "https://raw.githubusercontent.com/o/r/main/docs/examples/extension.bash.json"
        );
    }
}
