//! The remote `--load` source (LABS-872, ADR-0496; the `--load` grammar is
//! LABS-903, ADR-0500): an `http(s)` URL naming a remote `.rl` single-file
//! bundle. The CLI fetches the bundle bytes through the shared capped GET
//! ([`crate::url_fetch`]) and stages them at a private temp `.rl` path —
//! always **scratch**: the load that asked for it either turns it into the
//! in-memory store (the copy is released after the load) or clones it into
//! the caller's `--database` target (the copy is removed after the clone).
//! Nothing is ever kept. A CLI-process fetch, not an engine capability
//! (the ADR-0267 posture), so the engine's URI surface is untouched and a
//! `--load` value that is not an `http(s)` URL is a plain local path
//! exactly as before.
//!
//! Two halves, split along their gate. This module is the pure, ungated
//! one: [`plan`] classifies a `--load` value, rewrites a GitHub blob link,
//! and folds in the policy refusals — safe mode introduces no remote store
//! (ADR-0477) — so every build refuses a remote URL correctly before any
//! network I/O. The [`staging`] half (compiled only with the
//! `remote-database` feature) owns the fetch and the staged file's
//! lifecycle; without it the fetch fails closed (ADR-0104) and
//! [`cleanup_staged`] is a no-op.

use std::path::PathBuf;

use crate::plaintext::{authority_host, is_loopback_host};

/// The remote `--load` source a URL names: the URL to fetch, after the
/// GitHub blob→raw rewrite. Carries no I/O — [`plan`] is pure, so every
/// policy check runs before the network is touched.
#[derive(Debug)]
pub(crate) struct RemoteDb {
    fetch_url: String,
    /// Whether every hop of the fetch must be https — the LABS-470
    /// redirect-downgrade pin the release channel's client carries. It is
    /// set for every `https://` URL, so a `302` cannot undo the scheme
    /// policy [`require_secure_scheme`] enforced. The one release is the
    /// loopback plain-`http` carve-out (a local dev server, the e2e
    /// fixtures), where the pin would refuse the first hop; the URL's own
    /// host is the trust there. Only the gated fetch reads it, so a build
    /// without the feature carries the field unread.
    #[cfg_attr(not(feature = "remote-database"), allow(dead_code))]
    https_only: bool,
}

/// The `http(s)` scheme split of a remote value — the one scheme
/// vocabulary this module classifies by. `Some((scheme, rest))` only for
/// an `http`/`https` URL (the scheme compares case-insensitively, a URL's
/// scheme is case-insensitive per RFC 3986, so `HTTPS://…` is the same
/// remote value as `https://…`); `None` for every other `scheme://` form
/// and every non-URL, which are local paths the engine answers for itself
/// (ADR-0044). [`is_remote_uri`] and [`plan`] both consume this one split,
/// so the predicate and the classifier can never disagree about a value.
fn remote_uri_parts(uri: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = uri.split_once("://")?;
    if scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("http") {
        Some((scheme, rest))
    } else {
        None
    }
}

/// Whether a value is an `http(s)` URL — the one scheme vocabulary
/// [`remote_uri_parts`] splits, shared with the `--database` refusal that
/// keeps the target axis local (a URL is a `--load` source, never a store
/// to open).
pub(crate) fn is_remote_uri(uri: &str) -> bool {
    remote_uri_parts(uri).is_some()
}

/// The whole pure answer for one `--load` value: `Ok(None)` for a local
/// value (every non-URL form), `Ok(Some(..))` for a remote source that
/// passed every policy check, and `Err(message)` for a URL this run
/// refuses — the value's shape or safe mode. The caller words the `Err` as
/// a usage error (exit 2); the fetch that follows is a runtime error (exit
/// 1), which is why the halves report separately. (The live-freshness
/// refusal is a parse-time flag conflict — [`crate::load::parse_conflict`]
/// — a loaded source is never followed, whatever the target.)
///
/// A URL's last path segment must end in `.rl` (or the legacy v0.16 `.cl`,
/// ADR-0517 §5): a single-file bundle is the one remote store shape (a native
/// directory is several files and its own WAL protocol), so the refusal
/// happens here, before any fetch, naming the `.rl`-shaped URL that works in
/// its place.
pub(crate) fn plan(uri: &str, safe_mode: bool) -> Result<Option<RemoteDb>, String> {
    let Some((scheme, rest)) = remote_uri_parts(uri) else {
        return Ok(None);
    };
    let https = scheme.eq_ignore_ascii_case("https");
    let fetch_url =
        validate_url(scheme, https, rest).map_err(|e| format!("invalid --load: {e}"))?;
    if safe_mode {
        return Err(
            "--safe refuses a remote --load source — a downloaded store is untrusted input, \
             so safe mode introduces none"
                .into(),
        );
    }
    Ok(Some(RemoteDb {
        fetch_url,
        https_only: https,
    }))
}

/// Fetch the bundle and stage it at a private temp `.rl` path — scratch,
/// registered for [`cleanup_staged`] before its first byte, because every
/// `--load` consumer releases it (the memory turn-into after the load, the
/// durable-target clone after the clone). The `remote-database` feature's
/// one I/O step, delegated to the gated [`staging`] half; the
/// redirect-downgrade pin travels on [`RemoteDb`], set once in [`plan`].
#[cfg(feature = "remote-database")]
pub(crate) fn fetch_and_stage(remote: &RemoteDb) -> Result<PathBuf, String> {
    staging::fetch_and_stage(&remote.fetch_url, remote.https_only)
}

/// The fail-closed arm (ADR-0104): the URL parsed and every policy refusal
/// passed, but this build did not compile the fetch. Naming the URL keeps
/// the message actionable when it came from a script.
#[cfg(not(feature = "remote-database"))]
pub(crate) fn fetch_and_stage(remote: &RemoteDb) -> Result<PathBuf, String> {
    Err(format!(
        "{}: the remote bundle fetch is not compiled into this build (enable the \
         \"remote-database\" feature)",
        remote.fetch_url
    ))
}

/// Remove the staged bundle, if one was registered. Called by
/// `exit_flushed`, so every exit after a remote fetch cleans up (including
/// error exits and a failed open of the staged store), and by the two
/// `--load` consumers the moment the staged copy is unreferenced (the
/// memory turn-into after the open, the durable-target clone after the
/// clone); `process::exit` would skip any Drop-based cleanup, so the exit
/// path reads the registration instead.
pub(crate) fn cleanup_staged() {
    #[cfg(feature = "remote-database")]
    staging::remove_staged();
}

/// The URL-shape checks, one step: the plaintext scheme policy, the GitHub
/// file-URL rewrite, and the bundle requirement, in that order — each
/// refusal words itself; [`plan`] prefixes them all once.
fn validate_url(scheme: &str, https: bool, rest: &str) -> Result<String, String> {
    if rest.is_empty() {
        return Err("empty URL".into());
    }
    require_secure_scheme(https, rest)?;
    let fetch_url = rewrite_github_file_url(scheme, rest)?;
    require_bundle_path(&fetch_url)?;
    Ok(fetch_url)
}

/// The `http://` policy: plaintext is accepted only for a **loopback** host
/// — the e2e fixtures and a local dev server — and refused everywhere else
/// with the `https://` form named. There is no opt-in flag: a database
/// fetched over plaintext arrives unverified on a transport anyone on the
/// path can rewrite, and the fetch has no signature story either (ADR-0496
/// §9). The loopback recognition is the CLI's shared plaintext vocabulary
/// ([`crate::plaintext`]); the policy on top is `pull`'s shape (ADR-0122),
/// without its `--allow-plaintext` opt-in.
fn require_secure_scheme(https: bool, rest: &str) -> Result<(), String> {
    if https || is_loopback_host(authority_host(rest)) {
        return Ok(());
    }
    Err(format!(
        "plain http:// is only allowed for a loopback host; use https://{rest}"
    ))
}

/// The GitHub file-URL rewrite, one pure step over a URL [`plan`] already
/// split into `scheme` and the part after `://`: a **blob** link
/// (`github.com/<owner>/<repo>/blob/<ref>/<path>`, the shareable page URL)
/// or GitHub's own **raw** verb
/// (`github.com/<owner>/<repo>/raw/<ref>/<path>`, which 302s to the bytes)
/// becomes `raw.githubusercontent.com/<owner>/<repo>/<ref>/<path>` — one
/// request straight at the bytes instead of a redirect hop. The same path
/// on `www.github.com` rewrites alike. Every other host — a raw URL, a
/// loopback server, anyone else's file host — passes through unchanged, and
/// a `github.com` URL that is not a link to one file (a repository, tree,
/// or other-verb URL) is refused with the form that works.
fn rewrite_github_file_url(scheme: &str, rest: &str) -> Result<String, String> {
    // A plain non-GitHub URL keeps its scheme as given (case and all). The
    // host check rides the shared authority extraction, so a `:port` or
    // userinfo@ spelling of github.com rewrites like the bare host does.
    let path = rest.split_once('/').map_or("", |(_, path)| path);
    let host = authority_host(rest);
    // Host comparison is ASCII-case-insensitive, like the loopback check
    // below: a mixed-case `GitHub.com` URL is the same host to a browser and
    // must not skip the rewrite.
    if !host.eq_ignore_ascii_case("github.com") && !host.eq_ignore_ascii_case("www.github.com") {
        return Ok(format!("{scheme}://{rest}"));
    }
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    // owner/repo/<verb>/[<ref>/]<path-to-file> — the blob form needs all
    // five: a bare repository URL (two segments) and a blob link whose
    // ref swallows the file path (three or four) are folder-shaped.
    if segments.len() < 3 {
        return Err(format!(
            "a GitHub repository URL is not a single-file store; point at a .rl bundle file: {}",
            blob_hint(scheme, segments.first().copied(), segments.get(1).copied())
        ));
    }
    let (owner, repo, verb) = (segments[0], segments[1], segments[2]);
    let tail = &segments[3..];
    match verb {
        // Both are links to one file's bytes: `blob` is the page URL people
        // copy, `raw` is GitHub's own redirecting verb.
        "blob" | "raw" => {}
        "tree" => {
            return Err(format!(
                "a GitHub tree URL is a folder, not a single-file store; point at a .rl bundle \
                 file inside it: {}",
                blob_hint(scheme, Some(owner), Some(repo))
            ));
        }
        other => {
            return Err(format!(
                "a GitHub {other} URL is not a single-file store; the blob or raw form fetches \
                 the file's bytes: {}",
                blob_hint(scheme, Some(owner), Some(repo))
            ));
        }
    }
    if tail.is_empty() {
        return Err(format!(
            "a GitHub {verb} URL needs a file path after the ref: {}",
            blob_hint(scheme, Some(owner), Some(repo))
        ));
    }
    Ok(format!(
        "https://raw.githubusercontent.com/{owner}/{repo}/{}",
        tail.join("/")
    ))
}

/// The canonical blob URL every GitHub refusal points at, with the owner and
/// repo filled in when the URL carried them (`OWNER`/`REPO` otherwise) — one
/// spelling, so the form the errors teach cannot drift between the arms.
fn blob_hint(scheme: &str, owner: Option<&str>, repo: Option<&str>) -> String {
    format!(
        "{scheme}://github.com/{}/{}/blob/REF/PATH.{}",
        owner.unwrap_or("OWNER"),
        repo.unwrap_or("REPO"),
        relate_storage::BUNDLE_EXTENSION,
    )
}

/// The bundle requirement on the fetched URL's last path segment (query and
/// fragment stripped): the one remote store shape. The engine's own bundle
/// predicate decides, so `.rl` and the legacy v0.16 `.cl` are admitted alike
/// (ADR-0517 §5).
fn require_bundle_path(url: &str) -> Result<(), String> {
    let no_query = url.split(['?', '#']).next().unwrap_or(url);
    let last = no_query.rsplit('/').next().unwrap_or(no_query);
    if relate_storage::is_bundle_path(std::path::Path::new(last)) {
        Ok(())
    } else {
        Err(format!(
            "a remote --load source must be a .rl single-file bundle (a native directory store \
             cannot be fetched over HTTP); got '{last}'"
        ))
    }
}

#[cfg(feature = "remote-database")]
mod staging;

#[cfg(test)]
mod tests {
    use super::*;

    /// The planned fetch URL for a value every policy check accepts.
    fn planned(uri: &str) -> String {
        plan(uri, false)
            .unwrap()
            .expect("a remote source")
            .fetch_url
    }

    #[test]
    fn non_urls_are_not_remote() {
        for uri in [
            ":memory:",
            "./db",
            "file:///tmp/x.rl",
            "s3://bucket/x.rl",
            "",
        ] {
            assert!(plan(uri, false).unwrap().is_none(), "{uri}");
            // The safe-mode knob does not make a local value remote.
            assert!(plan(uri, true).unwrap().is_none(), "{uri}");
        }
    }

    #[test]
    fn is_remote_uri_is_the_scheme_vocabulary() {
        // The same http(s) classification `plan` uses, shared with the
        // --database refusal: a URL is a --load source, never a target.
        for (uri, remote) in [
            ("https://example.com/s.rl", true),
            ("HTTPS://Example.com/s.rl", true),
            ("http://127.0.0.1:9/s.rl", true),
            ("Http://localhost/s.rl", true),
            (":memory:", false),
            ("store.rl", false),
            ("file:///tmp/x.rl", false),
            ("s3://bucket/x.rl", false),
            ("https", false),
            ("", false),
        ] {
            assert_eq!(is_remote_uri(uri), remote, "{uri}");
        }
    }

    #[test]
    fn a_github_blob_url_is_rewritten_to_raw() {
        assert_eq!(
            planned("https://github.com/relatedb/relate/blob/main/examples/so.rl"),
            "https://raw.githubusercontent.com/relatedb/relate/main/examples/so.rl"
        );
        // The scheme compares case-insensitively, like every URL scheme.
        assert_eq!(
            planned("HTTPS://github.com/o/r/blob/main/d.rl"),
            "https://raw.githubusercontent.com/o/r/main/d.rl"
        );
        // A :port (or userinfo@) spelling of the host rewrites like the
        // bare host does.
        assert_eq!(
            planned("https://github.com:443/o/r/blob/main/d.rl"),
            "https://raw.githubusercontent.com/o/r/main/d.rl"
        );
        // The www spelling and a ref with slashes rewrite alike.
        assert_eq!(
            planned("https://www.github.com/o/r/blob/v1.2.3/d.rl"),
            "https://raw.githubusercontent.com/o/r/v1.2.3/d.rl"
        );
        // GitHub's own `raw` verb rewrites to the same place — the bytes in
        // one request instead of a redirect hop — including the
        // `refs/heads/<branch>` ref spelling.
        assert_eq!(
            planned("https://github.com/o/r/raw/main/d.rl"),
            "https://raw.githubusercontent.com/o/r/main/d.rl"
        );
        assert_eq!(
            planned("https://github.com/o/r/raw/refs/heads/main/d.rl"),
            "https://raw.githubusercontent.com/o/r/refs/heads/main/d.rl"
        );
        // A query string rides along untouched.
        assert_eq!(
            planned("https://github.com/o/r/blob/main/d.rl?raw=1"),
            "https://raw.githubusercontent.com/o/r/main/d.rl?raw=1"
        );
    }

    #[test]
    fn github_folder_urls_are_refused_with_the_blob_form_that_works() {
        for (uri, needle) in [
            ("https://github.com/relatedb/relate", "repository URL"),
            (
                "https://github.com/o/r/tree/main/stores",
                "tree URL is a folder",
            ),
            (
                "https://github.com/o/r/commits/main",
                "is not a single-file store",
            ),
            ("https://github.com/o/r/blob", "needs a file path"),
        ] {
            let err = plan(uri, false).unwrap_err();
            assert!(err.starts_with("invalid --load: "), "{uri}: {err}");
            assert!(err.contains(needle), "{uri}: {err}");
            assert!(err.contains("blob/REF/PATH.rl"), "{uri}: {err}");
        }
    }

    #[test]
    fn non_github_and_raw_urls_pass_through_unchanged() {
        for uri in [
            "https://raw.githubusercontent.com/o/r/main/d.rl",
            "https://example.com/databases/store.rl",
            "http://127.0.0.1:8080/store.rl",
        ] {
            assert_eq!(planned(uri), uri);
        }
    }

    #[test]
    fn a_remote_source_must_name_a_bundle_file() {
        let err = plan("https://example.com/databases/store", false).unwrap_err();
        assert!(err.contains("must be a .rl single-file bundle"), "{err}");
        // A GitHub blob URL to a file that is not a .rl bundle fails the
        // same requirement, naming the segment it got.
        let err = plan("https://github.com/o/r/blob/main/store", false).unwrap_err();
        assert!(err.contains("got 'store'"), "{err}");
        // A query or fragment after the .rl segment does not disqualify.
        assert_eq!(
            planned("https://example.com/store.rl?x=1"),
            "https://example.com/store.rl?x=1"
        );
        // The empty-URL refusal.
        let err = plan("https://", false).unwrap_err();
        assert!(err.contains("empty URL"), "{err}");
    }

    #[test]
    fn plaintext_is_loopback_only() {
        // https is never refused, whatever the host.
        assert_eq!(
            planned("https://example.com/store.rl"),
            "https://example.com/store.rl"
        );
        // http is accepted for loopback (the e2e fixtures, a dev server).
        for uri in [
            "http://127.0.0.1:8080/store.rl",
            "http://localhost/store.rl",
            "http://[::1]:9/store.rl",
            "http://127.9.9.9/store.rl",
        ] {
            assert_eq!(planned(uri), uri);
        }
        // ... and refused for anything else, naming the https form.
        for host in ["example.com", "10.0.0.1", "github.com", "0127.0.0.1"] {
            let err = plan(&format!("http://{host}/store.rl"), false).unwrap_err();
            assert!(
                err.contains("only allowed for a loopback host"),
                "{host}: {err}"
            );
            assert!(
                err.contains(&format!("https://{host}/store.rl")),
                "{host}: {err}"
            );
        }
    }

    #[test]
    fn safe_mode_refuses_a_remote_source() {
        let err = plan("https://example.com/s.rl", true).unwrap_err();
        assert!(
            err.contains("--safe refuses a remote --load source"),
            "{err}"
        );
    }

    #[test]
    fn a_legacy_cl_remote_source_still_plans() {
        // v0.16-and-earlier bundles end `.cl`; the gate admits both
        // extensions (ADR-0517 §5) while the refusals teach `.rl`.
        assert_eq!(
            planned("https://example.com/databases/legacy.cl"),
            "https://example.com/databases/legacy.cl"
        );
        assert_eq!(
            planned("https://github.com/o/r/blob/main/d.cl"),
            "https://raw.githubusercontent.com/o/r/main/d.cl"
        );
    }
}
