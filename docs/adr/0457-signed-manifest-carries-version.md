# ADR-0457: The signed release manifest carries the release version

Status: Proposed
Claim: 1ff95683ad9e72da75bbb0e9ffd5f9c4e19e89db

## Context

ADR-0115 authenticates the release manifest `SHA256SUMS` with an OpenSSH
SSHSIG signature verified against a public key pinned in both `install.sh`
and the `cypherlite` binary. The signature covers the manifest bytes, and the
manifest pins each CLI binary by SHA-256. What it does not cover is the
**version**: the manifest is a bare `shasum -a 256` list of file names and
digests, while the version a consumer believes it is installing comes from
outside the signed bytes.

- `cypherlite upgrade` reads `latest.tag` from `releases.json` on the project
  website (ADR-0250), then downloads `SHA256SUMS` from the release matching
  that tag. `releases.json` is not signed, and the tag is not part of the
  signed manifest.
- `install.sh` reads `tag_name` from the GitHub API's `releases/latest`
  response and downloads the same two documents.

A validly signed older manifest is therefore interchangeable with the newer
one at verification time: an actor who can serve the release assets (a mirror,
a proxy, a compromised release) can answer a request for `v0.20.0` with the
signed `SHA256SUMS` and binary of `v0.14.0`, and the checks pass. The upgrade
reports the version from `releases.json` while installing the older bytes —
a replay/downgrade, and the residual gap from security-audit finding 9
(LABS-609).

## Decision

**Put the release version inside the SSHSIG-signed `SHA256SUMS` bytes and
require every consumer to match it against the version it asked for.**

### 1. Wire format

The first line of `SHA256SUMS` is:

```text
# cypherlite-release-version: vX.Y.Z
```

followed by the existing `<64-hex>  <name>` entries. The value is the release
tag exactly as published (`v` prefix included). The header is inside the bytes
the signature covers, so changing it invalidates the signature. Both existing
manifest parsers already ignore non-checksum lines; the new line changes no
asset name and no download URL.

### 2. Enforcement

Both consumers verify, **after** the SSHSIG check and **before** the binary
download:

1. The first line has the header prefix.
2. Its value equals the tag the release metadata asked them to install
   (`releases.json`'s `latest.tag` for `upgrade`; the API's `tag_name` for
   `install.sh`).

The prefix is matched literally, including its single space after the colon:
any other spelling (no space, a tab, a later line) is a missing header, not a
different header. A header that is present but empty is a hard failure — a
publish bug, never a legacy manifest. A mismatch is a hard failure too:
nothing is downloaded past the manifest, and the running binary is untouched.
A missing header is a **legacy** manifest (see §3). Both implementations
apply those rules identically; the checksum tools they wrap treat the header
line as a non-entry.

The binary is bound to the header twice over: the header authenticates the
version, and the manifest entry authenticates the binary bytes. An attacker
cannot move a valid header onto another binary (the digest entry fails) or
move a valid binary under another version (the header fails). Replaying an
old signed manifest for a newer requested version fails the header equality.

### 3. Compatibility: bounded legacy window, not an open door

Releases published before this change have no header. The last such release
is **v0.14.0** (`LEGACY_MANIFEST_CEILING`). A headerless manifest is accepted
with a one-line warning only when the requested version is **at or below**
that ceiling; every requested version above it requires the header and is
rejected without one.

The tradeoff is stated plainly: a genuinely old release (`v0.14.0` or older)
can still be replayed while it remains a legitimate upgrade target, because no
signed version exists to compare. That window is finite and shrinking — every
release after `v0.14.0` carries the header, so no new version is ever
replayable. Accepting missing headers unconditionally would leave the hole
open forever; rejecting them unconditionally would break the installer and
upgrade path for the currently published release (and any user upgrading to
it) during the rollout. The ceiling is a frozen constant, not a moving one:
it names the last release known to predate the header and is not raised.

### 4. Release pipeline

`.github/workflows/release.yml` writes the header as the first line of
`SHA256SUMS` before the manifest is signed, using the same computed tag it
publishes. The signing step signs the resulting bytes unchanged.

### 5. Documentation and tests

`docs/api/cli/spec.md` (upgrade section), `RELEASING.md`, `README.md`, and
`docs/PUBLISHING.md` document the header and the legacy window. Tests pin:

- a matching header installs (manifest parsing and the flow's binding);
- a mismatched header is rejected, naming both versions;
- a present-but-empty header is rejected, not treated as legacy;
- the no-space spelling is headerless in both implementations;
- a missing header above the ceiling is rejected;
- a missing header at or below the ceiling is accepted with the warning;
- a replayed older signed manifest served for a newer requested version is
  rejected before the binary download — end to end against the committed
  real-key fixture, and in the `install.sh` harness;
- a drift guard in `scripts/tests/test_install_sh.sh` holds the installer's
  header prefix and ceiling constant equal to the Rust binding's.

## Alternatives considered

- **Sign a new `release.json` / `SHA256SUMS.json` metadata asset.** It would
  carry the version and the manifest digest. Rejected: a second signed asset
  means a new asset name in three consumers, a second signature-verification
  path, and a new compatibility matrix — for one string that fits in the
  manifest the signature already covers.
- **Put the version in the binary file name (`cypherlite-linux-amd64-v0.15.0`)
  and sign that.** Rejected: it breaks every documented asset name and URL
  (README, install.sh, release downloads) and still leaves the number outside
  the signed bytes; the signature covers file contents, not names.
- **Bind via the SSHSIG namespace or signer identity.** Rejected: the
  namespace is a fixed constant shared with the extension channel
  (ADR-0431), and both are part of the verification configuration, not the
  message; neither can carry a per-release value without rotating the pinned
  key or duplicating the verification root.
- **Make `releases.json` the signed document.** Rejected: it is a website
  file regenerated on every release and redeploy, not a release asset, and
  ADR-0250 explicitly treats it as selection metadata, never integrity. The
  binding belongs with the artifact it describes.
- **Accept any headerless manifest with a warning forever.** Rejected: a
  warning does not stop the downgrade, and an attacker simply serves the old
  format. The ceiling confines the exception to releases that predate the
  fix.
- **Reject every headerless manifest immediately.** Rejected: at rollout the
  newest published release is still `v0.14.0`; the install script on the
  website would fail for every user until the next release, and a user on
  `v0.13.x` could not reach `v0.14.0`. The ceiling keeps the legitimate old
  path working without extending protection it never had.
- **Compare only the version triple, not the full tag.** Rejected: the tag is
  the identity the metadata and the download URL use; accepting `0.15.0` for
  `v0.15.0` would invent a second spelling and weaken the equality the
  pipeline writes.

## Consequences

- `SHA256SUMS` gains one comment line. `shasum -a 256 -c` and
  `sha256sum -c` still verify the listed files (GNU coreutils prints an
  "improperly formatted line" warning for the comment); the two consumers
  parse the header themselves and ignore it as a checksum entry.
- A replayed old manifest is rejected for any release above `v0.14.0`, so
  `releases.json` can no longer redirect an upgrade to older signed bytes.
- The publish pipeline must write the header before signing; a release whose
  manifest lost the header ships uninstallable by current clients. The
  release workflow step is the single writer, and the signing step consumes
  its output unchanged.
- The legacy ceiling is a permanent reference to `v0.14.0`; removing it later
  is a new ADR, not a code cleanup.
- `install.sh` gains a small POSIX-sh version comparison (for the ceiling
  only); `upgrade` reuses its existing numeric triple parse.
- Older clients (built before this change) ignore the header line and keep
  working; they simply do not enforce the binding.

## Later references

ADR-0571 changes the tag provenance behind both places this decision names
the GitHub API for `install.sh`: the Context sentence that has the script
read `tag_name` from the `releases/latest` response, and §2's enforcement
point that makes that `tag_name` the tag the script was asked to install.
The requested tag now comes from the file host's `releases.json`
`latest.tag` (ADR-0250) — the same source `upgrade` reads — because the
extension channel publishes full releases into the same repository
(ADR-0431) and the latest-release endpoint names whichever release was
published last. The API's `tag_name` remains, now as the equality check on
the per-tag GitHub API fetch for the asset ids: the response's `tag_name`
must equal the requested tag, so the API describes exactly the release the
metadata named. The binding itself is unchanged — the signed header must
still equal the requested tag after the SSHSIG check and before the binary
download, and the empty-header, headerless-legacy, and v0.14.0 ceiling
rules stand as written.
