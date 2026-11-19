---
type: adr
area: infra
date: 2026-09-28
deciders: CypherLite maintainers
name: 0123-macos-codesign-notarize
status: accepted
tags:
  - release
  - security
title: Codesign and notarize the macOS release binaries with a Developer ID
REFERENCES:
  - "[[adr/0067-release-versioning-and-process]]"
  - "[[adr/0068-cli-install-script-via-github-pages]]"
  - "[[adr/0069-cli-lifecycle-commands]]"
  - "[[adr/0115-sshsig-release-signing]]"
---

# 0123. Codesign and notarize the macOS release binaries with a Developer ID

## Context

The release pipeline (`.github/workflows/release.yml`, per [ADR-0067](./0067-release-versioning-and-process.md)) builds
the `cypherlite` CLI for two macOS targets and publishes the raw binaries
`cypherlite-darwin-amd64` and `cypherlite-darwin-arm64`:

- the `cli` job matrix below builds `x86_64-apple-darwin` on `macos-15-intel`
  and `aarch64-apple-darwin` on `macos-latest`, then a "Rename binary" step
  copies `target/<target>/release/cypherlite` to the artifact name before
  `actions/upload-artifact@v4`;
- the `create-release` job downloads those artifacts, generates `SHA256SUMS`,
 and authenticates it with the SSHSIG release signature fixed by **[ADR-0115](./0115-sshsig-release-signing.md)**
  before publishing.

Both macOS binaries are currently unsigned. Users install the CLI with the
`curl | bash` installer (`install.sh`, asset contract [ADR-0068](./0068-cli-install-script-via-github-pages.md)) and self-upgrade
with the in-binary `upgrade` command (design [ADR-0069](./0069-cli-lifecycle-commands.md)). On modern macOS,
Gatekeeper does not run the quarantine-time malware scan at download time; it
assesses the binary when it is first executed. An unsigned, un-notarized Mach-O
downloaded from the network is blocked with "Apple could not verify
… is free of malware", and the user is pushed through a manual
System Settings bypass. That is the friction tracked as **LABS-128**.

**SSHSIG signing and Developer ID notarization are complementary, not
substitutes.** [ADR-0115](./0115-sshsig-release-signing.md)'s SSHSIG signature over `SHA256SUMS` is a
release-integrity mechanism consumed by CypherLite's own tooling — `install.sh`
and the `upgrade` command check the manifest against a public key pinned in the
source, closing the same-channel substitution gap. macOS Gatekeeper knows
nothing about SSHSIG, the pinned key, or the manifest; it assesses the Mach-O
binary itself against Apple's notarization service. Both are needed: SSHSIG
authenticates the checksum manifest for the CLI's upgrade path and `install.sh`,
while Developer ID codesign + notarization is what Gatekeeper assesses before
it lets the binary run.

Scope is **Gatekeeper only**, per LABS-128: no Mac App Store distribution, no
sandbox entitlements, and no provisioning profiles. Out of scope: Windows
Authenticode, Linux packaging, FFI dylib signing, Python wheel signing, and
`universal2`/`lipo` merges.

The decision below was settled before this ADR was written; this record states
it rather than reopening it.

## Decision

**Codesign each macOS CLI binary with a Developer ID Application identity and
notarize it with Apple's notary service at release time, using one shared
composite action in the two darwin legs of `release.yml`.**

1. **One shared composite action.** A single composite action at
   **`.github/actions/macos-notarize/action.yml`** performs signing and
   notarization. Both darwin legs of the `cli` job invoke it; there is no
   duplicated signing logic per leg.
2. **Per-arch codesign.** For each artifact the action codesigns the binary
   with `codesign` using a **Developer ID Application** identity, with
   **`--force`**, **`--options runtime`** (hardened runtime), and
   **`--timestamp`**. Each architecture is signed separately on its own runner;
   there is no post-hoc `lipo` merge or universal binary.
3. **Zip and notarize.** The action zips the signed binary and submits it with
   **`xcrun notarytool submit --wait`**, so the release fails if the submission
   is rejected rather than publishing an un-notarized binary.
4. **Invocation point.** The action runs in the darwin legs **after the
   "Rename binary" step and before `actions/upload-artifact`**, so only signed,
   notarized bytes ever leave the runner. The `create-release` job still
   checksums and SSHSIG-signs the published assets unchanged.
5. **Credentials come from repository secrets**, with the same names used by
   the sibling `neo4j-cli` repository: `MACOS_SIGN_P12`,
   `MACOS_SIGN_PASSWORD`, `MACOS_NOTARY_KEY_ID`, `MACOS_NOTARY_ISSUER_ID`, and
   `MACOS_NOTARY_KEY`. The action imports the `.p12` into a temporary keychain
   and removes it afterwards.
6. **Online Gatekeeper check only; no stapling.** Notarization is verified by
   Gatekeeper over the network at first launch. The action does not staple a
   notarization ticket.
7. **Fail closed.** If credentials are missing or signing or notarization
   fails, the darwin leg fails and no artifact is uploaded; an unsigned macOS
   binary cannot ship from a green release.

## Alternatives considered

- **Adopt GoReleaser.** A mature, batteries-included release tool with
  built-in macOS codesign and notarization support. Rejected: the release
  pipeline is a hand-rolled Rust workflow — `release.yml` — and GoReleaser
  would replace the whole version-bump/tag/SSHSIG flow for one feature,
 discarding [ADR-0067](./0067-release-versioning-and-process.md) and [ADR-0115](./0115-sshsig-release-signing.md) behavior and rewriting the release
  mechanism to gain a signing step.
- **Ship a stapled `.pkg` for offline Gatekeeper.** A signed, stapled installer
  package would let Gatekeeper validate the ticket without a network round
  trip. Rejected: `stapler` supports only UDIF disk images, code-signed
  executable bundles such as `.app`, and signed flat `.pkg` — not bare
  binaries or zips — so the published raw binary and `SHA256SUMS` contract
 ([ADR-0068](./0068-cli-install-script-via-github-pages.md)) could not carry a stapled ticket, and the installer shape would
  have to change for this one platform.
- **Keep the binaries unsigned.** Ship as today and rely on SSHSIG + checksum
  integrity. Rejected: Gatekeeper blocks unsigned, un-notarized binaries
  downloaded over the network, so `curl | sh` installs are blocked per
  LABS-128 — SSHSIG proves the bytes are ours but does not satisfy Apple's
  launch assessment.

## Consequences

- macOS users can run the released CLI without the System Settings
  Gatekeeper bypass; the `curl | sh` install path is no longer blocked at
  first launch.
- The darwin legs depend on Apple's notary service and on the Developer ID
  certificate being valid; a rejected submission or expired credential fails
  the release. This couples release success to an Apple-side service.
- Five signing/notary secrets become load-bearing release credentials in the
  repository, using the same names as the sibling `neo4j-cli` repository. Their
  custody and rotation belong with the release credentials documented in
 `RELEASING.md` ([ADR-0115](./0115-sshsig-release-signing.md)).
- The published macOS artifact is still the raw binary behind `SHA256SUMS`
 signed per [ADR-0115](./0115-sshsig-release-signing.md); signing and notarization add no new release asset and do
 not change the installer asset contract ([ADR-0068](./0068-cli-install-script-via-github-pages.md)) or the upgrade design
 ([ADR-0069](./0069-cli-lifecycle-commands.md)).
- Because notarization is verified online with no stapled ticket, a user who
  is offline at first launch is not covered by this decision; offline
  verification would require the `.pkg` path rejected above and a new ADR.
- The FFI dylib, Python wheel, and all non-macOS artifacts remain out of scope
  and unsigned; extending any of them requires a new ADR referencing this one.
