# ADR-0517: Rename the product from CypherLite to Relate

Status: Proposed

Claim: 1ff4a5f8adcec8953782a216fa1faa90e0a4d542

## Context

CypherLite v0.16.0 is the last release cycle under the current name: the
product renames to **Relate**. A full inventory (the rename feature's
inventory pass) classified 2384 files carrying the name into 15
occurrence classes and separated what renames from what must not: a keep
list of immutable records (accepted ADRs, frozen baseline envelopes,
vendored corpora, byte-frozen v0.8.0 golden stores, the CHANGELOG and
dated research notes) and store-format findings that flag every
load-bearing on-disk artifact whose bytes existing stores depend on.

The surfaces the rename must decide, from that inventory:

- 40 workspace crates named `cypherlite-*` across the four top-level
  groups, the product binary `cypherlite`, the env prefix `CYPHERLITE_`
  (including `CYPHERLITE_PREVIEW`, ADR-0371), and the C ABI prefix
  `cypherlite_*` (ADR-0054's one ABI) wrapped by the Go/JS/Java/Swift
  clients.
- Python: PyPI package `neo4j-cypherlite`, import `cypherlite`. npm:
  `@neo4j-labs/cypherlite` plus five platform packages.
- The GitHub repository `neo4j-labs/cypherlite`; the Pages site at
  `https://neo4j-labs.github.io/cypherlite/` (ADR-0068), which also
  hosts `install.sh`.
- The release channel: assets `cypherlite-<os>-<arch>`, the SSHSIG
  signer identity and namespace `cypherlite-release` (ADR-0115), and
  the signed manifest's version header `# cypherlite-release-version: `
  (ADR-0457), pinned byte-identical in `release.yml`, `install.sh`, and
  `upgrade_cmd/manifest.rs`.
- The `.cl` single-file bundle extension and the `MDBCLB01` superblock
  magic (ADR-0127), and the on-disk store artifacts: `.cypherlite.lock`,
  `.cypherlite.writer`, the `CYPHERLITE-LOCK-v1` role bytes, the
  `.cypherlite` markdown sidecar family (ADR-0406), the
  `.cypherlite/schema/` mirror (ADR-0481), the `MDBWAL01`/`MDBWAL02`
  WAL magics, and the `format_version`/`value_encoding` markers.

ADR-0053 is the precedent: the MemoryDB→CypherLite rename changed
crate, binary, and env names while leaving the on-disk WAL magic
untouched. This ADR makes the same split for Relate and settles the
questions ADR-0053 left open (the update channel, the signing identity,
external names).

## Decision

Rename every naming surface to **Relate** in one clean break, and keep
every load-bearing on-disk artifact byte-identical. The canonical map:

| Surface | Old | New |
| --- | --- | --- |
| CamelCase brand | `CypherLite` | `Relate` |
| Binary / release assets | `cypherlite` / `cypherlite-<os>-<arch>` | `relate` / `relate-<os>-<arch>` |
| Workspace crates | `cypherlite-*` (40 crates) | `relate-*` (same suffixes) |
| Engine group directory | `cypherlite/` | `relate/` |
| Env prefix | `CYPHERLITE_*` | `RELATE_*` |
| C ABI prefix | `cypherlite_*` | `relate_*` |
| PyPI package | `neo4j-cypherlite` | `relate-db` |
| Python module | `import cypherlite` | `import relate` |
| npm packages | `@neo4j-labs/cypherlite` + 5 platform packages | `@relate/relate`, `@relate/<platform>-<arch>` × 5, `@relate/relate-wasm` |
| GitHub repository | `neo4j-labs/cypherlite` | `relatedb/relate` |
| Canonical URL base | `https://neo4j-labs.github.io/cypherlite/` | `https://relatedb.ai/` |
| SSHSIG signer identity + namespace | `cypherlite-release` | `relate-release` |
| SUMS version header | `# cypherlite-release-version: ` | `# relate-release-version: ` |
| Bundle extension | `.cl` | `.rl` (`.cl` stays accepted; see 5) |

1. **Clean break.** The new binary carries no `cypherlite` aliases: no
   second command name, no `cypherlite_*` ABI shim, and — departing
   from ADR-0053's `MEMORYDB_*` bridge — no fallback reads of
   `CYPHERLITE_*` env vars. One rename, one release.

2. **Swan-song release.** The old channel ends with **cypherlite
   0.16.1**, published before cutover under the old name, old signer
   identity, and old SUMS header so every existing binary can verify
   and install it. Its release notes and its `releases.json` entry
   (ADR-0250) carry the rename notice and the Relate install one-liner
   (`curl -sSfL https://relatedb.ai/install.sh | bash`); publishing the
   installer there before the swan-song ships is part of the
   external-home bring-up (3).
   After 0.16.1 the `cypherlite` channel publishes nothing further; old
   binaries keep working, but their `upgrade` path ends by design.

3. **External names are separate coordination.** The external home is
   **relatedb**: the public repository is `github.com/relatedb/relate`,
   and `https://relatedb.ai/` is the canonical base the pipeline-owned
   files are served from — `install.sh`, `releases.json` (ADR-0250),
   and `extensions.json` (ADR-0431). npm publishes under the `@relate`
   scope: `@relate/relate` (the pure-JS client), the five platform
   packages `@relate/<platform>-<arch>`, and `@relate/relate-wasm`. The
   PyPI wheel is `relate-db` (the bare `relate` name is taken); the
   import stays `relate`. The CLI is distributed by the bash installer
   at `https://relatedb.ai/install.sh`, not by npm — the npm packages
   are the language libraries only. The website — marketing site and
   cookbook pages — lives in the separate `relatedb/relate-web`
   repository, served at the same canonical base; until that handoff
   lands, this repository's gh-pages pipeline keeps assembling the site
   artifact it assembles today. Acquiring and wiring these homes (the
   repository, the domain, the npm scope, the PyPI name, relate-web)
   happens outside this repository and does not block the in-repo
   sweep; the in-repo change assumes the targets above and re-points
   `repository` URLs, the `release-owner` gate, the channel constants,
   and `docs/PUBLISHING.md` at them. (Amended in place: the original
   proposal pointed the external names at the neo4j-labs organization;
   the external-home sync settled on relatedb before any of them were
   acquired.)

4. **Store compatibility (from the inventory's store-format
   findings).** Every on-disk artifact flagged load-bearing keeps its
   exact name and bytes: `.cypherlite.lock` (ADR-0097),
   `.cypherlite.writer`, the `CYPHERLITE-LOCK-v1` root/wal role bytes,
   the `.cypherlite` markdown sidecar family (ADR-0406 §3), the
   `.cypherlite/schema/` mirror (ADR-0481), the `MDBWAL01`/`MDBWAL02`
   WAL magics, the `format_version` and `value_encoding` markers
   (ADR-0127), and the `MDBCLB01` bundle superblock magic. Stores from
   v0.8.0 through v0.16.x open unchanged under Relate. These are frozen
   compatibility markers, not brand surfaces — ADR-0053 kept
   `MDBWAL01` through the MemoryDB→CypherLite rename for the same
   reason — and the on-disk names carry the old product name
   permanently as the accepted cost. Everything the findings marked
   non-compat renames: the product word in error text (with its
   asserting tests), env vars, crate-path identifiers, and prose.

5. **Single-file bundles.** The store-compat policy extends to bundles:
   `.rl` becomes the bundle extension Relate writes, and `.cl` remains
   accepted as the legacy extension so every v0.16-and-earlier bundle
   (including the byte-frozen `testdata/stores/v0.8.0/graph.cl`
   fixture) keeps opening. The `MDBCLB01` superblock magic and
   `VERSION=1` are unchanged; `is_bundle_path` and the remote URL gate
   admit both extensions.

6. **Release signing and the SUMS header.** The signer identity and
   SSHSIG namespace become `relate-release`, and the signed manifest's
   version header becomes `# relate-release-version: `, changing in
   lock step in `release.yml`, `install.sh`, and
   `upgrade_cmd/manifest.rs` (byte-identical pins). This is the change
   the asset-contract gates in ADR-0068 and ADR-0115 require a new ADR
   for; this ADR is it, and ADR-0067's release policy is otherwise
   unchanged. The Ed25519 keypair itself is kept: the key material
   embeds no name — only the identity, the namespace, and the public
   key file's comment do, and those rename. Old-binary verification is
   unaffected because the swan-song is signed under the old identity
   and nothing published after it needs to verify in old binaries.

7. **The keep list stands.** Accepted ADRs (text and filenames), frozen
   baseline envelopes, vendored TCK/CE corpora, the byte-frozen v0.8.0
   golden stores, `CHANGELOG.md`, and dated research records keep the
   old name. The next baseline freeze cycle regenerates envelopes under
   the new names, and a fresh v0.16.0 golden-store set is generated by
   the renamed binary rather than editing the frozen v0.8.0 fixtures.

## Alternatives considered

- **Keep the name CypherLite.** Rejected: the rename is settled product
  direction; this ADR records how the rename lands, not whether it
  does.
- **A bridge period** (dual binary names, a `cypherlite_*` ABI shim,
  `CYPHERLITE_*` fallback env reads — the ADR-0053 pattern). Rejected:
  ADR-0053's `MEMORYDB_*` bridge kept two names alive in code and docs
  until ADR-0054 hard-cut them; a bridge here doubles the sweep surface
  and guarantees a second breaking change later.
- **Rename the on-disk artifacts too** (a successor `.relate.lock`, new
  WAL or bundle magics). Rejected: it orphans every existing store,
  vault, and golden fixture — ownership identity (ADR-0097) and the
  readable-set gates (ADR-0127) key on those exact bytes.
- **Replace `.cl` with `.rl` outright**, dropping the legacy extension.
  Rejected: every existing single-file bundle ends `.cl` and the
  extension is what selects the bundle VFS; dropping it orphans them.
- **Keep `cypherlite-release` and the old SUMS header.** Rejected: both
  are user-visible brand surfaces verified on every install and
  upgrade; the asset-contract gates in ADR-0068 and ADR-0115 anticipated
  exactly this change and require this ADR, not silence.
- **Rotate the signing keypair along with the identity.** Rejected: the
  Ed25519 key material embeds no name, so rotation buys nothing for the
  rename and adds a key-custody ceremony (RELEASING.md) to the cutover;
  rotation remains available as its own procedure if ever needed.

## Consequences

- One sweep renames the crates and their directories, the binary and
  release assets, env vars, the C ABI and every language client, the
  Python and npm packages, repository URLs, the external home and
  installer URL, CI workflows, docs, the CLI skill bundle, and the
  `product_truth.py` contract — the map above is the canonical
  reference for all of them, and AGENTS.md §2/§3 documentation updates
  ride the same change as the code they document.
- Existing stores, vaults, and `.cl` bundles open unchanged; docs must
  present `.cypherlite.lock`, the `.cypherlite` sidecar, and the `MDB*`
  magics as frozen compatibility markers that deliberately keep the old
  name.
- Pre-cutover releases stay as published; from the first Relate release
  the assets are `relate-<os>-<arch>`, the signature namespace is
  `relate-release`, and the SUMS header is
  `# relate-release-version: `.
- `cypherlite` 0.16.1 is the terminal release of the old channel: old
  binaries keep serving existing stores indefinitely but receive no
  further updates.
- Accepted ADRs, baselines, vendored corpora, frozen golden stores, and
  dated historical records keep the old name, so full-text search for
  `cypherlite` keeps finding the product's history after the rename.
- The engine group directory `cypherlite/` renames to `relate/`, moving
  the 40 crate directories and the workspace member globs in one
  mechanical change; no top-level path carries the old name afterward.

## Later references

- LABS-999 retires the product-truth gate and its
  `docs/product-capabilities.yaml` contract: the sweep no longer bumps the
  contract's `version`, and the checker's file-scoped keep for the
  pre-cutover `neo4j-labs.github.io/cypherlite` URL (§3) goes with the
  file. The repository's own pre-cutover name is unaffected.
- ADR-0623 renames the `.cypherlite` engine sidecar family (§4's
  on-disk keep of it) to `.relate/`, adopting existing stores by one
  directory rename at their first writable open. The lock and writer
  markers and the lock role bytes stay as this ADR kept them.
