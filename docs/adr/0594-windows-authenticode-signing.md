# ADR-0594: Sign the Windows CLI binary with Authenticode

Status: Proposed
Claim: 96e3165fe8152d7f944ebce7950cd0623508d858

## Context

The release pipeline (`.github/workflows/release.yml`, per ADR-0067) builds
the CLI for `x86_64-pc-windows-msvc` on a Windows runner and publishes the
raw executable `relate-windows-amd64.exe` behind the `SHA256SUMS` manifest
authenticated by ADR-0115's SSHSIG signature. The darwin legs sign and
notarize their binaries (ADR-0123); the Windows executable ships unsigned.

Windows consumers consult none of that. An unsigned executable triggers
the "unknown publisher" prompt in Microsoft Defender SmartScreen, is
blocked outright by common enterprise download policy, and shows no
publisher in file properties. SSHSIG authenticates the checksum manifest
for `install.sh` and `relate upgrade` — the same complementarity ADR-0123
records for Gatekeeper: the platform's own check assesses the binary
itself, so the binary must carry the platform's own signature.

One constraint shapes the mechanism: the organization's code-signing
keypairs are non-exportable and held by the certificate authority's key
management (DigiCert Software Trust Manager). No local certificate bundle
exists to place in a repository secret, and no export can be produced;
the signing design must drive that remote key management instead.

The decision below was settled by the maintainers before this ADR was
written; this record states it rather than reopening it.

## Decision

**Sign `relate-windows-amd64.exe` at release time with an Authenticode
signature carrying an RFC 3161 timestamp, produced remotely by DigiCert
Software Trust Manager through its `smctl` client, using one shared
composite action in the windows leg of `release.yml`.**

1. **One shared composite action.** A single composite action at
   `.github/actions/windows-sign/action.yml` performs the signing. The
   windows leg of the `cli` job invokes it after the "Rename binary" step
   and before `actions/upload-artifact` — the same invocation-point
   contract as ADR-0123 — so only signed bytes leave the runner once the
   credentials exist. Scope is the CLI executable only; the FFI DLL,
   wheels, and every other artifact stay unsigned.
2. **Remote signing through smctl.** The action installs the `smctl`
   client (from DigiCert's public client-tool repository, which serves
   only its latest build) with the client's version and SHA-256 pinned
   as constants in the action source, verified against the pinned hash —
   never a checksum fetched from the same origin — before anything from
   the origin executes or any `SM_*` credential is read. The action then
   authenticates a DigiCert ONE API user — API key, client certificate
   and client-certificate password as step env, the `SM_*` contract
   smctl reads (`smctl` and its simple-signing mode are documented in
   DigiCert's public Software Trust Manager documentation,
   <https://docs.digicert.com/en/software-trust-manager.html>) — and
   signs with smctl's simple signing: no third-party signing tool, no
   DigiCert KSP library, no Windows certificate-store sync. The private
   key never leaves DigiCert's key management; no code-signing
   certificate material is decoded to disk anywhere in the action.
3. **Identity: alias and fingerprint.** The keypair is selected by its
   alias and its certificate pinned by SHA-1 fingerprint (`smctl sign
   --keypair-alias … --fingerprint …`), held as repository variables, so
   a renewal under the same alias is an explicit fingerprint change
   rather than a silent switch of the publisher.
4. **RFC 3161 timestamp, SHA-256 digest.** Timestamping stays enabled in
   the sign call, so the signature remains valid after the certificate
   expires.
5. **Verify on the runner.** The action verifies the signed executable
   with `Get-AuthenticodeSignature`: the status must be `Valid` and a
   time-stamper certificate must be present. A failed verification fails
   the leg before any upload.
6. **Absent credentials: skip loudly, bytes unchanged.** While the API
   key is unset the action prints a prominent warning and exits 0,
   leaving the binary untouched and the release green. This is a
   deliberate asymmetry with ADR-0123's fail-closed rule: the skip path
   exists so the pipeline can run before the credentials are acquired,
   and it is loud — a workflow warning on every release — not silent.
7. **Credentials present: fail closed.** Once the API key is set, a
   missing companion credential, any smctl failure — including a user
   not authorized as a signer on the keypair — or a failed verification
   fails the leg; no unsigned Windows binary ships from a green run.
8. **Secret hygiene.** The API user's client-authentication certificate
   is the one credential smctl needs as a file: the caller decodes it
   from a base64 secret into the runner temp dir (DigiCert's documented
   CI pattern) and deletes it in an `always()` cleanup; the action never
   writes certificate material to disk, and no credential is ever
   echoed.

## Alternatives considered

- **A local certificate bundle in repository secrets — the exportable
  `.pfx` design this ADR first recorded** — rejected: the organization's
  code-signing keypairs are non-exportable and held by the certificate
  authority's key management, so no exportable bundle exists to store;
  remote signing also keeps the signing key off the runner and out of
  the repository's secret store entirely.
- **Traditional signtool signing through smctl (KSP library +
  certificate-store sync)** — rejected: it puts the DigiCert KSP library
  and a synced certificate on the runner — extra client components and
  certificate material on the build machine — for no capability this use
  needs; smctl's simple signing signs the same file types remotely with
  the client alone.
- **Fail closed on the absent credential, as ADR-0123 does** — rejected:
  that rule landed alongside existing credentials; here gating every
  release on credentials that have not been acquired yet would block
  releases until they are, and the skip path makes the gap visible
  instead.
- **A different managed signing service (e.g. Azure Trusted Signing)** —
  rejected: the organization's code-signing certificates are issued and
  held by DigiCert's key management; a second provider would mean
  re-issuance and a second credential system for one platform's leg.
- **Sign every Windows artifact (FFI DLL, installers)** — rejected: scope
  was settled as the CLI executable only; extending it requires a new
  ADR referencing this one.
- **Keep the executable unsigned** — rejected: SmartScreen and enterprise
  policy assess the binary, not the manifest, so SSHSIG cannot satisfy
  them — the same argument ADR-0123 records for Gatekeeper.

## Consequences

- While the DigiCert credentials are unset, releases ship an unsigned
  Windows executable — a stated, visible state (a workflow warning on
  every release) that ends the moment they are set.
- Once the credentials exist, the windows leg is fail-closed: a missing
  companion credential, an unauthorized signer, an expired certificate,
  or a failed verification blocks the release before tagging.
- Three secrets and three variables join the load-bearing release
  credentials (API key, client certificate and its password; service
  host, keypair alias, certificate fingerprint); custody, first-time
  setup, and rotation are documented in `RELEASING.md`, mirroring
  ADR-0115 and ADR-0123.
- The runner holds no code-signing certificate material at all: the
  private key stays in DigiCert's key management, and the one local
  credential file — the API user's client certificate — is created and
  deleted within the job.
- The windows leg now depends on DigiCert's service being reachable and
  the API user being an authorized signer — an external coupling of the
  same class ADR-0123 records for Apple's notary service. A new upstream
  smctl build fails the pinned hash check until the version and SHA-256
  are bumped together (`RELEASING.md`).
- The timestamped signature outlives the certificate's validity window;
  a revoked certificate still invalidates signatures made with it, and
  renewal is a service-side change pinned by updating the fingerprint
  variable.
- The published artifact remains the raw executable behind the
  `SHA256SUMS`/SSHSIG contract (ADR-0115): no new release asset, and no
  change to the installer asset contract (ADR-0068) or the `upgrade`
  design (ADR-0069).
