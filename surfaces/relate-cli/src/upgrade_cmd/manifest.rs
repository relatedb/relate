//! The signed `SHA256SUMS` manifest contract (ADR-0115, ADR-0457): the
//! version header carried inside the signed bytes, the binding of that
//! version to the release the metadata requested, the bounded legacy window,
//! and the checksum entry lookup. Everything here is pure; the upgrade flow
//! owns the network, the pinned-key verification, and the console output.

use super::UpgradeError;

/// The first line of a signed `SHA256SUMS` names the release it belongs to
/// (ADR-0457 §1). The exact spelling is load-bearing: `install.sh` matches
/// the identical literal, and both parsers treat any other spelling as a
/// headerless (legacy) manifest. The header is inside the SSHSIG-signed
/// bytes, so the version a manifest claims cannot be altered without
/// invalidating the signature.
const SUMS_VERSION_PREFIX: &str = "# relate-release-version: ";

/// The last release published before the signed version header existed
/// (ADR-0457 §3). A headerless manifest is a legacy manifest and is accepted
/// only when the requested version is at or below this ceiling; every later
/// release must carry the header. The constant is frozen, never raised;
/// `scripts/tests/test_install_sh.sh` holds `install.sh`'s copy to this value.
const LEGACY_MANIFEST_CEILING: (u64, u64, u64) = (0, 14, 0);

/// [`LEGACY_MANIFEST_CEILING`] in the `vX.Y.Z` spelling the diagnostics use,
/// so the messages cannot drift from the constant.
pub(super) fn legacy_ceiling_label() -> String {
    let (major, minor, patch) = LEGACY_MANIFEST_CEILING;
    format!("v{major}.{minor}.{patch}")
}

/// The release tag the signed manifest claims, from the header line
/// (ADR-0457 §1). `None` when the manifest has no header (legacy) — including
/// an empty document, a later-line header, or the no-space spelling
/// `# …-version:vX.Y.Z`, all of which `install.sh` also treats as
/// headerless. Only the first line is consulted: the header's position is
/// part of the wire format the release pipeline writes.
fn signed_manifest_version(sums: &str) -> Option<&str> {
    sums.lines().next()?.strip_prefix(SUMS_VERSION_PREFIX)
}

/// The outcome of binding the signed manifest's version to the requested
/// release (ADR-0457 §2). The caller matches it exhaustively to decide
/// whether to warn about the legacy window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ManifestBinding {
    /// The signed header named the requested release.
    Authenticated,
    /// No header; accepted as a pre-header legacy manifest for the requested
    /// release (ADR-0457 §3).
    Legacy,
}

/// Bind the authenticated `SHA256SUMS` to the release the metadata requested
/// (ADR-0457 §2). The manifest is already signed; this check makes the
/// version inside those signed bytes equal the requested tag, so a validly
/// signed older manifest cannot be replayed for a newer release. A headerless
/// manifest is legacy and accepted only at or below
/// [`LEGACY_MANIFEST_CEILING`] (§3), the releases that genuinely predate the
/// header. Pure, so every case is unit-testable without a socket.
pub(super) fn bind_signed_version(
    sums: &str,
    requested_tag: &str,
    requested_version: (u64, u64, u64),
) -> Result<ManifestBinding, UpgradeError> {
    // Normalize once, where the value enters the decision: the arms below
    // compare and interpolate the trimmed form.
    match signed_manifest_version(sums).map(str::trim) {
        // Checked before equality so an empty header can never match a caller
        // that passed an empty tag: it is a publish bug, not a release.
        Some("") => Err(UpgradeError::reinstall(format!(
            "SHA256SUMS from {requested_tag} carries an empty version header; not upgrading"
        ))),
        Some(version) if version == requested_tag => Ok(ManifestBinding::Authenticated),
        // version_binding: the signed manifest names a different release than
        // the metadata requested (a replay or a mismatched publish).
        Some(version) => Err(UpgradeError::reinstall(format!(
            "SHA256SUMS is for {version}, not the requested release {requested_tag}; not upgrading"
        ))),
        // Legacy manifests predate the header; accept one only for a release
        // old enough to have shipped without it (ADR-0457 §3).
        None if requested_version <= LEGACY_MANIFEST_CEILING => Ok(ManifestBinding::Legacy),
        // version_binding: a requested release above the legacy ceiling came
        // with a headerless manifest, so its version is unauthenticated.
        None => Err(UpgradeError::reinstall(format!(
            "SHA256SUMS from {requested_tag} has no signed version header; refusing to install a \
             manifest whose version is not authenticated (legacy manifests are accepted only at \
             or below {})",
            legacy_ceiling_label()
        ))),
    }
}

/// Find the sha256 for `name` in a `SHA256SUMS` manifest. Lines are
/// `<64-hex>  <name>` (`shasum -a 256` output: two spaces, no binary-mode
/// marker); like install.sh we also tolerate extra whitespace and a `*`
/// binary marker, and the ADR-0457 version header is simply a line that does
/// not parse as an entry. Returns the lowercase hex digest.
pub(super) fn sha256sums_entry(sums: &str, name: &str) -> Option<String> {
    for line in sums.lines() {
        let Some(hash) = line.get(..64) else {
            continue;
        };
        if !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        // The 64 bytes proven ASCII above end on a char boundary, so the
        // remainder starts exactly at 64: one invariant, not a second fallible
        // lookup.
        let rest = &line[64..];
        if !rest.starts_with([' ', '\t']) {
            continue;
        }
        let entry = rest.trim_start_matches([' ', '\t']);
        let entry = entry.strip_prefix('*').unwrap_or(entry);
        if entry == name {
            return Some(hash.to_ascii_lowercase());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    // The one-liner is pinned from its one source, the parent module: every
    // abort the `reinstall` constructor renders must carry it.
    use super::super::INSTALL_ONE_LINER;

    #[test]
    fn signed_manifest_version_reads_the_first_line_header() {
        assert_eq!(
            signed_manifest_version("# relate-release-version: v0.15.0\nabc  x\n"),
            Some("v0.15.0")
        );
        // Surrounding whitespace is tolerated; the value is compared trimmed.
        assert_eq!(
            signed_manifest_version("# relate-release-version:  v0.15.0 \n"),
            Some(" v0.15.0 ")
        );
        // The written prefix is exact: a no-space spelling or a tab after the
        // colon is headerless here, and `install.sh` matches the same literal.
        assert_eq!(
            signed_manifest_version("# relate-release-version:v0.15.0\n"),
            None
        );
        assert_eq!(
            signed_manifest_version("# relate-release-version:\tv0.15.0\n"),
            None
        );
        // No header: legacy. An empty document has none either.
        assert_eq!(signed_manifest_version("abc  x\n"), None);
        assert_eq!(signed_manifest_version(""), None);
        // Only the first line is the header; a later line is not.
        assert_eq!(
            signed_manifest_version("abc  x\n# relate-release-version: v0.15.0\n"),
            None
        );
    }

    #[test]
    fn bind_signed_version_accepts_the_matching_manifest() {
        let sums = "# relate-release-version: v0.15.0\nabc  x\n";
        assert_eq!(
            bind_signed_version(sums, "v0.15.0", (0, 15, 0)).unwrap(),
            ManifestBinding::Authenticated
        );
        // Both ends of the value are trimmed, and `install.sh` does the same;
        // the written form has exactly one space, this pins the tolerance.
        let padded = "# relate-release-version:  v0.15.0 \nabc  x\n";
        assert_eq!(
            bind_signed_version(padded, "v0.15.0", (0, 15, 0)).unwrap(),
            ManifestBinding::Authenticated
        );
    }

    #[test]
    fn bind_signed_version_rejects_a_mismatched_or_replayed_manifest() {
        // The same check serves the version-mismatch and the replay case: a
        // validly signed older manifest answered for a newer request.
        for (header, requested_tag, requested_version) in [
            ("v0.14.0", "v0.15.0", (0, 15, 0)),
            ("v0.20.0", "v0.15.0", (0, 15, 0)),
            ("0.15.0", "v0.15.0", (0, 15, 0)),
        ] {
            let sums = format!("# relate-release-version: {header}\nabc  x\n");
            let err = bind_signed_version(&sums, requested_tag, requested_version)
                .unwrap_err()
                .stderr_message();
            assert_eq!(err.lines().count(), 1, "one line: {err:?}");
            assert!(err.contains("SHA256SUMS is for"), "{err:?}");
            assert!(err.contains(header), "names the signed version: {err:?}");
            assert!(
                err.contains(requested_tag),
                "names the requested release: {err:?}"
            );
            assert!(err.contains(INSTALL_ONE_LINER), "{err:?}");
        }
    }

    #[test]
    fn bind_signed_version_accepts_legacy_manifests_at_or_below_the_ceiling() {
        // ADR-0457 §3: releases that predate the header are the only
        // legitimate headerless manifests. The ceiling itself and anything
        // older are accepted; the outcome names the legacy window.
        for version in [(0, 14, 0), (0, 13, 9), (0, 0, 1)] {
            assert_eq!(
                bind_signed_version("abc  x\n", "v0.14.0", version).unwrap(),
                ManifestBinding::Legacy
            );
        }
    }

    #[test]
    fn bind_signed_version_rejects_legacy_manifests_above_the_ceiling() {
        // A headerless manifest for a release that should carry the header is
        // a replay (or a broken publish): reject before the binary download.
        for version in [(0, 15, 0), (0, 14, 1), (1, 0, 0)] {
            let err = bind_signed_version("abc  x\n", "v0.15.0", version)
                .unwrap_err()
                .stderr_message();
            assert_eq!(err.lines().count(), 1, "one line: {err:?}");
            assert!(err.contains("no signed version header"), "{err:?}");
            assert!(err.contains("v0.15.0"), "{err:?}");
            assert!(err.contains(INSTALL_ONE_LINER), "{err:?}");
        }
    }

    #[test]
    fn bind_signed_version_rejects_an_empty_header() {
        // A header with no value is a publish bug, not a legacy manifest: it
        // must not fall into the legacy window.
        let err = bind_signed_version(
            "# relate-release-version: \nabc  x\n",
            "v0.14.0",
            (0, 14, 0),
        )
        .unwrap_err()
        .stderr_message();
        assert!(err.contains("empty version header"), "{err:?}");
        assert!(err.contains(INSTALL_ONE_LINER), "{err:?}");
    }

    #[test]
    fn bind_signed_version_rejects_an_empty_header_before_matching_an_empty_tag() {
        // The empty-header arm runs before equality, so a caller that passes
        // an empty requested tag still rejects the headerless-value manifest.
        let err = bind_signed_version("# relate-release-version: \nabc  x\n", "", (0, 14, 0))
            .unwrap_err()
            .stderr_message();
        assert!(err.contains("empty version header"), "{err:?}");
    }

    #[test]
    fn legacy_manifest_ceiling_is_the_last_pre_header_release() {
        // ADR-0457 §3 pins the ceiling to the last release published before
        // the header; raising it would silently keep replay protection off
        // for later releases. `install.sh`'s copy is held to this value by
        // the drift guard in `scripts/tests/test_install_sh.sh`.
        assert_eq!(LEGACY_MANIFEST_CEILING, (0, 14, 0));
        assert_eq!(legacy_ceiling_label(), "v0.14.0");
    }

    #[test]
    fn sha256sums_entry_finds_entries_below_the_version_header() {
        // ADR-0457 §1: the header is the first line, the checksum entries
        // follow. The entry parser must keep ignoring non-checksum lines.
        let good = "d".repeat(64);
        let sums = format!("# relate-release-version: v0.15.0\n{good}  relate-linux-amd64\n");
        assert_eq!(sha256sums_entry(&sums, "relate-linux-amd64"), Some(good));
    }

    #[test]
    fn sha256sums_entry_parses_two_space_lines() {
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        let sums = format!("{a}  relate-linux-amd64\n{b}  relate-darwin-arm64\n");
        assert_eq!(
            sha256sums_entry(&sums, "relate-linux-amd64"),
            Some(a.clone())
        );
        assert_eq!(sha256sums_entry(&sums, "relate-darwin-arm64"), Some(b));
        // An asset that has no line is None.
        assert_eq!(sha256sums_entry(&sums, "relate-windows-amd64.exe"), None);
    }

    #[test]
    fn sha256sums_entry_tolerates_star_marker_and_lowercases() {
        let upper = "A".repeat(64);
        let sums = format!("{upper}  *relate-darwin-arm64\n");
        assert_eq!(
            sha256sums_entry(&sums, "relate-darwin-arm64"),
            Some("a".repeat(64))
        );
    }

    #[test]
    fn sha256sums_entry_ignores_malformed_lines() {
        let good = "c".repeat(64);
        let sums = format!(
            "too-short  relate-linux-amd64\n\
             {good}-not-whitespace\n\
             zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz  relate-linux-amd64\n\
             {good}  relate-linux-amd64\n"
        );
        assert_eq!(sha256sums_entry(&sums, "relate-linux-amd64"), Some(good));
    }
}
