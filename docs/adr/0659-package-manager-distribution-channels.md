---
type: adr
area: infra
claim: baa561ad7ab1a246f7ad2b63d6b316a62520f285
date: 2026-11-22
deciders: Relate maintainers
name: 0659-package-manager-distribution-channels
status: proposed
tags:
  - release
  - security
title: The CLI distributes through six package-manager channels and defers upgrades to them
REFERENCES:
  - "[[adr/0069-cli-lifecycle-commands]]"
  - "[[adr/0115-sshsig-release-signing]]"
  - "[[adr/0457-signed-manifest-carries-version]]"
  - "[[adr/0546-release-discovery-via-relate-web]]"
  - "[[adr/0586-windows-powershell-installer]]"
---

# 0659: The CLI distributes through six package-manager channels and defers upgrades to them

## Context

The CLI reaches users as GitHub release assets through `install.sh`,
`install.ps1` and `relate upgrade` ([ADR-0069](./0069-cli-lifecycle-commands.md),
[ADR-0586](./0586-windows-powershell-installer.md)), all of which replace the
binary themselves under the SSHSIG-signed manifest
([ADR-0115](./0115-sshsig-release-signing.md),
[ADR-0457](./0457-signed-manifest-carries-version.md)). A package manager needs
the opposite contract: it owns the binary and its repository, so a
self-replacing CLI fights it, and its package must run on the oldest supported
distribution.

## Decision

**Six package-manager channels:**

1. **Homebrew** for macOS and Linux, through a project tap.
2. **APT** for Debian and Ubuntu, from a signed repository.
3. **RPM** for Fedora, RHEL and openSUSE, from a signed repository.
4. **winget** for Windows, from the release artifact and its published hash.
5. **Scoop** for Windows, from a project bucket.
6. **Chocolatey** for Windows, from the release artifact and its published hash,
   through the Chocolatey community repository.

The unmanaged channels — GitHub release assets, both installers, a manual copy —
keep today's behavior.

**Repository hosting.** The release pipeline generates the APT and RPM
repository trees from the release's own staged assets and attaches them to the
release; relate-web pulls and serves them at the canonical `relatedb.ai` paths
(`/apt`, `/rpm`), as it already serves `releases.json` and the installers
([ADR-0546](./0546-release-discovery-via-relate-web.md)). Those two
repositories have no third-party host and no separate artifact store: the
signed release and its asset manifest (`assets.json`, every asset's sha256 and
size) stay the one source of truth, and their path gains no vendor and no
credential. The registry channels publish through the registry each one
requires — the project tap, `microsoft/winget-pkgs`, the project Scoop
bucket, the Chocolatey community repository — and each package carries only
the release artifact's URL and the manifest's sha256, so a registry costs a
credential but no artifact store.

**Signing-key policy.** A dedicated OpenPGP key signs repository metadata,
separate from the SSHSIG release key ([ADR-0115](./0115-sshsig-release-signing.md))
and the jar-signing key: one key per signing surface, so a rotation touches one.
APT signs `Release`/`InRelease`; RPM signs `repomd.xml`. The private key is only
a release-workflow secret; the public key and its 40-hex fingerprint are
committed in `distribution/`, and a client trusts exactly that key. Rotation
publishes the new key before any metadata it signs, keeps the old one trusted
for one overlap release, and re-signs. The signature authenticates the index
only: payloads ride the asset manifest's sha256 and, for the binary, the SSHSIG
chain.

**Install provenance.** A package records itself with
`.relate-package-managed`, a regular file beside the canonicalized
`current_exe` — never the PATH symlink, never `/usr/bin` — holding JSON
`{"manager": "<kebab-case id>", "upgrade": "<the command the user runs>"}`.
Presence is what relate reads, not content: a regular file, symlink, directory
or uninspectable entry there all mean package-managed (fail closed), and a
malformed marker still counts, with the refusal naming the path and parse
error. Unknown fields are ignored, so a new manager needs no relate release.
Writing it is the only cooperation a package owes — a convention, not a
security control. An unmanaged install writes none.

**Upgrade deferral.** `relate upgrade` checks provenance before its platform gate
and any network request: on a package-managed install it replaces nothing, runs
no skill refresh, and exits non-zero with one line naming the manager's upgrade
command. `--check` still reports current and latest, advising that command
instead of `relate upgrade`. Both installers refuse to overwrite a
package-managed install with the same message. An unmanaged install upgrades
exactly as today.

## Alternatives considered

- **A third-party package host.** Rejected: it adds a vendor, an account and a
  credential to the release path and splits the audit trail; generating the
  static trees on the release keeps one source of truth.
- **Detect the manager by path prefix, writability or a package-database
  query.** Rejected: prefixes are stringly, miss Linuxbrew and user
  prefixes, and false-positive a manual copy into `/usr/local/bin`; writability
  is wrong for root and user-owned prefixes; a package-database subprocess is
  distro-specific and unusable in a static binary; an environment variable does
  not survive `exec`.
- **Self-replace a package-managed binary.** Rejected: it bypasses the manager's
  file ownership and database, and the next package operation conflicts with it.
- **One signing key for every surface.** Rejected: the repository key and the
  SSHSIG release key differ in format, lifetime and verifier; sharing one
  couples unrelated rotations.

## Consequences

- The release gains assets: the asset manifest and the two signed repository
  trees.
- A package keeps the real binary in a package-owned directory and symlinks it
  onto `PATH`; a recipe that writes the marker elsewhere is non-conforming and
  `relate upgrade` will not see it.
- The binary shipped to APT, RPM and Homebrew on Linux must run on the oldest
  supported distribution — a baseline CPU ISA and a documented glibc floor —
  so its build environment and that floor become part of the channel contract.
- The winget, Scoop and Chocolatey follow-ups ([ADR-0586](./0586-windows-powershell-installer.md))
  now have their prerequisites: a hash-carrying manifest and a provenance
  marker.
- `relate upgrade` is now platform- and provenance-dependent; its help text and
  `docs/api/cli/spec.md` must say so.
