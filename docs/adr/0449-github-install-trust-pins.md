---
type: adr
area: cli
claim: 16d55dac8c2cd90e0726575c5f86b9c9ee3bca18
name: 0449-github-install-trust-pins
status: proposed
tags:
  - extensions
  - security
title: GitHub installs pinned to a commit
REFERENCES:
  - "[[adr/0068-cli-install-script-via-github-pages]]"
  - "[[adr/0358-upgrade-download-route]]"
  - "[[adr/0429-cli-extension-registry]]"
  - "[[adr/0431-cli-extension-distribution]]"
---

# ADR-0449: GitHub installs pinned to a commit

## Context

[ADR-0431](./0431-cli-extension-distribution.md) distributes extensions through three tiers: the signed first-party
channel (§1, §4), `--url` for a third-party tarball (§6), and `--from` for
a local directory (§5). The first tier is authenticated; the other two are
explicit acts whose only record is the provenance marker beside the package.

The gap is a package published as a GitHub release. The signed channel
only covers this repository, and `--url` names one tarball — no release
assets, no platform selection, no way to say "the version I approved".
What a user of a third-party repository actually needs is the commit they
installed: "tag `v1.2.3` was commit `abc…` and the darwin-arm64 asset was
SHA256 `def…`; if the tag moves, refuse". A URL alone is a moving target
(any re-upload at the same URL is a new binary), and a tag alone is a
moving target too (tags can be moved and re-pointed).

The machinery already exists: the probe-then-fallback download route
([ADR-0358](./0358-upgrade-download-route.md)) serves release assets for any repository slug, the channel's
platform vocabulary picks the host's asset ([ADR-0068](./0068-cli-install-script-via-github-pages.md)), and GitHub's REST
API resolves a tag to its commit and records each asset's SHA256 digest.
What is missing is the decision to record that resolution and enforce it.

## Decision

### 1. A fourth tier: `github-pin`

```text
cypherlite extension install <name> --github OWNER/REPO@TAG
cypherlite extension install <name> --github OWNER/REPO@<FULL-COMMIT-SHA>
```

`--github` installs the package `<name>` from a release of `OWNER/REPO`.
It is mutually exclusive with `--url`, `--from` and `--version`; the source
flags each name the install source ([ADR-0431](./0431-cli-extension-distribution.md) §3). An untagged `OWNER/REPO`
is a **usage error** — a repository without a pin is a moving target — and
so is any value that is not `owner/repo@ref`. A 40-hex ref is read as the
commit pin; any other ref is a tag.

The tier's label is `github-pin`; the provenance marker grows the tier
value and, only for this tier, a `pin` object (`repo`, `tag`, `commit`,
`assetSha256`). Existing two-field markers decode unchanged.

### 2. Resolution: the tag's commit and the asset's SHA256

Before any package bytes are fetched, the install resolves the pin over
GitHub's REST API (anonymous, or with `GITHUB_TOKEN` when set):

- **The commit**: `GET /repos/{repo}/commits/{ref}` — annotated and
  lightweight tags both answer the commit they point at.
- **The release**: `GET /repos/{repo}/releases/tags/{tag}`. The package
  asset is the one whose name matches the channel's convention
  `<name>-<version>-<os>-<arch>.tar.gz` for the requested package and the
  host platform; zero or several candidates refuse. Its SHA256 is
  GitHub's own recorded `assets[].digest` (`sha256:<hex>`); an asset
  without a digest refuses rather than installing unverified bytes.
- **A commit pin** scans the newest 100 releases for the tag that targets
  the commit; none or several matches refuse, naming what was found and
  pointing at the tag pin.

The download then rides the existing probe-then-fallback route
([ADR-0358](./0358-upgrade-download-route.md)): `https://github.com/{repo}/releases/download/{tag}/{asset}`
first, the authenticated API asset route after a typed 404 with
`GITHUB_TOKEN`. The bytes must match the resolved SHA256 before anything
unpacks; the closed member contract and every load refusal of [ADR-0431](./0431-cli-extension-distribution.md) §2,
§4 step 5 apply unchanged.

### 3. The per-user trust store

The pin lives in two places: the package's provenance marker (the record
of how this copy arrived) and a per-user trust store at
`config_dir/extensions-trust.json` — the config-dir rule [ADR-0429](./0429-cli-extension-registry.md) §6
already shares (`$XDG_CONFIG_HOME/cypherlite/`, falling back to
`~/.config/cypherlite/`), never a hardcoded path. The file is a flat JSON
object keyed by extension name, each entry
`{repo, tag, commit, assetSha256}`; writes are staged beside the target
and renamed into place.

The first install records the pin: **the invocation is the approval**.
The store is read before any package fetch and it outlives `remove` — a
`remove` + reinstall is still compared against the recorded pin, so
deleting the package directory cannot re-open the first-install path.

### 4. Same pin proceeds, a moved pin refuses

A reinstall whose resolved pin equals the recorded one proceeds silently
(same commit, same bytes). Any difference — the same tag resolving to a
different commit, a changed asset SHA256, or another repository — refuses
with exit 1 **before any fetch**, printing the recorded pin beside the
resolved one. Nothing is downloaded and nothing lands.

A different pin is accepted only by an explicit re-approval:
`--re-approve` accepts whatever the pin resolves to now, and
`--approve <FULL-COMMIT-SHA>` accepts only a resolution whose commit is
exactly that SHA. Either way the new asset must verify against its
resolved SHA256 first; only after a verified, load-checked package is the
recorded pin updated. `--re-approve` and `--approve` are mutually
exclusive, and both require `--github`.

### 5. Inspecting and clearing are offline, dependency-free

`extension list --trust` prints every recorded pin (name, repo, tag,
commit, asset SHA256) from the trust store, and `extension remove <name>
--forget-pin` clears one. A plain `remove` keeps the pin. Both ride the
`extensions` feature — the store is plain JSON — so they stay available in
a build without `extension-install`, and neither ever fetches.

### 6. Documentation and tests land in the same change

`docs/api/extensions/spec.md` documents the four tiers and the trust
model, `docs/api/cli/spec.md` the flags and rows, and the skill-bundle
reference the install path. The resolution, the pin comparison, the
pre-fetch drift refusal, and the re-approval are each pinned by tests over
loopback GitHub-shaped fixtures: a tag that moves between fetches, an
asset whose digest changes, a clean reinstall, a remove + reinstall, a
tampered download, and the commit pin's release scan.

## Alternatives considered

- **`--url` plus a `--sha256` flag.** Rejected: a checksum transcribed by
  the user pins integrity, not identity — it cannot tell which commit a
  tag named, rots at every release, and re-creates the "paste a hash you
 cannot verify" ritual [ADR-0431](./0431-cli-extension-distribution.md) already rejected. The resolution does
  the hashing.
- **Trusting the tag alone (no commit resolution).** Rejected: tags are
  movable refs. The moving-target failure is exactly what the pin exists
  to catch; comparing only tags would let a re-pointed tag install new
  bytes under an old name.
- **Trusting the downloaded bytes' hash only (record on first download,
  compare on later ones).** Rejected without the commit: a re-upload of
  the same tag under new bytes would still need the commit to explain
  what moved, and the API's digest is the publisher-visible value the
  refusal can name. The commit and the digest are compared together.
- **A trust store inside the package directory.** Rejected: `remove`
  deletes the package, so the pin would vanish with it — the exact bypass
  the store exists to prevent. The store belongs to the user, not to the
  installed copy.
- **Prompts (y/n) on a moved pin.** Rejected for the same reason [ADR-0431](./0431-cli-extension-distribution.md)
  rejected confirmations: the audience is agents and CI, where a prompt
  hangs. An explicit flag is a consent the invocation carries.
- **An `extension trust` sub-action.** Rejected: `list --trust` and
  `remove --forget-pin` are the two facts already at hand (what is
  recorded, which name to forget); a third verb for them doubles the
  action vocabulary without adding a decision.
- **Deriving the platform asset from a `SHA256SUMS` asset.** Rejected:
  GitHub records a digest per asset, so a second file convention and a
  second parse are unnecessary; a publisher whose assets lack digests is
  told to re-upload through the API/UI rather than handed a fallback.
- **Pinning only the commit and letting the asset bytes float.** Rejected:
  a release asset can be replaced in place; the digest is what makes
  "same bytes" true, and the acceptance's "same commit, same bytes"
  needs both.

## Consequences

- A fourth tier joins [ADR-0431](./0431-cli-extension-distribution.md) §7's ladder: first-party, github-pin,
  third-party URL, local. The cross-tier no-shadow rule still requires an
  explicit `remove` to move a name between tiers.
- The trust store is new per-user state. It is small, user-readable JSON;
  a malformed store refuses (never an empty fallback), and
  `remove --forget-pin` is the documented reset.
- `GITHUB_TOKEN` is now useful for two more requests (the commit and
  release lookups); anonymous installs work within GitHub's rate limits.
- A commit pin costs one release-list request plus one commit request per
  scanned release; tag pins cost two requests. The bound is documented
  (newest 100 releases).
- The `@ref` grammar is restricted to ASCII slug characters: tags with
  `/` or whitespace are not expressible. Such a release can still be
  installed through `--url` (unsigned) or `--from` (local).
