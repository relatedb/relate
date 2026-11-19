---
type: adr
area: cli
claim: ae2bab2dd5107b1926d6051bed12f020b08c6db5
name: 0431-cli-extension-distribution
status: proposed
tags:
  - extensions
  - release
title: Extension distribution — the first-party channel, the install verbs, and trust tiers
REFERENCES:
  - "[[adr/0068-cli-install-script-via-github-pages]]"
  - "[[adr/0069-cli-lifecycle-commands]]"
  - "[[adr/0079-anonymous-usage-metrics]]"
  - "[[adr/0080-cli-pull-command]]"
  - "[[adr/0103-engine-surfaces-testing-layout]]"
  - "[[adr/0104-cargo-feature-bundles]]"
  - "[[adr/0115-sshsig-release-signing]]"
  - "[[adr/0250-releases-json-update-discovery]]"
  - "[[adr/0261-upgrade-skill-refresh-ownership]]"
  - "[[adr/0279-lab-command-namespace]]"
  - "[[adr/0358-upgrade-download-route]]"
  - "[[adr/0368-one-product-binary-serve-absorbs-bins]]"
  - "[[adr/0385-http-gzip-response-compression]]"
  - "[[adr/0429-cli-extension-registry]]"
  - "[[adr/0430-cli-extension-wire-protocol]]"
  - "[[adr/0449-github-install-trust-pins]]"
---

# ADR-0431: Extension distribution — the first-party channel, the install verbs, and trust tiers

## Context

[ADR-0429](./0429-cli-extension-registry.md) and [ADR-0430](./0430-cli-extension-wire-protocol.md) fix the host side of command-kind extensions: one
extension is one package directory under
`$XDG_CONFIG_HOME/cypherlite/extensions/<name>/` ([ADR-0429](./0429-cli-extension-registry.md) §2) that mounts
as `cypherlite run <name> [flags]` and speaks wire v1 to a host that owns
the store ([ADR-0430](./0430-cli-extension-wire-protocol.md)). The host ships with zero installed extensions and
every unknown name fails closed with the discovery hint naming first-party
availability ([ADR-0429](./0429-cli-extension-registry.md) §7). Nothing decided yet can put a package on the
machine: who publishes one, how the CLI verifies it, which verbs manage
it, and what the user is trusting at each tier. That distribution half is
this ADR (LABS-469).

Every primitive it needs is already in production:

- `cypherlite upgrade` reads the static `releases.json` from the website
 ([ADR-0250](./0250-releases-json-update-discovery.md)), downloads through the probe-then-fallback route ([ADR-0358](./0358-upgrade-download-route.md)),
  and authenticates the `SHA256SUMS` manifest against the one Ed25519
 SSHSIG key pinned in the binary ([ADR-0115](./0115-sshsig-release-signing.md)) before the checksum step —
  ureq + sha2 + ssh-key, already the CLI's optional dependencies.
- `pull --from FILE` ([ADR-0080](./0080-cli-pull-command.md) §3) is the local-file-instead-of-network
  precedent.
- `skill install` / `skill remove` ([ADR-0261](./0261-upgrade-skill-refresh-ownership.md)) is the marker-owned
  install/remove precedent: write what you own, refuse what you don't.
- The P0 seam (#502, LABS-467) holds the three `lab` actions in one
  static table — the extraction seam LABS-552 builds on.

The UX decisions that bind this ADR were settled in LABS-552
(2026-10-27): the lifecycle verb is `extension` (`install | list |
remove`); the total new reserved words are `run` + `extension`; the
installer seeds nothing — capabilities are install-on-demand from the
signed channel, the Postgres/PGlite model taken to its conclusion: one
engine binary, capabilities as signed published packages; and the
importer extraction is mandatory. LABS-469 (this card) owns the
distribution mechanics and adapts to those decisions without restating
them.

The 2026-10-28 user direction completes the retirement half of those
decisions: the hidden `neo` dies with the `lab` surface — deleted
entirely, never an extension — and the retirement itself is a clean
break, no stub, no compat (§11, §12).

The walkthrough the program exists to make true: on a fresh default
install, `run git-to-graph` fails closed with the install hint;
`extension install git-to-graph` installs it from the signed channel;
`run git-to-graph DIR -d store` imports and REPL-handoffs.

## Decision

### 1. First-party is the release channel and its one signing root

A package is first-party because of where it is published, nothing else:
this repository's release channel is the first-party channel, and the
distribution work adds no second trust root, no second key, no second
verification path. Extension artifacts are release assets beside the
platform binary:

- **The channel index** — `extensions.json` at the website root beside
  `releases.json`, written only by the release pipeline, never
 hand-edited ([ADR-0250](./0250-releases-json-update-discovery.md)'s rules carry over). One row per package: the
  `name`, its published versions, and for each version its `protocol`
  and per-platform assets (the release tag, the asset filename, the
  SHA256).
- **The packages** — one tarball per platform per version (§2),
  published as release assets on the tag the index names.
- **Authentication** — the index is signed by the one release signing
 key ([ADR-0115](./0115-sshsig-release-signing.md): Ed25519 SSHSIG, namespace `cypherlite-release`, public
  key pinned in the binary, never fetched), and each tarball verifies
  by its SHA256 against the signed index. This is the upgrade chain
  generalized: the index is the checksum manifest, the tarball the
  payload, the verification identical in shape to `SHA256SUMS` →
  binary.
- **Transport** — asset downloads ride the same route machinery
 `upgrade` already runs: the [ADR-0358](./0358-upgrade-download-route.md) probe-then-fallback with its
  `GITHUB_TOKEN` advice, and os/arch selection as the installer already
  does; `releases_json.rs`'s asset selection generalizes to extension
  rows (LABS-469).

Platform scope matches the release channel's ([ADR-0068](./0068-cli-install-script-via-github-pages.md)): macOS and
Linux. A host platform with no matching row refuses with an authored
error naming the platform and the package.

### 2. The artifact and the closed unpack contract

The distribution artifact is a gzip'd tarball holding exactly the
package directory: `extension.json` and the executable, regular files
only. The member contract is closed and enforced by the unpacker: no
symlinks, no hardlinks, no device nodes, no paths that escape the
package directory, no members the manifest does not account for — each
is a loud refusal ([ADR-0429](./0429-cli-extension-registry.md) §2's escaping-executable rule extended to
every member), never a skipped entry. The executable lands with its
exec bit from the member's mode field.

Decompression is `flate2` with the pure-Rust `miniz_oxide` backend —
already a workspace dependency ([ADR-0385](./0385-http-gzip-response-compression.md)) — and the ustar reader is a
small in-tree reader for this closed contract. No new external
dependency lands (the `tar` crate is rejected below); R6 is not
triggered.

### 3. The verbs

```text
cypherlite extension install <name> [--url URL] [--from DIR] [--version V] [--force]
cypherlite extension list [--available]
cypherlite extension remove <name>
```

- `extension` is a first-positional command word in the hand-rolled
 parser (the [ADR-0069](./0069-cli-lifecycle-commands.md) shape), reserved in `COMMAND_WORDS` in every
 build ([ADR-0104](./0104-cargo-feature-bundles.md)): a build without the feature prints the shared
  not-included error (exit 2), and `./extension` / `-- extension`
 keep the path reachable — exactly `run`'s reservation ([ADR-0429](./0429-cli-extension-registry.md) §1).
- The three actions are exit-after lifecycle actions in the
  resource-action shape: exactly one positional (the name) on
  `install` and `remove`, none on `list`; action names are closed.
- `--url` and `--from` are mutually exclusive (a usage error naming
  both) and each names the install source; with neither, `install`
  means the first-party channel. `--version V` selects a listed channel
  version (default: the newest listed) and is a usage error beside
  `--url` / `--from` — the source the flag names already determines
  the version.
- `--force` clobbers a foreign target directory — one without the
  install marker — on `install` only, the `skill install` refusal
 ([ADR-0261](./0261-upgrade-skill-refresh-ownership.md)): default install refuses to touch what it does not own,
  exit 1, pinned text. A marked install of the same name replaces
  cleanly (reinstall or version change; there is no `extension
  upgrade` verb). `remove` carries no `--force`: a foreign or unknown
  directory is a refusal naming what it found.
- Feature gating ([ADR-0104](./0104-cargo-feature-bundles.md), [ADR-0429](./0429-cli-extension-registry.md) §8): the `extensions` feature
  owns the registry, the `run` mount, `list` and `remove` —
  dependency-free. A second feature, `extension-install`, adds the
  install verbs and pulls the network/unpack code (`dep:ureq`,
  `dep:sha2`, `dep:ssh-key`, `dep:flate2` — the upgrade feature's own
  optional set plus the unpack reader). `full` and `agent` carry both;
  `minimal` carries neither; a build with `extensions` but without
  `extension-install` keeps `list` and `remove` and prints the
  not-included error for `install` — network dependencies gate the
  install verbs only (LABS-469).
- Telemetry: command `extension`, action the action word
  (`install` / `list` / `remove` — `&'static str`s, never a package
 name, [ADR-0079](./0079-anonymous-usage-metrics.md) anonymity), the `skill` command's pattern.
- Exit codes follow [ADR-0430](./0430-cli-extension-wire-protocol.md) §5's split: flag problems and manifest
  problems (a reserved-word package name, an unknown kind, a protocol
  the host does not speak) exit 2; name-resolution, environment,
  network, verification and unpack failures exit 1. Every refusal has
  pinned, tested text (LABS-469's acceptance rule).

### 4. Install: verify, unpack, check — never a silent fetch

The default path (no `--url`, no `--from`), in order:

1. Fetch the channel index and its signature; verify the SSHSIG
   against the pinned key — fail closed on a missing or invalid
 signature, no signature-optional path ([ADR-0115](./0115-sshsig-release-signing.md) §6).
2. Resolve the row: the name (an unknown name is an authored error
   naming the package and the channel), the version (`--version` or
   the newest listed — an unlisted `--version` is an authored refusal
   naming the listed ones), and the host platform's asset row.
3. Download the tarball through the route machinery; verify its
   SHA256 against the signed row.
4. Unpack under the closed contract (§2) into a staged sibling
   directory, then rename into place — the `skill install` staging
   shape (`.cypherlite-stage-*` → rename): a failed install never
   leaves a half-written package.
5. Load the manifest and run every [ADR-0429](./0429-cli-extension-registry.md) load refusal — unknown
   kind, unknown field, bad flag kind, an escaping executable path —
   plus the protocol check (§8) and the name-collision checks (§7),
   at install time: an install that would fail at load fails here,
   with the same pinned texts. Then write the provenance marker (§7).

The network is touched only by this verb (and `list --available`, §9).
Discovery, `run`, and every failure on an uninstalled name never fetch
— [ADR-0429](./0429-cli-extension-registry.md) §7 stands unchanged: the `no installed action '<name>'`
error is offline knowledge.

### 5. `--from DIR`: the offline and CI story

`--from DIR` points at an unpacked package directory —
`extension.json` plus the executable, exactly what a channel install
would unpack into the XDG dir. No network, no signature: the local
directory is the trust act, the posture `pull --from FILE` already
pinned ([ADR-0080](./0080-cli-pull-command.md) §3) — whoever provisions the directory owns what is
in it, exactly as whoever provisions a credentials file owns the
credentials. The executable must *resolve* inside the directory it is
named in, not merely be spelled inside it: an intermediate-directory
symlink (`bin` pointing elsewhere, say) refuses at canonicalize +
containment, so out-of-directory content never rides in through a
lexically-inside name.

The manifest, protocol, collision and staging checks of §4 step 5 run
unchanged; the package is copied into place (the source directory is
left untouched) with its provenance recorded as the local path. CI
installs fixture packages this way (the install-from-dir e2e in
LABS-469's acceptance); air-gapped machines and sandbox images carry
packages across the gap as directories — an installed extension is
just a directory, so an image can `COPY` one in and install from it.

### 6. `--url URL`: the explicit third-party trust act

`--url` names the package tarball at any https source the invoker
chooses — https only, an authored refusal for anything else. No
signature exists to check — no root covers third-party publishers —
so the trust act is the invocation itself: the user naming where the
code comes from, over the one transport that protects it. The install
fetches, unpacks under the same closed member contract, runs the same
manifest, protocol and collision checks, records the URL as
provenance, and prints one line naming the package, its version, and
the URL it came from. `extension list` shows every installed package's
tier, so a third-party install is never mistaken for a first-party
one.

### 7. Trust tiers, provenance, and the no-shadow refusals

Three tiers, one ladder:

- **First-party** — the signed channel, automatic verification (§1).
- **Third-party URL** — the explicit act, loud output, recorded
  provenance (§6).
- **Local directory** — the explicit local act (§5).

(P2's callable packs, LABS-468, will sit at the data tier below these
— the lightest ceremony — recorded here so the ladder is one table.)

Every install writes a provenance marker beside the manifest (the
`skill install` marker, [ADR-0261](./0261-upgrade-skill-refresh-ownership.md)) recording the tier and the source
reference — the channel version, the URL, or the directory path.
`list` reads it; `remove` requires it.

Third-party can never shadow a reserved word or a first-party name:
install refuses, before anything is unpacked into place, a package
whose name collides with a reserved word (`COMMAND_WORDS` — locally
knowable, dependency-free) or with an installed extension of any
provenance; both refusals name both sides. Since every install records
provenance and every collision refuses, a first-party package can never
be replaced by a third-party one, or the reverse, without an explicit
`remove`: there is no silent substitution path. [ADR-0429](./0429-cli-extension-registry.md) §6's
load-time collision refusals remain the backstop for a hand-edited
registry.

### 8. Compatibility: the protocol version is the only coupling

The extension never touches the store ([ADR-0430](./0430-cli-extension-wire-protocol.md) §6 — the child receives
neither a store path nor raw argv), so the host and a package are
coupled by the manifest's `protocol` ([ADR-0429](./0429-cli-extension-registry.md) §4) and nothing else:
not the CLI version, not the release tag, not a compatibility matrix.
First-party packages release on their own cadence — a package release
bumps only its index row, and the CLI release need not move — and the
engine (storage formats, WAL, checkpoints) revs freely under an old
extension while `protocol: 1` holds.

Enforced at install: the manifest's `protocol` must be one this host
speaks, else an authored refusal naming both numbers (the manifest's
value and the host's) — the fail-closed-no-negotiation rule [ADR-0430](./0430-cli-extension-wire-protocol.md)
§2 pins for the `hello` echo, caught one boundary earlier. The index's
`protocol` column lets the refusal fire before the tarball download.

### 9. The catalog: `extension list --available`

`extension list` (no flag) prints the installed set — name, version,
tier, source — from the local registry and markers: offline,
dependency-free, never a fetch. `extension list --available` answers
the other question (LABS-552's open item, decided here): what exists
to install. It fetches the channel index and its signature (verified
as §4 step 1) and prints one row per package: the name, its newest
version, its `protocol`, and whether it is installed. Hidden packages
stay hidden in no listing ([ADR-0429](./0429-cli-extension-registry.md) §2's hidden policy) —
`--available` does not advertise what the `run` listing would not.

Offline, `--available` fails closed: an authored, pinned, exit-1 error
naming the index URL and the network cause — never an empty list,
never a silent fallback to installed-only. It needs the network, so
it rides the `extension-install` feature.

### 10. No pre-seed, install-on-demand

The installer and the release binary seed nothing: a fresh install has
zero extensions and every `run <name>` fails closed with the discovery
hint. Capabilities arrive only through the explicit install verb;
nothing auto-installs, suggests-and-fetches, or background-syncs. The
Docker image is unchanged by this program — `cypherlite` and
`entrypoint` only ([ADR-0368](./0368-one-product-binary-serve-absorbs-bins.md)): image authors who want capabilities baked
in layer them themselves (a package directory plus `install --from`,
or the XDG dir), and everyone else installs on demand.

### 11. The importer extraction is mandatory

`fs-to-graph` and `git-to-graph` leave the CLI crate into first-party
extension crates speaking protocol v1 (LABS-552): production dogfooding
of P1, the channel's first real content, and the end of the
compiled-in importer surface. The hidden `neo` is not extracted: the
egg is deleted entirely, in the same change — no extension crate, no
channel row, no remnant of the action in the CLI (the 2026-10-28 user
direction; the "extract `neo` too" option is rejected below). The
extraction is not an optional flip: two homes for the same importers
is parallel state at feature scale, and a channel with zero
first-party content proves nothing about the distribution story —
the LABS-469 "keep the importers compiled-in" option is rejected
below. The extension crate sources are workspace crates placed under
the [ADR-0103](./0103-engine-surfaces-testing-layout.md) group and boundary rules; where the three groups do not
fit them, the extraction change records the placement decision.

### 12. The `lab` retirement: a clean break

The change that extracts the importers retires `lab` outright — the
2026-10-28 user direction: no stub, no compat. The word leaves
`COMMAND_WORDS`, the `lab` feature, the command code, the help page,
and every doc row all go in that one change, and nothing recognizes
`lab` afterward. A bare `cypherlite lab` falls into the path-first
form and fails as a Cypher syntax error; `cypherlite lab <action> …`
fails as the unexpected-argument usage error — the standard failures
of a word the CLI does not know, the same treatment any never-existing
word gets. LABS-552 §5's dispatch concern (the fall-through above) is
answered by accepting it, not by a stub: the retirement is recorded in
the release notes and this ADR, not carried in the binary.

[ADR-0279](./0279-lab-command-namespace.md)'s Later references note (its namespace superseded by `run`)
lands with the extraction change, when the supersession happens;
[ADR-0368](./0368-one-product-binary-serve-absorbs-bins.md)'s note — the one-binary posture gains a multi-artifact
distribution dimension — lands in this same change, because the
distribution decision is made here.

## Alternatives considered

- **A second signing key or a separate extension registry.** Rejected:
  a second root means a second key ceremony, a second pinned key to
  rotate in lockstep with the first, and a second verification path to
  test — for no trust gain: "first-party" needs no stronger definition
  than "published by this repo's release channel", and the one root
  already authenticates exactly this class of artifact (release
  assets).
- **Keep the importers compiled-in and publish the channel for
  third-party packages only.** Rejected: the channel would open with
  zero first-party content — the distribution story unverifiable and
  the walkthrough (fresh install → hint → install → run) false; and
  two homes for the same importers is the parallel-state shape at
  feature scale. LABS-552 settled the extraction as mandatory.
- **Extract the hidden `neo` into a first-party extension crate with
 the importers.** Rejected: the egg is a lab easter egg ([ADR-0279](./0279-lab-command-namespace.md)),
  not a capability — extraction would mint a signed, supported,
  first-party package out of a joke and put the channel on the hook
  for publishing it. The 2026-10-28 user direction settles it: `neo`
  is deleted with the `lab` surface, entirely.
- **A one-release `lab` stub in `COMMAND_WORDS` printing retirement
  guidance.** Rejected: the 2026-10-28 user direction is a clean
  break — no stub, no compat. A stub is compat state that needs its
  own removal one release later (a second change to retire one
  experimental word), and the guidance it would print duplicates the
  discovery hint `run` already fails closed with; the fall-through
  failures (§12) are the standard unknown-word treatment.
- **Pre-seed the first-party importers into the installer, image, or
  release bundle.** Rejected: pre-seeded packages put code on disk
  nobody asked for, make the channel's verification optional instead
  of the default path, and re-create a compiled-in importer surface
  under a new name — the Postgres/PGlite model this program cites,
  taken to its conclusion, is install-on-demand (LABS-552 §2).
- **`--sha256` checksum pinning for third-party installs.** Rejected:
  a checksum the user transcribes pins integrity, not identity — it
  says nothing about who published the package, rots at every upstream
  release, and trains users to paste values they cannot verify. The
  explicit URL is the trust act; provenance and the loud install line
  carry the record (LABS-469's "at minimum a loud confirmation naming
  what is being installed").
- **A y/n confirmation prompt on `--url` installs.** Rejected: the
  primary audience is agents and CI, where a prompt is a hang; and
  typing the URL is already an unambiguous consent act — a prompt
  re-asks what the flag already answered. The one prompt the CLI
  carries today (`upgrade`'s post-upgrade skill refresh) guards a
  side effect the user did not name; `--url` names the thing itself.
- **Per-publisher channel indexes (a mini-registry behind every
  `--url`).** Rejected: an index per publisher re-creates discovery,
  listing, and key-pinning for sources that have no signing root; one
  tarball at one URL is the smallest contract a third-party publisher
  can actually host.
- **Reusing the per-release `SHA256SUMS` as the extension catalog.**
  Rejected: it can only describe assets that shipped with that CLI
  release — exactly the lockstep coupling §8 removes; extensions
  release on their own cadence, so the catalog is its own signed
  artifact.
- **The `tar` crate for unpacking.** Rejected: a general reader
  accepts the members the contract forbids (symlinks, hardlinks,
  devices, sparse, pax extensions) — the refusal list is the security
  property — and it adds an external dependency for a contract an
  in-tree ustar reader of a hundred-odd lines enforces better (the
 no-new-dependency posture of [ADR-0430](./0430-cli-extension-wire-protocol.md); R6's spirit).
- **An `extension upgrade` verb.** Rejected: install over a marked
  package already replaces it cleanly; a second verb for the same
  write is a second code path for one policy.
- **`--from` as a channel snapshot (index plus signature in a
  directory).** Rejected: two shapes behind one flag is the
  shape-sniffing the code-shape rules forbid, and the pull precedent
  pins `--from` as local input replacing the network input — for
  install, the package directory. The signed path stays
  pointable-at-mirrors through the documented URL seams (the
 [ADR-0250](./0250-releases-json-update-discovery.md) §1 / [ADR-0358](./0358-upgrade-download-route.md) §8 pattern), so no second shape is needed.

## Consequences

- The release workflow grows the index publisher (the
  `publish-releases-json` pattern) and N platforms × M extension
  tarballs as release assets; a committed `extensions.json` fixture
  joins the `releases.json` fixture for parser tests.
- The install e2e runs through fixtures: install-from-dir → `run` →
  `remove` through the full lifecycle offline, plus signature-failure
  and tampered-tarball refusals (LABS-469's acceptance).
- Every failure path has a pinned, tested text: signature failure,
  checksum mismatch, a contract-violating tarball member, unknown
  package, unlisted version, missing platform row, protocol mismatch,
  reserved-word name, collision with an installed name, foreign
  directory, offline catalog.
- The index and download URLs get the same documented env seams the
 upgrade path has ([ADR-0250](./0250-releases-json-update-discovery.md) §1, [ADR-0358](./0358-upgrade-download-route.md) §8), so e2e drives the
  channel from a loopback fixture and the signature checks stay
  pure-function tests with fixture keys (the `TEST_SIGNER` pattern).
- Docs rule §2 lands with the implementation change:
  `docs/api/extensions/` gains the distribution section (channels,
  tiers, verbs, the offline story), `docs/api/cli/spec.md` the
  `extension` rows and flag table, the skill-bundle reference and
  SKILL.md table the new surface, and the AGENTS.md command list and
  `docs/reference/cli-flags.md` the sweep LABS-552 lists.
- The extraction change also deletes the hidden `neo` action — code,
  tests, help section — and retires `lab` outright with it: the
  `COMMAND_WORDS` row, the `lab` feature, the command code and help
  page, and every doc row, all in that one change (§11, §12).
- [ADR-0368](./0368-one-product-binary-serve-absorbs-bins.md)'s one-binary posture gains the multi-artifact distribution
 dimension — the note lands in this same change; [ADR-0279](./0279-lab-command-namespace.md)'s
  namespace-supersession note lands with the extraction change.
- An Oplane threat model runs on the implementation diff (download +
  verify + unpack paths) before merge, per LABS-466's rules.

## Later references

- [ADR-0449](./0449-github-install-trust-pins.md) adds a fourth trust tier, `github-pin`: a GitHub release
  installed with `--github OWNER/REPO@TAG|@SHA`, its tag-to-commit and
  platform-asset SHA256 recorded in a per-user trust store and refused
  when the pin moves. §7's ladder gains the rung; the three tiers above
  and their refusals stand unchanged.
