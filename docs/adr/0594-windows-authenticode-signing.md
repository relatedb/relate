---
type: adr
area: infra
claim: 96e3165fe8152d7f944ebce7950cd0623508d858
date: 2026-11-13
name: 0594-windows-authenticode-signing
status: proposed
tags:
  - release
  - security
title: Sign the Windows CLI binary with Authenticode
REFERENCES:
  - "[[adr/0067-release-versioning-and-process]]"
  - "[[adr/0068-cli-install-script-via-github-pages]]"
  - "[[adr/0069-cli-lifecycle-commands]]"
  - "[[adr/0115-sshsig-release-signing]]"
  - "[[adr/0123-macos-codesign-notarize]]"
---

# 0594. Sign the Windows CLI binary with Authenticode

## Context

The release pipeline (`.github/workflows/release.yml`, per [ADR-0067](./0067-release-versioning-and-process.md)) builds
the CLI for `x86_64-pc-windows-msvc` on a Windows runner and publishes the
raw executable `relate-windows-amd64.exe` behind the `SHA256SUMS` manifest
authenticated by [ADR-0115](./0115-sshsig-release-signing.md)'s SSHSIG signature. The darwin legs sign and
notarize their binaries ([ADR-0123](./0123-macos-codesign-notarize.md)); the Windows executable ships unsigned.

Windows consumers consult none of that: SmartScreen prompts "unknown
publisher", enterprise download policy blocks the bare executable, and
SSHSIG authenticates only the checksum manifest — the complementarity
[ADR-0123](./0123-macos-codesign-notarize.md) records for Gatekeeper: the platform's own check assesses the
binary, so the binary must carry the platform's signature.

One constraint shapes the mechanism: the organization's code-signing
keypairs are non-exportable and held by the certificate authority's key
management (DigiCert Software Trust Manager) — no export can be produced,
so the signing design must drive that remote key management instead.

The decision below was settled by the maintainers before this ADR was
written; this record states it rather than reopening it.

## Decision

**Sign `relate-windows-amd64.exe` at release time with an Authenticode
signature carrying an RFC 3161 timestamp, produced remotely by DigiCert
Software Trust Manager through DigiCert's official signing action, using
one shared composite action in the windows leg of `release.yml`.**

1. **One shared composite action.** A single composite action at
   `.github/actions/windows-sign/action.yml` performs the signing,
   invoked by the windows leg of the `cli` job after the "Rename binary"
   step and before the artifact upload — the invocation-point contract
   of [ADR-0123](./0123-macos-codesign-notarize.md) — so only signed bytes leave the runner once the
   credentials exist. Scope is the CLI executable only; the FFI DLL,
   wheels, and every other artifact stay unsigned.
2. **Remote signing through DigiCert's official action.** The sign step
   runs DigiCert's public signing action
   (`digicert/code-signing-software-trust-action`) in simple-signing
   mode, pinned by full commit SHA with the release it names — never a
   floating tag, the rule every third-party action in the release
   workflows follows. The action drives only the `smctl` client, which
   authenticates a DigiCert ONE API user from the `SM_*` step env and
   signs remotely: the private key never leaves DigiCert's key
   management, and no code-signing certificate material is decoded to
   disk anywhere in the action.
3. **Identity: alias selects, fingerprint verifies.** The action selects
   the keypair by its alias — its only keypair input — and the expected
   certificate is pinned by its SHA-1 fingerprint, a repository variable
   the runner-side verification enforces: a renewal under the same alias
   is an explicit fingerprint change, not a silent publisher switch.
4. **RFC 3161 timestamp, SHA-256 digest.** Timestamping stays enabled,
   so the signature remains valid after the certificate expires.
5. **Verify on the runner.** The action verifies the signed executable
   with `Get-AuthenticodeSignature`: the status must be `Valid`, a
   time-stamper certificate must be present, and the signer thumbprint
   must equal the pinned fingerprint (uppercase hex, no spaces); a
   failed verification fails the leg before any upload.
6. **Absent credentials: skip loudly, bytes unchanged.** While the API
   key is unset the action warns and exits 0, the binary untouched — a
   deliberate asymmetry with [ADR-0123](./0123-macos-codesign-notarize.md)'s fail-closed rule, so the
   pipeline runs before the credentials exist.
7. **Credentials present: fail closed.** Once the API key is set, a
   missing companion credential, any smctl failure — including an
   unauthorized signer — or a failed verification fails the leg.
8. **Secret hygiene.** The client-authentication certificate — the one
   credential smctl needs as a file — is decoded from a base64 secret
   by the caller and deleted in an `always()` cleanup; the action never
   writes certificate material to disk, and no credential is echoed.

## Alternatives considered

- **A local certificate bundle in repository secrets — the exportable
  `.pfx` design this ADR first recorded** — rejected: the keypairs are
  non-exportable and held by the certificate authority's key management,
  so no exportable bundle exists to store; remote signing also keeps the
  signing key off the runner and out of the secret store entirely.
- **A hand-rolled composite action installing and driving `smctl`
  itself, the client's version and SHA-256 pinned as constants** —
  rejected: DigiCert maintains an official action for exactly this
  workflow, the surface that tracks the service's client tooling. The
  honest trade-off: the official action downloads its client from
  DigiCert's CDN against a same-origin checksum — weaker than an
  independent pin — compensated by the pinned action ref and the
  runner-side thumbprint verification.
- **Traditional signtool signing through smctl (KSP library +
  certificate-store sync)** — rejected: it puts the KSP library and a
  synced certificate on the build machine for no capability this use
  needs; simple signing signs the same file types with the client alone.
- **Fail closed on the absent credential, as [ADR-0123](./0123-macos-codesign-notarize.md) does** — rejected:
  that rule landed alongside existing credentials; gating every release
  on credentials not acquired yet would block releases until they exist,
  and the skip path makes the gap visible instead.
- **A different managed signing service (e.g. Azure Trusted Signing)** —
  rejected: the organization's code-signing certificates are issued and
  held by DigiCert's key management; a second provider would mean
  re-issuance and a second credential system for one platform's leg.
- **Sign every Windows artifact (FFI DLL, installers)** — rejected: scope
  was settled as the CLI executable only; extending it requires a new
  ADR referencing this one.
- **Keep the executable unsigned** — rejected: SmartScreen and enterprise
  policy assess the binary, not the manifest, so SSHSIG cannot satisfy
  them — the same argument [ADR-0123](./0123-macos-codesign-notarize.md) records for Gatekeeper.

## Consequences

- While the DigiCert credentials are unset, releases ship an unsigned
  Windows executable — a stated, visible state (a workflow warning on
  every release) that ends the moment they are set.
- Once they exist, the leg is fail-closed: a missing companion
  credential, an unauthorized signer, an expired certificate, or a
  failed verification blocks the release before tagging.
- Three secrets and three variables join the load-bearing release
  credentials (API key, client certificate and its password; service
  host, keypair alias, certificate fingerprint); custody, setup, and
  rotation are documented in `RELEASING.md`, mirroring [ADR-0115](./0115-sshsig-release-signing.md)/[ADR-0123](./0123-macos-codesign-notarize.md).
- The runner holds no code-signing key material: the private key stays
  in DigiCert's key management; the one local credential file is
  created and deleted within the job.
- The windows leg now depends on DigiCert's service being reachable and
  the API user being an authorized signer — the same class of external
  coupling [ADR-0123](./0123-macos-codesign-notarize.md) records for Apple's notary service. The
  action stays on its pinned commit until the SHA is bumped explicitly.
- The timestamped signature outlives the certificate's validity window;
  a revoked certificate still invalidates signatures made with it, and
  renewal is a service-side change pinned by the fingerprint variable.
- The published artifact remains the raw executable behind the
  `SHA256SUMS`/SSHSIG contract ([ADR-0115](./0115-sshsig-release-signing.md)): no new release asset, and no
  change to the installer contract ([ADR-0068](./0068-cli-install-script-via-github-pages.md)) or `upgrade` ([ADR-0069](./0069-cli-lifecycle-commands.md)).
