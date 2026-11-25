# Releasing Relate

Releases are cut manually via the **Release** GitHub Actions workflow
(`.github/workflows/release.yml`). There are no tag pushes by hand — the
workflow computes the version, builds everything, tags, and publishes the
GitHub release.
The versioning and process policy is recorded in
[ADR-0067](docs/adr/0067-release-versioning-and-process.md).

## Contents

1. [Dispatch a release](#dispatch-a-release)
2. [What the run does, in order](#what-the-run-does-in-order)
3. [Registry publishing](#registry-publishing)
4. [Extension package releases](#extension-package-releases)
5. [Signing key](#signing-key)
6. [Repository signing key](#repository-signing-key)
7. [Apple signing and notarization](#apple-signing-and-notarization)
8. [Windows signing](#windows-signing)
9. [Chocolatey package](#chocolatey-package)
10. [Failure recovery](#failure-recovery)

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

**Registry publishes are not chained by default**: the `registries` input
defaults to `false` — a plain release dispatch runs the full pipeline
(website poke, Slack, canary) but does not touch any registry. The
chained call's OIDC token names `release.yml`, and the registries'
trusted publishers register `publish-registries.yml` — the v1.0.0 PyPI
publish failed on exactly this mismatch — so publishing runs by a direct
`publish-registries.yml` dispatch against the tag (see
[Registry publishing](#registry-publishing)). Opt back into the chain
with `-f registries=true` only after registering `release.yml` on every
registry ([docs/PUBLISHING.md](docs/PUBLISHING.md)).

The version is derived from the existing `v*` git tags plus the chosen bump,
not from the workspace version. The first ever run (no `v*` tags yet)
produces `v0.1.0` regardless of the bump choice — the first release cut
from this repository; the old repository’s tags never came across.

## What the run does, in order

0. **Third-party licenses** (parallel with the bump, report-only) — the
   `licenses` job runs
   [`third-party-licenses.yml`](.github/workflows/third-party-licenses.yml):
   the shipped dependency closure is checked against the Apache-2.0-compatible
   allow-list in `deny.toml` and `THIRDPARTY-NOTICES.md` is checked for drift
   (see [Third-party licenses](#third-party-licenses)). It warns, never fails,
   and is not a `tag` prerequisite until enforcement is switched on.
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
   [ADR-0123](docs/adr/0123-macos-codesign-notarize.md)). The windows leg
   Authenticode-signs its binary at the same invocation point, remotely
   through DigiCert Software Trust Manager (see
   [Windows signing](#windows-signing),
   [ADR-0594](docs/adr/0594-windows-authenticode-signing.md)).
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
   `SHA256SUMS`, `SHA256SUMS.sig`, `assets.json` — the asset manifest
   mapping every attached asset to its sha256 and size, generated from the
   staged directory and gated in the same step so no asset ships without a
   manifest entry
   ([`scripts/release_asset_manifest.py`](scripts/release_asset_manifest.py);
   the manifest carries no entry for itself) — and GitHub's auto-generated
   notes (the full pull-request list). The chained **Release notes** workflow then
   regenerates the narrative: a trusted step harvests the tag range's
   merged pull requests into a corpus
   ([`.github/scripts/release-notes-corpus.sh`](.github/scripts/release-notes-corpus.sh),
   ADR-0618), a pinned strong model
   (`RELEASE_NOTES_MODEL` secret or variable overrides it) writes the
   curated highlights and the fixed-area change list from the changelog
   section plus the corpus, and the result is prepended to the release
   body above the auto-generated list. Re-run it for a published tag
   with `gh workflow run release-notes.yml -f version=<X.Y.Z>` —
   `publish=false` (the default) is a dry run that only prints the
   artifacts.
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
9. **Server image** — the `build-image` job builds the `relate-bolt`
   image from the release commit (`make docker`) and runs the docker smoke;
   it has no AWS access. The `publish-image` job then pushes it as `X.Y.Z`
   (no `latest` tag) to the DevRel production ECR repository
   (`715633473519.dkr.ecr.us-east-1.amazonaws.com/production-relatedb-repository`),
   where version tags are immutable. It authenticates through GitHub OIDC to
   an IAM role that trusts only `release.yml` running from `main`; the role
   ARN and repository name come from the `AWS_ECR_PUSH_ROLE_ARN` and
   `AWS_ECR_REPOSITORY` secrets. Offline releases skip both jobs.
10. **winget** — the `winget` job downloads the release's `assets.json`,
    generates the three winget manifests for the Windows channel with
    [`scripts/winget_manifests.py`](scripts/winget_manifests.py) — the
    installer URL is the tagged `relate-windows-amd64.exe` release asset and
    its sha256 is the manifest's own entry for it, so the package carries
    the published hash ([ADR-0659](docs/adr/0659-package-manager-distribution-channels.md) §4) —
    and validates them with `winget validate`. Unless the dispatch set
    `winget_dry_run` (which stops after validation and prints the submission
    it would make), the job then submits the version bump to
    `microsoft/winget-pkgs` through wingetcreate, using the `WINGET_TOKEN`
    secret; while that secret is unset the submission skips with a notice,
    like the relate-web poke. Offline releases skip the job.

## Third-party licenses

Every binary release is the Rust closure of four crates — `relate-cli` (the
CLI), `relate-ffi` (the native library under the JavaScript, Java, Go and
Swift clients), `relate-python` (the wheels) and `relate-wasm` (the WASM
bundle). Dev-dependencies and `testing/*` crates never ship and are out of
scope. `THIRDPARTY-NOTICES.md` at the repository root carries the license
text and the crate list for that closure, meeting the attribution
requirements of the libraries we ship; the release attaches it next to the
binaries (ADR-0605). Each license appears once, grouped by SPDX id, with the
SPDX list's standard text (`scripts/license-texts/<id>.txt`) and every crate
using it, with each crate's own copyright lines listed under it. When the
allow-list gains a license, add its text:
`curl -fsS https://raw.githubusercontent.com/spdx/license-list-data/main/text/<id>.txt -o scripts/license-texts/<id>.txt`.

**Maintain it on every dependency change** (a `Cargo.lock` change to a
shipped crate's closure) — and re-check it at release time:

```sh
cargo install --locked cargo-about@0.9.2 cargo-deny@0.20.2   # once
python3 scripts/third-party-licenses.py notices --write      # regenerate, commit (Python 3.11+)
python3 scripts/third-party-licenses.py all                  # drift + policy report
```

- **Policy** (`deny.toml`): the allow-list is the Apache-2.0-compatible
  permissive set the closure uses today, and it is the one spelling of that
  vocabulary — the tool derives cargo-about's accepted list from it, and
  `about.toml` holds only cargo-about's own settings. A new license is a
  deliberate edit to `deny.toml` alone — never an accident — and anything
  copyleft or non-commercial must be raised with the maintainers first.
- **Report-only today.** `third-party-licenses.yml` warns on a violation or a
  stale notices file; it does not fail the release. Once a few releases show
  it never false-fails, enforce it: pass `enforce: true` from `release.yml`'s
  `licenses` job and add that job to the `tag` job's `needs`.
- **Dry run:** `gh workflow run third-party-licenses.yml` (optionally
  `-f enforce=true`) runs the same job alone; the freshly generated notices
  are uploaded as the `third-party-notices` artifact.

## Embedded dependency manifests

The release jobs that build shipped Rust binaries — the CLI (its non-Windows
legs; the Windows leg belongs to the windows-authenticode-signing lane), the
FFI cdylib, the wasm bundle, and the server image's in-container build (the
Dockerfile's build stage, through a pinned cargo-auditable of its own) —
build through
[cargo-auditable](https://github.com/rust-secure-code/cargo-auditable),
installed from a version-plus-SHA-256 pinned release
(`scripts/install-cargo-auditable.sh`; the Dockerfile carries the musl
digest inline, the same pinned-release pattern as its sccache install).
Every built artifact embeds its
exact crate list: a Zlib-compressed JSON dependency tree in the `.dep-v0`
section, so a shipped binary can be audited without its `Cargo.lock`:

```sh
cargo audit bin relate-darwin-arm64   # prints the crate list, checks it against the RustSec advisory DB
cargo audit bin librelate_ffi.so      # the FFI cdylib
cargo audit bin relate_bg.wasm        # the wasm bundle (also: wasm-tools metadata show)
docker create --name relate-audit relate-bolt:<version> \   # the server image's binaries
  && docker cp relate-audit:/bin/relate . && docker rm relate-audit \
  && cargo audit bin relate
```

`trivy`, `grype`, `syft`, and `osv-scanner` read the same section when
scanning shipped artifacts — grype and osv-scanner read it directly off the
shipped container image. Exceptions: the FFI and XCFramework staticlibs
carry no manifest (archives have no link step for the injection), and the
PyPI wheels are built by maturin inside manylinux containers the wrapper
cannot reach. The injection is link-time only (a RUSTC_WORKSPACE_WRAPPER
chained over the sccache RUSTC_WRAPPER) — external dependency compiles are
byte-identical to plain builds, so the lanes' rust-cache keys, restored
dependency artifacts, and the Dockerfile's chef-cooked dependency layer all
stay valid; only workspace members recompile. sccache keys every compile on
the `CARGO_*` environment, and the wrapper sets `CARGO_AUDITABLE_ORIG_ARGS`,
so its entries are keyed apart from plain-build ones (dependency hits are
served within auditable builds) while the chained workspace-member compiles
come out non-cacheable and compile from source.

## Registry publishing

npm (ADR-0278), PyPI, and crates.io (ADR-0560) publish from one workflow,
[`.github/workflows/publish-registries.yml`](.github/workflows/publish-registries.yml),
run by direct dispatch against a release tag. The release run can also
chain it (`registries=true`, off by default — the chained OIDC identity
is the release workflow's, which the registries' publishers do not
accept; skipped by `offline=true`). All three use OIDC trusted
publishing — no API tokens in CI.

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
[docs/PUBLISHING.md](docs/PUBLISHING.md).

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

## Repository signing key

The APT and RPM repository metadata is signed with a GPG key separate from
the SSHSIG release key above: package managers verify repository metadata
(`Release`, `repomd.xml`), not the release manifest. The public key is
committed as `distribution/repo-signing-key.asc` with its 40-hex
fingerprint in `distribution/repo-signing-key.fingerprint` beside it —
users import that file, and it is the anchor the signing step checks
itself against. The private key is an **unencrypted** RSA-4096 key held
only as the Actions secret `REPO_GPG_KEY` on `relatedb/relate`
(ADR-0630): `scripts/sign-repo-metadata.sh` decodes it into a throwaway
GNUPGHOME, refuses a secret whose fingerprint is not the committed one,
signs the metadata, and verifies the fresh signature against the
committed public key before anything is published. The offline suite
`make test-sign-repo-metadata` proves the same contract with a fixture
keypair.

### Repository signing first-time setup

1. Generate the keypair (no passphrase: CI cannot answer a prompt) and
   note the fingerprint:

   ```sh
   gpg --batch --passphrase '' --pinentry-mode loopback \
     --quick-generate-key relate-repo-signing rsa4096 sign never
   gpg --list-secret-keys --with-colons | grep '^fpr:' | head -n 1 | cut -d: -f10
   ```

2. Commit the public half and its fingerprint:

   ```sh
   gpg --armor --export <FINGERPRINT> > distribution/repo-signing-key.asc
   printf '%s\n' <FINGERPRINT> > distribution/repo-signing-key.fingerprint
   ```

3. Upload the private half as the secret, then delete the local copy:

   ```sh
   gpg --armor --export-secret-keys <FINGERPRINT> | base64 | tr -d '\n' \
     | gh secret set REPO_GPG_KEY
   ```

4. Rehearse the secret against `main` — the dry-run job signs the
   fixture repository metadata and fails closed unless the secret is the
   committed key's private half:

   ```sh
   gh workflow run publish-registries.yml -f tag=vX.Y.Z \
     -f repo-signing-dry-run=true -f npm=false -f pypi=false \
     -f crates-io=false -f maven=false
   ```

### The RPM repository tree

`scripts/build_rpm_repo.py` turns a staged release binary into the RPM
package and the signed tree dnf reads, one architecture per run:

```sh
python3 scripts/build_rpm_repo.py --binary relate-linux-amd64 --version 1.2.0 \
  --glibc-floor GLIBC_2.35 --output rpm
```

It builds the package with `rpmbuild` — the real binary at
`/usr/lib/relate/relate`, a `/usr/bin/relate` symlink, the
`.relate-package-managed` provenance marker beside the binary, and
`Requires: glibc >= <floor>` from the release binary's glibc floor — then
writes the metadata with `createrepo_c` and signs `repomd.xml` through
`scripts/sign-repo-metadata.sh`, which needs `REPO_GPG_KEY` set. The output
tree also carries `relate.repo` and `repo-signing-key.asc`, which publish
at `https://relatedb.ai/rpm`; the repo file's `gpgkey=` points at that key
copy, so users import nothing by hand. `make test-release-scripts` covers
the spec and repo file, and its end-to-end case builds and verifies a real
signed tree where `rpmbuild` and `createrepo_c` are installed. `release.yml`'s
`rpm-repo` job runs it in a Fedora container and attaches the tree as
`relate-rpm-repo.tar.gz`.

### The APT repository

`release.yml`'s `apt-repo` job turns the release's own
`relate-linux-amd64` and `relate-linux-arm64` into the Debian package and
the signed repository tree that serves it (`scripts/build-apt-repo.sh`,
which takes the Debian architecture as its second argument and derives the
control's `Architecture`, the pool file name and the `binary-<arch>` index
path from it), attaches the trees as `relate-apt-repo-amd64.tar.gz` and
`relate-apt-repo-arm64.tar.gz`, and relate-web serves them at
`https://relatedb.ai/apt`. The tree holds
`dists/stable/{Release,Release.gpg,main/binary-<arch>/Packages*}`,
`pool/main/r/relate/relate_<version>_<arch>.deb`, and `repo-signing-key.asc`
beside them, so the documented install command fetches the key from the
same root. The package keeps the binary in `/usr/lib/relate` and symlinks
it to `/usr/bin/relate`, declares the ISA gate's glibc floor as
`libc6 (>= <floor>)` in `Depends`, and writes the provenance marker
`.relate-package-managed` beside the real binary.

Locally, `make test-build-apt-repo` covers the package and index contract
offline, and `scripts/check-apt-repo.sh <tree> GLIBC_2.35` installs from a
built tree in a clean `ubuntu:24.04` container, checking the signed
metadata, the reported version, the floor and the marker.

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

## Windows signing

The Windows CLI binary (`relate-windows-amd64.exe`) is signed with an
Authenticode signature so SmartScreen shows a signed publisher instead of
"unknown publisher" ([ADR-0594](docs/adr/0594-windows-authenticode-signing.md)).
One shared composite action —
[`.github/actions/windows-sign/action.yml`](.github/actions/windows-sign/action.yml)
— runs in the windows leg of the `cli` job, after the binary is renamed and
before it is uploaded, the same invocation point as the macOS action
(ADR-0123).

Signing is remote, through DigiCert Software Trust Manager: the
organization's code-signing keypairs are non-exportable and held by
DigiCert's key management, so no code-signing certificate material exists
on the runner or in the repository's secrets. The sign step runs
DigiCert's official signing action — pinned by full commit SHA in the
composite action, never a floating tag — which in simple-signing mode
installs and runs only the `smctl` client, authenticates a DigiCert ONE
API user from the `SM_*` step env, and signs the binary with the keypair
selected by its alias, using a SHA-256 digest and an RFC 3161 timestamp
(the signature stays valid after the certificate expires). The signed
binary is then verified on the runner with `Get-AuthenticodeSignature`:
the status must be `Valid`, a time-stamper certificate must be present,
and the signer certificate's thumbprint must equal the
`WINDOWS_SIGN_CERT_FINGERPRINT` variable (both compared as uppercase hex
without spaces), or the leg fails.

The action is skip-open, then fail-closed — the deliberate asymmetry with
the macOS action recorded in ADR-0594: while `WINDOWS_SIGN_API_KEY` is
unset it prints a workflow warning and leaves the binary untouched, so
the release pipeline runs before the DigiCert credentials exist. Once it
is set, the leg fails closed: a missing companion credential, any signing
failure — including the API user not being an authorized signer on the
keypair — or a failed verification fails the release before tagging, and
no unsigned Windows binary can ship from a green run. The API user's
client-authentication certificate, the one credential `smctl` needs as a
file, is decoded from its base64 secret by the workflow — not by the
action, which never writes certificate material to disk — and is deleted
from the runner in an `always()` cleanup step. No credential is ever
echoed.

Before the sign step, the action preflights the credentials with
`smctl healthcheck`: `smctl` is downloaded from the same CDN location the
pinned vendor action installs it from, but verified against a SHA-256
pinned in the action itself — not the CDN's own sidecar, which proves no
provenance — and that pin moves only by an explicit bump, the same rule
as the third-party action SHA pins. The preflight's scratch (the
downloaded binary and the healthcheck report) is removed in an
`always()` cleanup step.
[`scripts/check-smctl-healthcheck.sh`](scripts/check-smctl-healthcheck.sh)
classifies the report — `Authentication: 2FA` proceeds to signing; a
connected-but-1FA report fails naming the client-certificate secrets to
fix (`WINDOWS_SIGN_CLIENT_CERT_FILE_B64` /
`WINDOWS_SIGN_CLIENT_CERT_PASSWORD`); a not-connected or
connection-failed report fails naming `WINDOWS_SIGN_SM_HOST` /
`WINDOWS_SIGN_API_KEY`; a no-credentials report fails naming the
certificate-load pair. Without the preflight, the same failure reaches
DigiCert's API only at sign time, as a bare 403 ("User is not
multi-factor authenticated") that names neither the failing credential
nor the remedy — the state the first credentialed release run shipped
in.

### Windows secrets and variables

Three repository secrets and three repository variables:

| Name | Kind | Holds |
| --- | --- | --- |
| `WINDOWS_SIGN_API_KEY` | secret | the DigiCert ONE API key for the signing user (first factor) |
| `WINDOWS_SIGN_CLIENT_CERT_FILE_B64` | secret | the base64-encoded DigiCert ONE client-authentication certificate (`.p12`, second factor) for that user |
| `WINDOWS_SIGN_CLIENT_CERT_PASSWORD` | secret | the password for that client-authentication certificate |
| `WINDOWS_SIGN_SM_HOST` | variable | the DigiCert ONE client-authentication host, e.g. `https://clientauth.one.digicert.com` |
| `WINDOWS_SIGN_KEYPAIR_ALIAS` | variable | the alias of the code-signing keypair in Software Trust Manager |
| `WINDOWS_SIGN_CERT_FINGERPRINT` | variable | the SHA-1 fingerprint of the code-signing certificate that must sign |

### Windows first-time setup

1. In DigiCert ONE, create the signing user's API key and client
   authentication certificate (each is shown once at creation; DigiCert's
   public documentation walks through both), then store them:

   ```sh
   gh secret set WINDOWS_SIGN_API_KEY   # the DigiCert ONE API key
   base64 < client-auth.p12 | gh secret set WINDOWS_SIGN_CLIENT_CERT_FILE_B64
   gh secret set WINDOWS_SIGN_CLIENT_CERT_PASSWORD   # the certificate password
   ```

2. Set the host, keypair alias, and certificate fingerprint as repository
   variables (the alias and fingerprint come from the keypair's details
   in Software Trust Manager):

   ```sh
   gh variable set WINDOWS_SIGN_SM_HOST --body 'https://clientauth.one.digicert.com'
   gh variable set WINDOWS_SIGN_KEYPAIR_ALIAS --body '<keypair alias>'
   gh variable set WINDOWS_SIGN_CERT_FINGERPRINT --body '<certificate SHA-1 fingerprint>'
   ```

3. Delete the local `client-auth.p12` copy; the secrets are the only copy
   kept. The code-signing keypair itself never leaves DigiCert.

### Rotating Windows credentials

- **API key or client certificate:** create a fresh API key (or client
  authentication certificate) in DigiCert ONE, update the corresponding
  secret, then cut a release to prove the new credential.
- **Renewed code-signing certificate:** the keypair stays, but the new
  certificate carries a new SHA-1 fingerprint — update
  `WINDOWS_SIGN_CERT_FINGERPRINT` and cut a release. The leg fails
  closed when the pinned fingerprint no longer matches the keypair's
  certificate, so a half-done rotation cannot ship an unsigned binary.

### Bumping the pinned signing action

The composite action runs DigiCert's official signing action, pinned by
full commit SHA in [the action](.github/actions/windows-sign/action.yml)
— the `uses:` ref with its version comment, never a floating `@v1` tag.
The pin is the supply-chain gate, the same rule every third-party action
in the release workflows follows. To take a newer release of DigiCert's
action, resolve its tag to the commit SHA it names and update the pin
together with the version comment:

```sh
git ls-remote https://github.com/digicert/code-signing-software-trust-action.git refs/tags/v1
```

The tag also names the release (for example `v1.2.1`), which becomes the
new version comment. Commit; the next release proves the new pin
(ADR-0594). While the DigiCert credentials are unset the sign step is
skipped, so a stale pin never breaks the skip-open path — the bump only
matters once the leg starts signing.

### Windows runbook item

After the first signed release, fresh-download the Windows binary and
confirm the signature is valid, timestamped, and from the expected
publisher:

```powershell
curl.exe -LO https://github.com/relatedb/relate/releases/latest/download/relate-windows-amd64.exe
Get-AuthenticodeSignature .\relate-windows-amd64.exe |
  Format-List Status, TimeStamperCertificate,
    @{ Label = 'SignerSubject'; Expression = { $_.SignerCertificate.Subject } }
.\relate-windows-amd64.exe --version
```

The status must be `Valid` with a `TimeStamperCertificate` present, the
`SignerSubject` must be the subject of the code-signing certificate held
for the `WINDOWS_SIGN_KEYPAIR_ALIAS` keypair in DigiCert Software Trust
Manager, and the binary must report its version.

## Chocolatey package

Windows users can install the CLI from the Chocolatey community
repository:

```powershell
choco install relate
```

The package ([`distribution/chocolatey`](distribution/chocolatey)) is one
template for every release: `choco pack --version <X.Y.Z>` fills in the
version, the install script derives the release tag from it, fetches that
release's `assets.json` and installs nothing unless the downloaded
`relate-windows-amd64.exe` matches the manifest entry's SHA-256. The
binary lands in Chocolatey's package directory with the
`.relate-package-managed` marker beside it
(`{"manager":"chocolatey","upgrade":"choco upgrade relate"}`), so
`relate upgrade` refuses to self-replace
([ADR-0659](docs/adr/0659-package-manager-distribution-channels.md)).

The `chocolatey` job in
[`publish-registries.yml`](.github/workflows/publish-registries.yml)
packs the package, installs it from a local source on `windows-latest`
and asserts the installed `relate --version` reports the release version,
then pushes the `.nupkg`. It is on by default (`chocolatey=true`);
`chocolatey-dry-run=true` stops after the local install, before the push.
While the `CHOCOLATEY_API_KEY` secret is unset the push step warns and
skips — a direct dispatch still proves packaging and installation.

### Chocolatey first-time setup

1. Create a Chocolatey community account and its API key
   (<https://community.chocolatey.org/account>).
2. Store the key as the repository secret:

   ```sh
   gh secret set CHOCOLATEY_API_KEY
   ```

3. Rehearse packaging and the local install without pushing:

   ```sh
   gh workflow run publish-registries.yml -f tag=vX.Y.Z \
     -f chocolatey-dry-run=true -f npm=false -f pypi=false \
     -f crates-io=false -f maven=false
   ```

## Failure recovery

- **Failed before the tag was pushed:** safe to re-dispatch the same bump.
  No tag exists to clean up. The version-bump commit may already be on
  `main`, but the version is recomputed from git tags, so a tagless rerun
  derives the same version.
- **Notarization rejected or signing failed:** the darwin leg — or the
  windows leg, once the Windows credentials exist — fails, so no
  artifact is uploaded and the tag job never ran. Fix the cause — a rejection
  prints the notary log, an expired credential is a
  [rotation](#rotating-apple-credentials) ([Windows](#rotating-windows-credentials)
  too) — then re-dispatch the same bump: no tag exists, so the
  tagless-rerun bullet above applies and the re-dispatch is safe.
- **Failed after the tag was pushed:** do **not** delete or reuse the pushed
  tag. Re-dispatching computes the *next* version forward from the existing
  tag — bump forward and ship that instead.
- **Server image build or push failed, or was skipped:** the release itself
  is unaffected. Re-run the failed job from the release run's page (it keeps
  the run's `main` ref, which the IAM role requires). `publish-image` reuses
  the image artifact, which is kept for 7 days; after that, re-run
  `build-image`, which re-runs `publish-image` too. An offline release
  leaves that version without an image until the next release.
