# Releasing Relate

Releases are cut manually via the **Release** GitHub Actions workflow
(`.github/workflows/release.yml`). There are no tag pushes by hand — the
workflow computes the version, builds everything, tags, and publishes.
The versioning and process policy is recorded in
[ADR-0067](docs/adr/0067-release-versioning-and-process.md).

## Dispatch a release

From the CLI:

```sh
gh workflow run release.yml -f bump=patch   # or minor / major
```

Or from the browser: **Actions → Release → Run workflow → choose `bump`**
(`patch`, `minor`, or `major`; default `patch`).

**Offline (GitHub-only) releases**: dispatch with `offline=true` —
`gh workflow run release.yml -f bump=patch -f offline=true` — to build every
artifact, tag, and publish the GitHub release with all assets and the
generated notes, without touching anything outside GitHub: no relate-web
poke (`releases.json` and `install.sh` are still attached to the release for
a later backfill), no Slack announcement, no self-update canary. Use it
while the website and announcement channels are being stood up; a later
non-offline release-notes dispatch can still announce the tag.

**Skipping only the registries**: dispatch with `registries=false` —
`gh workflow run release.yml -f bump=patch -f registries=false` — for a
full release (website poke, Slack, canary) that does not chain the
registry publish. Use it while a registry's trusted-publishing setup is
pending; a later manual `publish-registries.yml` dispatch against the
tag publishes the registries after the fact.

The version is derived from the existing `v*` git tags plus the chosen bump,
not from the workspace version. The first ever run (no `v*` tags yet)
produces `v0.1.0` regardless of the bump choice — the first release cut
from this repository; the old repository’s tags never came across.

## What the run does, in order

1. **Version bump** — versions are bumped across the Cargo workspace and the
   Python package, the changelog is rotated, and the bump commit is pushed to
   `main`.
2. **Builds** — CLI, FFI, and Python artifacts are built from that commit.
3. **Codesign and notarize (macOS)** — the darwin legs sign each macOS CLI
   binary with a Developer ID Application identity and have it notarized by
   Apple via the shared composite action
   [`.github/actions/macos-notarize/action.yml`](.github/actions/macos-notarize/action.yml),
   so Gatekeeper accepts it (see
   [Apple signing and notarization](#apple-signing-and-notarization),
   [ADR-0123](docs/adr/0123-macos-codesign-notarize.md)).
4. **Tag** — once the builds are green, tag `vX.Y.Z` is pushed.
5. **Sign** — `SHA256SUMS` is generated with the release tag as its first
   line (`# relate-release-version: vX.Y.Z`), then signed with the
   release signing key (`ssh-keygen -Y sign`, namespace
   `relate-release`); the result is published as `SHA256SUMS.sig`. The
   version header is inside the signed bytes, so `install.sh` and
   `relate upgrade` can reject a validly signed older manifest served
   for another requested release (see
   [ADR-0457](docs/adr/0457-signed-manifest-carries-version.md) and
   [Signed manifest version](#signed-manifest-version)). The step fails when
   `RELEASE_SIGNING_PRIVATE_KEY` is unset, so a release cannot ship unsigned
   (see [Signing key](#signing-key), [ADR-0115](docs/adr/0115-sshsig-release-signing.md)).
6. **GitHub release** — the release is created for the tag with all artifacts,
   `SHA256SUMS`, `SHA256SUMS.sig`, and generated release notes.
7. **Release discovery files** — the `publish-releases-json` job regenerates
   `releases.json` — the release-discovery document `relate upgrade` /
   `update` reads
   ([ADR-0250](docs/adr/0250-releases-json-update-discovery.md)) — from the
   live release list after the release notes have been prepended to the
   release body, attaches it and `install.sh` to the release's assets, and
   pokes the `relate-web` repository with a `repository_dispatch`
   (`{kind: cli, tag}`) to pull and serve them at
   `https://relatedb.ai/...` ([ADR-0546](docs/adr/0546-release-discovery-via-relate-web.md)).
   The poke skips with a notice while `relate-web` is not ready (no
   `RELATE_WEB_DISPATCH_TOKEN` secret, or the repository does not answer);
   a manual run of the **Publish releases.json** workflow re-attaches the
   files and re-fires the poke — use that to backfill after a release
   published outside this pipeline (e.g. a hand-cut `gh release create`).
8. **Self-update smoke** — the `self-update-smoke` job runs
   [`.github/scripts/self-update-smoke.sh`](.github/scripts/self-update-smoke.sh):
   it downloads the previous stable release's `relate-linux-amd64`
   binary plus the release's `SHA256SUMS` manifest, checksum-verifies the
   binary against the manifest entry before executing it, runs
   `relate upgrade` on that isolated copy, and asserts the
   upgraded copy reports the new version — the live self-update path a real
   user rides (`releases.json`, release asset, upgrade), proven end to end.
   The inner upgrade reads the live `releases.json`, which the relate-web
   deploy must already serve with the new version as latest; while the
   site lags the release list the smoke skips with a stated reason
   (the live-site currency gate). Two more cases skip the same way: no
   previous stable release (the expected release is the first stable
   release ever), and a previous release older than the `v0.12.6` floor
   (the first release carrying the API-fallback route), which cannot
   self-update while the repository is internal
   ([ADR-0358](docs/adr/0358-upgrade-download-route.md)).

## Registry publishing

npm (ADR-0278), PyPI, and crates.io (ADR-0560) publish from one workflow,
[`.github/workflows/publish-registries.yml`](.github/workflows/publish-registries.yml),
which the release run calls once the GitHub release and its assets exist
(skipped by `offline=true`). All three use OIDC trusted publishing — no
API tokens in CI.

The workflow also dispatches by hand with a release tag, per-registry
toggles (`npm`, `pypi`, `crates-io`), and dry-run knobs
(`pypi-target=testpypi`, `npm-dry-run`, `crates-io-dry-run`), so a
partial publish is recovered — and auth is smoke-tested — without
re-cutting a release:

```sh
gh workflow run publish-registries.yml -f tag=v0.16.2 \
  -f pypi=true -f npm=false -f crates-io=false -f pypi-target=testpypi
```

Artifacts always come from the release's assets (wheels/sdist, npm
tarballs); crates.io publishes from a checkout of the tag. Every asset
must verify against the release run's build-provenance attestation
(`actions/attest-build-provenance` in the release's create-release job,
`gh attestation verify` here) — so the tag must be one cut **after** the
attestation step landed; older tags fail verification by design. Each
registry's one-time setup — the npm placeholder publishes and trusted
publishers, the PyPI/TestPyPI pending publisher, the crates.io bootstrap
— is a runbook item in
[docs/PUBLISHING.md](docs/PUBLISHING.md); the npm and crates.io jobs are
fail-closed (`&& false` guards) until theirs is done.

## Extension package releases

First-party extension packages release through their own dispatch,
[`.github/workflows/release-extensions.yml`](.github/workflows/release-extensions.yml).
The workflow takes a single input — `package` — and one dispatch releases
one package:

```sh
gh workflow run release-extensions.yml -f package=<name>
```

The table of first-party packages is
[`distribution/extension-packages.json`](distribution/extension-packages.json)
(each row names the package, the crate directory that builds its
executable, and its `extension.json` manifest). Its four current rows:

```sh
gh workflow run release-extensions.yml -f package=git-to-graph
gh workflow run release-extensions.yml -f package=fs-to-graph
gh workflow run release-extensions.yml -f package=dungeon
gh workflow run release-extensions.yml -f package=ideas
```

Each dispatch builds the package for the four channel platforms, signs and
notarizes the darwin executables, packs the tarballs, tags
`relate-extensions-<name>-<X.Y.Z>`, cuts the release, then chains the
signed-index publish — the signed `extensions.json` pair rides the
extension release's assets and relate-web is poked to pull it
([ADR-0546](docs/adr/0546-release-discovery-via-relate-web.md)) — see
[the channel and its publisher](docs/api/extensions/spec.md#the-channel-and-its-publisher).

**A CLI release that ships extension-system or importer changes must
dispatch a package release for every first-party row.** The channel lists
a package only after that package's first dispatch: the v0.14.0 lab
retirement shipped with zero dispatches and left `extension install`
failing on a 404 index. Package releases otherwise run on their own
cadence — a package release bumps only its index row, and the CLI release
need not move
([ADR-0431 §8](docs/adr/0431-cli-extension-distribution.md#8-compatibility-the-protocol-version-is-the-only-coupling)).

## Signing key

Releases are signed with a single Ed25519 key under the OpenSSH SSHSIG
namespace `relate-release` ([ADR-0115](docs/adr/0115-sshsig-release-signing.md)).
The private key is an **unencrypted** OpenSSH private key held only as the
Actions secret `RELEASE_SIGNING_PRIVATE_KEY` on `relatedb/relate` — the
secret itself is the protection, so no local copy is kept. The public key is
committed as `distribution/release-signing-key.pub` and pinned byte-identical
in `surfaces/relate-cli/src/upgrade_cmd.rs` and
`distribution/install.sh`; verification never fetches it from the release.

### Signed manifest version

`SHA256SUMS` starts with the release tag as a comment line
([ADR-0457](docs/adr/0457-signed-manifest-carries-version.md)):

```text
# relate-release-version: vX.Y.Z
<64-hex>  relate-linux-amd64
...
```

The header is inside the SSHSIG-signed bytes. `install.sh` and
`relate upgrade` require it to equal the release they were asked to
install, and reject a mismatch before downloading the binary. A manifest
without the header is a legacy manifest: it is accepted (with a warning) only
for releases at or below **v0.14.0**, the last release published before the
header; every later release must carry it. The release workflow writes the
header from the computed tag (`RELEASE_TAG`) in the `Generate SHA256SUMS`
step — it must precede the signing step, and never be edited after signing.

### First-time setup

1. Generate the keypair (no passphrase: CI cannot answer a prompt):

   ```sh
   ssh-keygen -t ed25519 -C relate-release \
     -f relate-release-signing-key -N ''
   ```

2. Check that the private key derives the public key you are about to pin:

   ```sh
   ssh-keygen -y -f relate-release-signing-key
   ```

3. Upload it as the secret, then delete the local copy:

   ```sh
   gh secret set RELEASE_SIGNING_PRIVATE_KEY < relate-release-signing-key
   rm relate-release-signing-key
   ```

4. Commit the `.pub` as `distribution/release-signing-key.pub`, and paste the
   same single line into `RELEASE_SIGNING_PUBLIC_KEY` in `upgrade_cmd.rs` and
   `SIGNING_KEY` in `install.sh`.

### Rotation

A rotation is a breaking change: each client trusts only the key pinned in its
own copy of the binary or script, so the new key must ship before anything it
signs can be trusted.

1. Generate the new keypair and update the secret (first-time setup, steps 1–3).
2. Update the pinned public key in all three places on `main`:
   `distribution/release-signing-key.pub`, `upgrade_cmd.rs`, and `install.sh`.
3. Cut a release. It signs `SHA256SUMS` with the new key, and its
   `publish-releases-json` job attaches the updated `install.sh` to the
   release's assets for relate-web to serve; until that release runs the
   published installer still carries the old key.
4. A binary installed before the rotation pins the old key, so its
   `relate upgrade` fails closed against a new-key signature — reinstall via
   the one-line installer to pick up the new pinned key.

## Apple signing and notarization

The macOS CLI binaries are signed with a Developer ID Application identity and
notarized by Apple so Gatekeeper accepts a downloaded `relate`
([ADR-0123](docs/adr/0123-macos-codesign-notarize.md)). One shared composite
action — [`.github/actions/macos-notarize/action.yml`](.github/actions/macos-notarize/action.yml)
— runs in both darwin legs of the `cli` job, after the binary is renamed and
before it is uploaded, so only signed, notarized bytes leave the runner. The
action is fail-closed: it self-verifies the signed binary with the
`codesign --verify -R` check described below and fails the release before
tagging if signing or notarization does not hold.

The action signs each architecture separately (no `lipo`/universal merge) with
`codesign --force --options runtime --timestamp` (hardened runtime, secure
timestamp), zips the binary, and submits it with
`xcrun notarytool submit --wait`; a rejection fails the leg rather than
publishing an un-notarized binary. It then verifies acceptance with
`codesign --verify -R` against Gatekeeper's notarized Developer ID exec
requirement (spctl assesses app bundles only, not bare binaries). The `.p12`
is imported into an ephemeral keychain that is deleted afterwards.

**Stapling is not used and cannot be:** `stapler` supports only `.dmg`, `.app`,
and signed flat `.pkg`, not bare binaries or zips, so notarization is checked
online by Gatekeeper on first launch. Only the CLI binaries are signed — the
FFI dylib and Python wheels are out of scope and remain unsigned.

### Secrets

Five repository secrets, using the same names as the sibling `neo4j-cli`
repository:

| Secret | Holds |
| --- | --- |
| `MACOS_SIGN_P12` | base64-encoded Developer ID Application certificate, exported as a PKCS#12 (`.p12`) bundle |
| `MACOS_SIGN_PASSWORD` | the password for that `.p12` |
| `MACOS_NOTARY_KEY_ID` | App Store Connect API key ID |
| `MACOS_NOTARY_ISSUER_ID` | App Store Connect API issuer ID |
| `MACOS_NOTARY_KEY` | base64-encoded App Store Connect API private key (`.p8`); a raw PEM is also accepted |

### Apple first-time setup

1. Export the Developer ID Application certificate from the Keychain as a
   `.p12`, then base64-encode it into the secret and set its password:

   ```sh
   base64 -i DeveloperIDApplication.p12 | gh secret set MACOS_SIGN_P12
   gh secret set MACOS_SIGN_PASSWORD   # the .p12 export password
   ```

2. Create an App Store Connect API key with notarization access, then set the
   three notary secrets from the key's ID, the issuer ID, and the `.p8`:

   ```sh
   gh secret set MACOS_NOTARY_KEY_ID     # the key's ID
   gh secret set MACOS_NOTARY_ISSUER_ID  # the issuer ID
   base64 -i AuthKey_XXXXXXXXXX.p8 | gh secret set MACOS_NOTARY_KEY
   ```

3. Delete the local `.p12` and `.p8` copies; the secrets are the only copies
   kept.

### Rotating Apple credentials

The certificate expires and the API key can be revoked, so rotate whichever
credential is affected and re-run the corresponding first-time setup commands:

1. Export the renewed Developer ID Application certificate to a `.p12`, then
   update `MACOS_SIGN_P12` and `MACOS_SIGN_PASSWORD`.
2. Create a replacement App Store Connect API key, then update
   `MACOS_NOTARY_KEY_ID`, `MACOS_NOTARY_ISSUER_ID`, and `MACOS_NOTARY_KEY`.
3. Cut a release to prove the new credentials. The darwin legs fail closed on a
   missing, bad, or expired credential, so a broken rotation cannot ship an
   unsigned binary.

### Runbook item

After the first signed release, fresh-download the macOS binary and confirm
Gatekeeper accepts it without a warning:

```sh
NOTARIZED_REQ='anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and notarized'
curl -LO https://github.com/relatedb/relate/releases/latest/download/relate-darwin-arm64
chmod +x relate-darwin-arm64
codesign --verify --check-notarization --verbose=2 -R="$NOTARIZED_REQ" relate-darwin-arm64
./relate-darwin-arm64 --version
```

where `$NOTARIZED_REQ` is Gatekeeper's notarized Developer ID exec
requirement (spctl cannot be used here: it assesses app bundles only, not
bare binaries). The `chmod +x` is required because `curl -LO` downloads
without an execute bit.

The first execution must run with no Gatekeeper warning, and `codesign --verify
-R` must pass.

## Failure recovery

- **Failed before the tag was pushed:** safe to re-dispatch the same bump.
  No tag exists to clean up. The version-bump commit may already be on
  `main`, but the version is recomputed from git tags, so a tagless rerun
  derives the same version.
- **Notarization rejected or signing failed:** the darwin leg fails, so no
  artifact is uploaded and the tag job never ran. Fix the cause — a rejection
  prints the notary log, an expired credential is a
  [rotation](#rotating-apple-credentials) — then re-dispatch the same bump: no tag exists, so the
  tagless-rerun bullet above applies and the re-dispatch is safe.
- **Failed after the tag was pushed:** do **not** delete or reuse the pushed
  tag. Re-dispatching computes the *next* version forward from the existing
  tag — bump forward and ship that instead.
