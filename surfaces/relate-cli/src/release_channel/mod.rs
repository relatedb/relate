//! The first-party release channel's shared machinery (ADR-0431 §1): the
//! pieces `upgrade` (ADR-0250, ADR-0358) and `extension install`
//! (ADR-0431) both run — the one HTTP client, the probe-then-fallback
//! download routes, the GitHub API asset route, and the one signing root
//! (ADR-0115). Extracted from `upgrade_cmd` when the extension installer
//! became the second consumer: a helper used by siblings lives in the
//! lowest shared module, not in the first caller's.
//!
//! Compiled under `upgrade` **or** `extension-install` — the `agent`
//! bundle carries the extension channel without the self-upgrade.
//!
//! Everything here renders failures as a bare [`ChannelError`] text; each
//! flow wraps it with its own `Error: <flow>: …` prefix, so the shared
//! machinery never picks a command's wording. The env-seam policy
//! (ADR-0250 §1, ADR-0358 §8) is shared for the same reason: one
//! classification of set/unset/empty/non-UTF-8 behind every URL seam and
//! the `GITHUB_TOKEN` read.

pub(crate) mod api_assets;
pub(crate) mod download_route;
pub(crate) mod http;
pub(crate) mod signing;

/// GitHub repository publishing the release assets (ADR-0067), the one source
/// of the slug the metadata, download and API URLs are all built from.
pub(crate) const REPO: &str = "relatedb/relate";

/// A release-channel failure as the shared machinery reports it: bare text,
/// no command prefix — the calling flow's error type owns the
/// `Error: <command>: …` rendering, and converts through
/// [`std::fmt::Display`]. One string suffices because every failure path
/// collapses to one message and one exit code inside the calling flow.
#[derive(Debug)]
pub(crate) struct ChannelError(String);

impl ChannelError {
    pub(crate) fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}

impl std::fmt::Display for ChannelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The one environment policy shared by the URL seams
/// (`RELATE_RELEASES_JSON_URL`, `RELATE_RELEASE_DOWNLOAD_URL`,
/// `RELATE_GITHUB_API_URL`, `RELATE_EXTENSIONS_JSON_URL`) and the
/// `GITHUB_TOKEN` read (ADR-0250 §1, ADR-0358 §8, ADR-0431): a set,
/// non-empty value is `Some`; unset and empty are `None`, the documented
/// default/absent case; a set but non-UTF-8 value is a typed error naming
/// the variable — never silently treated as unset. Pure over one
/// `std::env::var` result, so the policy is unit-testable without racing
/// the process environment.
pub(crate) fn classify_env(
    var: &str,
    value: Result<String, std::env::VarError>,
) -> Result<Option<String>, ChannelError> {
    match value {
        Ok(value) if !value.is_empty() => Ok(Some(value)),
        Ok(_) | Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(ChannelError::new(format!(
            "{var} is set but not valid Unicode"
        ))),
    }
}

/// [`classify_env`] with the environment read: the one helper the URL seams
/// and the `GITHUB_TOKEN` read call.
pub(crate) fn env_nonempty(var: &str) -> Result<Option<String>, ChannelError> {
    classify_env(var, std::env::var(var))
}

/// The documented base-URL dev/test seam resolver (ADR-0250 §1, ADR-0358
/// §8): the environment's value when set and non-empty, else `default`.
/// There is no fallback between the two; a set but non-UTF-8 value is a
/// typed error naming the variable.
pub(crate) fn seam(var: &str, default: &str) -> Result<String, ChannelError> {
    Ok(env_nonempty(var)?.unwrap_or_else(|| default.to_string()))
}

/// Lowercase hex sha256 of `bytes` (the sha2 crate, ADR-0069 §5): the one
/// checksum helper both flows' verifications run through — the upgrade's
/// `SHA256SUMS` entry check and the extension tarball's signed-index
/// check (ADR-0431 §4 step 3).
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(bytes))
}

/// The release channel's platform vocabulary (ADR-0068): the one os/arch
/// pair every channel asset is named by — `upgrade`'s platform binary
/// `relate-<os>-<arch>` (ADR-0250 §2) and the channel index's
/// per-platform extension tarball rows (ADR-0431 §1) speak the same
/// pair, so the Rust→channel mapping is one table here, not one per
/// consumer (the os/arch picking the upgrade already does, generalized
/// to the extension assets). The words come from `crate::platform`, the
/// vocabulary the feedback report shares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Platform {
    /// The channel's os name (`darwin`, `linux`).
    pub(crate) os: &'static str,
    /// The channel's arch name (`amd64`, `arm64`).
    pub(crate) arch: &'static str,
}

/// Map a Rust `std::env::consts::{OS, ARCH}` pair onto the channel's
/// platform vocabulary (ADR-0068 via ADR-0431 §1: macOS and Linux only),
/// or `None` on a host with no channel platform — each consumer renders
/// its own authored refusal over the `None` (`upgrade` adds its
/// Windows-specific manual-download pointer; the channel names the
/// platform and the package). Pure over the given pair, so the table is
/// unit-testable from any host.
pub(crate) fn platform_for(os: &str, arch: &str) -> Option<Platform> {
    // The channel's restriction (macOS and Linux only) over the shared
    // vocabulary tables: the words are `platform`'s, the pair is ours.
    if !matches!(os, "macos" | "linux") {
        return None;
    }
    Some(Platform {
        os: crate::platform::os_word(os)?,
        arch: crate::platform::arch_word(arch)?,
    })
}

/// The host's own platform in the channel vocabulary, or `None` — the
/// pair the caller's refusal names when absent.
pub(crate) fn host_platform() -> Option<Platform> {
    platform_for(std::env::consts::OS, std::env::consts::ARCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_policy_keeps_a_non_empty_value_and_defaults_on_unset_or_empty() {
        // The one pinned env policy (ADR-0250 §1, ADR-0358 §8): a set,
        // non-empty value wins; unset and empty both mean absent. The pure
        // core is tested directly, so no test races the process environment.
        assert_eq!(
            classify_env("V", Err(std::env::VarError::NotPresent)).unwrap(),
            None
        );
        assert_eq!(classify_env("V", Ok(String::new())).unwrap(), None);
        assert_eq!(
            classify_env("V", Ok("http://127.0.0.1:9".to_string())).unwrap(),
            Some("http://127.0.0.1:9".to_string())
        );
    }

    #[test]
    fn env_policy_rejects_a_non_unicode_value_naming_the_variable() {
        // A set but non-UTF-8 value must not read as unset: it aborts with a
        // typed single-line error naming the variable, for the seams and the
        // GITHUB_TOKEN read alike.
        let err = classify_env(
            "GITHUB_TOKEN",
            Err(std::env::VarError::NotUnicode(std::ffi::OsString::from(
                "not utf-8",
            ))),
        )
        .unwrap_err();
        assert_eq!(err.to_string(), "GITHUB_TOKEN is set but not valid Unicode");
    }

    #[test]
    fn sha256_hex_known_answers() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn platform_for_maps_every_channel_platform_and_refuses_the_rest() {
        // The one os/arch table (ADR-0068 via ADR-0431 §1): the four
        // channel platforms, and None for every other pair — the consumers'
        // authored refusals render the None (`upgrade`'s Windows pointer,
        // the channel's platform-and-package refusal).
        let platform = |os, arch| platform_for(os, arch).map(|p| (p.os, p.arch));
        assert_eq!(platform("macos", "x86_64").unwrap(), ("darwin", "amd64"));
        assert_eq!(platform("macos", "aarch64").unwrap(), ("darwin", "arm64"));
        assert_eq!(platform("linux", "x86_64").unwrap(), ("linux", "amd64"));
        assert_eq!(platform("linux", "aarch64").unwrap(), ("linux", "arm64"));
        for (os, arch) in [
            ("windows", "x86_64"),
            ("windows", "aarch64"),
            ("freebsd", "x86_64"),
            ("macos", "riscv64"),
            ("linux", "x86"),
        ] {
            assert!(
                platform_for(os, arch).is_none(),
                "{os}/{arch} must carry no channel platform"
            );
        }
    }

    #[test]
    fn host_platform_resolves_the_running_host_through_the_table() {
        // The host pair must come from the same table the pure mapping is
        // pinned over — `std::env::consts` are the only inputs, so every
        // supported test host (darwin/linux, amd64/arm64) resolves.
        assert_eq!(
            host_platform(),
            platform_for(std::env::consts::OS, std::env::consts::ARCH)
        );
        assert!(host_platform().is_some(), "the test hosts are darwin/linux");
    }
}
