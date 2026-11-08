# ADR-0250: Release discovery from a static releases.json on the website

Status: Accepted

Later references: ADR-0546 supersedes this ADR's publisher contract
(commits to a local `gh-pages` branch) — releases.json now rides the
release's assets and relate-web pulls it. The schema, the canonical URLs,
and the window rule are unchanged.

## Context

`cypherlite upgrade` (and its `update` alias) discovers the latest release
through `GET https://api.github.com/repos/neo4j-labs/cypherlite/releases/latest`
and downloads assets through the API asset endpoint (ADR-0069 §3-4).
Anonymous REST API requests are limited to 60 per hour per source IP. Users
behind shared NATs, corporate proxies, and CI runners share that IP, so a few
runs exhaust the budget and `upgrade` fails with HTTP 403 until the window
rolls over — a failure unrelated to the user's own usage and unfixable by
retrying (Linear LABS-316). The status-quo workaround, documented since
ADR-0068/0069, is a `GITHUB_TOKEN` hint: it does not help tokenless
environments and puts a secret into a path that only reads public data.

The same problem was solved in `neo4j-cli` (Linear CLI-244) with its option 4:
publish a static `releases.json` to the project website — including release
notes — and have the CLI read it. LABS-316 adopts that option here.

Three properties of this repository make the option cheap:

- The website is a static GitHub Pages site (the `gh-pages` branch, deployed
  by `.github/workflows/deploy-pages.yml` to
  `https://neo4j-labs.github.io/cypherlite/`). A file committed at the branch
  root lands at the site root, and Pages has no anonymous request budget
  comparable to the API's 60/hour.
- The release pipeline already owns release-facing files on `gh-pages`: the
  `docs` job copies `distribution/install.sh` there on every release
  (ADR-0068), and `update-website.yml` keeps the page itself in sync
  (ADR-0129). A `releases.json` publisher follows the same pattern.
- The CLI consumes only `tag_name` and asset names/ids today; it never reads
  release notes and never fetches prereleases. The file can therefore carry
  more than the CLI needs — notes and a recent-releases window — for
  changelog-shaped consumers.

Ordering matters for the notes: `release.yml`'s `release-notes` job runs a
reusable workflow that prepends agent-written narrative notes to the GitHub
release body (ADR-0124, chained explicitly per ADR-0265). A publisher that
reads the release body must run after it.

The integrity posture does not change with the metadata source: the binary is
authenticated by the OpenSSH SSHSIG signature over `SHA256SUMS` against a key
pinned in the binary (ADR-0115), not by where the release list came from.
`releases.json` only selects what to download; it is never trusted for
integrity.

## Decision

Publish `releases.json` at the website root and make `upgrade`/`update` read
it. The GitHub REST API leaves the update path entirely — no fallback, not
even when `GITHUB_TOKEN` is set.

### 1. Canonical URL and hosting

- The canonical URL is
  `https://neo4j-labs.github.io/cypherlite/releases.json`.
- The file lives at the root of the `gh-pages` branch. It is written only by
  the release pipeline and never hand-edited — the same pipeline-owned
  convention as `install.sh` and `wasm/` (`gh-pages/MAINTAINING.md`).
- A new job in `.github/workflows/release.yml` publishes it after
  `release-notes` (so the notes in the file are final) and after
  `create-release` (so the asset list is complete), reading the release list
  with the workflow's `GITHUB_TOKEN`. It commits `releases.json` to
  `gh-pages` with the same checkout/commit/push pattern as the existing
  install.sh sync job. `deploy-pages.yml` copies the branch tree verbatim,
  so no change is needed there.
- The publisher reads the GitHub API; the CLI never does. The API's
  anonymous rate limit is a per-user problem, not a per-workflow one.

### 2. Schema (`schema_version` 1)

The document is UTF-8 JSON with exactly these top-level keys:

| Key | Type | Required | Nullable | Meaning |
| --- | --- | --- | --- | --- |
| `schema_version` | integer | yes | no | Schema revision; `1` as defined here |
| `generated_at` | string (RFC 3339 UTC, `Z`) | yes | no | When the publisher wrote the file |
| `latest` | release object | yes | yes | Newest stable release; `null` iff no stable release exists |
| `latest_prerelease` | release object | yes | yes | Newest published non-draft prerelease, or `null`; the CLI does not consume it (prereleases are out of scope), the slot is reserved |
| `releases` | array of release objects | yes | no | Recent-releases window, newest first; may be empty |

A **release object** has exactly these keys:

| Key | Type | Required | Nullable | Meaning |
| --- | --- | --- | --- | --- |
| `tag` | string | yes | no | Exact git tag including the leading `v`, e.g. `v0.10.0` |
| `published_at` | string (RFC 3339 UTC, `Z`) | yes | no | When the GitHub release was published |
| `notes` | string | yes | no | The release body markdown verbatim (after `release-notes`); the empty string when the release has no body — never `null` |
| `prerelease` | boolean | yes | no | Whether the release is a prerelease |
| `assets` | array of strings | yes | no | Exact GitHub release asset names, e.g. `cypherlite-darwin-arm64`, `SHA256SUMS`, `SHA256SUMS.sig`; may be empty |

**Window rule.** `releases` holds the **10 most recent stable releases**
(published, non-draft, non-prerelease), newest first, including the release
that generated the file; fewer when fewer exist. `latest` equals
`releases[0]` when `releases` is non-empty, and is `null` when it is empty.
Prereleases never appear in `releases` in schema version 1; a prerelease
publish updates `latest_prerelease` and `generated_at` only. The window is
recomputed from the live release list on every publish — the file is a full
snapshot, never an append log — so an older release drops out once ten newer
stable releases exist. The 10-entry cap keeps the file (notes included)
bounded while covering any changelog-shaped use.

**Invariants and compatibility.**

- Every `tag` is non-empty and matches the git tag exactly. For stable
  entries it is `v` followed by an `X.Y.Z` numeric triple; the CLI requires
  that shape for `latest.tag` and treats anything else as a malformed file
  (the existing comparison rules of ADR-0069 §3 are unchanged).
- Consumers ignore every unknown key at every level (forward compatibility).
  Additive changes (new keys, new members) do not bump `schema_version`.
  Removing, renaming, or retyping a key, or changing its nullability, bumps
  it; a consumer that reads a `schema_version` other than the one it
  implements fails closed with an unsupported-schema error.
- The CLI requires the platform binary, `SHA256SUMS`, and `SHA256SUMS.sig`
  to be listed in `latest.assets` and fails closed when one is missing
  (ADR-0115 unchanged). Extra asset names are ignored.

Illustrative document:

```json
{
  "schema_version": 1,
  "generated_at": "2026-10-15T12:00:00Z",
  "latest": {
    "tag": "v0.10.0",
    "published_at": "2026-10-15T11:58:00Z",
    "notes": "### Highlights\n\n- Faster multi-hop aggregates\n",
    "prerelease": false,
    "assets": [
      "cypherlite-darwin-amd64",
      "cypherlite-darwin-arm64",
      "cypherlite-linux-amd64",
      "cypherlite-linux-arm64",
      "cypherlite-windows-amd64.exe",
      "SHA256SUMS",
      "SHA256SUMS.sig"
    ]
  },
  "latest_prerelease": null,
  "releases": [
    {
      "tag": "v0.10.0",
      "published_at": "2026-10-15T11:58:00Z",
      "notes": "### Highlights\n\n- Faster multi-hop aggregates\n",
      "prerelease": false,
      "assets": [
        "cypherlite-darwin-amd64",
        "cypherlite-darwin-arm64",
        "cypherlite-linux-amd64",
        "cypherlite-linux-arm64",
        "cypherlite-windows-amd64.exe",
        "SHA256SUMS",
        "SHA256SUMS.sig"
      ]
    },
    {
      "tag": "v0.9.2",
      "published_at": "2026-10-01T09:00:00Z",
      "notes": "",
      "prerelease": false,
      "assets": ["cypherlite-darwin-arm64", "SHA256SUMS", "SHA256SUMS.sig"]
    }
  ]
}
```

### 3. Asset download URL template

Assets are downloaded through the non-API GitHub route:

```
https://github.com/neo4j-labs/cypherlite/releases/download/{tag}/{asset}
```

`{tag}` is the release object's `tag` (leading `v` included) and `{asset}`
is the exact name from `assets`. For example, the `v0.10.0` darwin-arm64
binary is
`https://github.com/neo4j-labs/cypherlite/releases/download/v0.10.0/cypherlite-darwin-arm64`.
This route is not subject to the `api.github.com` anonymous rate limit and
returns the raw asset bytes directly.

The CLI must confirm the needed names are present in `assets` before
downloading (fail closed), fetch `SHA256SUMS` and `SHA256SUMS.sig` first,
verify the SSHSIG over the exact manifest bytes against the pinned key, and
only then fetch the binary — the ADR-0115 flow, unchanged, including the
`MAX_ASSET_BYTES` cap and the atomic self-replace and skill-refresh steps
of ADR-0069.

### 4. Failure behavior

- **Missing.** Any non-2xx response (including 404) or a network/read failure
  fetching the URL aborts with a single-line `Error: upgrade: ...`
  diagnostic on stderr and exit code 1; nothing is downloaded and nothing on
  disk is modified. The message names the URL and points at the documented
  reinstall one-liner.
- **Malformed.** A body that is not valid JSON, is not a JSON object, is
  missing a required key, has a key of the wrong type or nullability,
  carries a `schema_version` other than 1, or has a `latest.tag` that is not
  `v` plus an `X.Y.Z` triple aborts the same way (single-line error, exit 1)
  before any asset request. `latest: null` (no stable release published) is
  a valid document but has nothing to compare against, so it also aborts
  with a clear message and exit 1.
- **Stale.** When `latest.tag` is not newer than the running binary's
  `CARGO_PKG_VERSION` — the normal state for a development build — the
  command is not an error: it prints
  `cypherlite <current> is up to date (latest release: <latest>)` and exits
  0, exactly as today. `--check` prints the current-vs-latest report and
  exits 0. There is no downgrade path.
- In all three cases there is no retry, no on-disk cache, and no GitHub API
  fallback — not even when `GITHUB_TOKEN` is set. The API path is removed
  from the code, not demoted.

### 5. GITHUB_TOKEN

With the API out of the update path, `upgrade`/`update` no longer uses the
token for metadata and never sends it to the website: the `releases.json`
request goes to a different origin (`neo4j-labs.github.io`) and the file is
public, so attaching a credential would leak it for no benefit.

The token remains read by `upgrade` for one purpose only: an optional
`Authorization: Bearer $GITHUB_TOKEN` header on the direct asset downloads,
parity with `install.sh` while the repository is internal (ADR-0068 §6; the
README already documents it as a download credential). It is never required,
it never gates a fallback, and it becomes unnecessary (but harmless) once
the repository is public. The implementation change updates the environment
rows to match: `docs/api/cli/spec.md`'s `GITHUB_TOKEN` row and
`docs/reference/cli-flags.md`'s sentence change from "release-API requests"
to "release-asset downloads".

### 6. Publisher contract

- The publisher runs on every published release, after `release-notes`, and
  rewrites the whole document as a snapshot: the window comes from the live
  release list, never from the previous file. Re-running for the same tag is
  idempotent apart from `generated_at`.
- It reads `tag`, publish time, body, prerelease flag, and asset names from
  the published GitHub release (`gh release view`/`gh release list` with the
  workflow token) and copies `notes` verbatim from the release body.
- It publishes even for a prerelease (updating `latest_prerelease` and
  `generated_at` only), so the file never lags the release list.
- The file exists from the first release published after the publisher job
  lands. Until then, `upgrade` fails with the missing-file error above — an
  accepted transition cost, removable by backfilling the current latest
  release (manual dispatch of the publisher, or running the same steps by
  hand).

## Alternatives considered

- **Keep the GitHub REST API and hint at `GITHUB_TOKEN` (the CLI-244 status
  quo)** — rejected: it leaves the failure mode untouched. The 60/hour
  anonymous limit is per source IP, so the users LABS-316 is about (shared
  NATs, proxies, CI) are exactly the ones a token hint does not help; and
  the token itself is a secret in a path that reads public data. The API
  shape is also more surface to keep parsing than the CLI needs.
- **The GitHub `releases.atom` feed
  (`https://github.com/neo4j-labs/cypherlite/releases.atom`)** — rejected:
  the feed carries no asset list and no asset URLs, its entry content is
  HTML, and it exposes no reliable prerelease/draft distinction; consuming
  it would require an XML parser dependency to get strictly less than the
  API it replaces, while still leaving the CLI without the per-asset data it
  needs to fail closed.
- **`git ls-remote --tags`** — rejected: it requires a `git` binary on the
  user's PATH, which the CLI does not otherwise need; tags carry no notes,
  no publish time, and no asset list; and remote ref order is not release
  order (annotated versus lightweight tags, drafts, and prereleases are
  invisible), so "latest" would have to be reconstructed from tag names —
  a reimplementation of what the GitHub release record already states.
- **Scraping the GitHub releases HTML page** — rejected: it is not a
  contract. GitHub can change the markup at any time, and the CLI would
  have to parse HTML to recover a tag and an asset list that the JSON
  document states directly.
- **Keeping the API as a `GITHUB_TOKEN`-gated fallback** — rejected: it
  doubles the fetch/parse path and keeps the whole API client and its error
  surface alive for a case the static file covers; it also makes behavior
  depend on whether a secret happens to be present in the environment,
  which is untestable parity, and it still leaves tokenless users exactly
  where LABS-316 found them.
- **Generating `releases.json` from `main` (a committed file or a
  main-push workflow) instead of from the release pipeline** — rejected:
  the notes and the asset list only exist after the release is published,
  so a main-side file is stale between releases and gains a second writer;
  the release pipeline already owns the release-facing `gh-pages` files
  (install.sh, ADR-0068/0129), and the notes ordering requirement ties the
  publisher to the release run.

## Consequences

- `upgrade` no longer depends on `api.github.com`. Users behind shared IPs
  get the same behavior as everyone else, and once the repository is public
  the command needs no credential at all.
- The metadata's freshness is bounded by the release pipeline: a release
  published outside it (a manual `gh release create`) does not update
  `releases.json` until the next pipeline release or a manual publisher run.
- The CLI gains a hard dependency on the website's availability: a Pages
  outage makes `upgrade` fail closed with the missing-file error. The
  installer already shares that hosting coupling (ADR-0068), so the website
  becomes load-bearing for both the first install and the update path.
- `gh-pages` gains a third pipeline-owned file (`install.sh`, `wasm/`,
  `releases.json`); hand-editing it is out of contract, and a manual edit
  is overwritten by the next release.
- The schema is versioned and forward-compatible: additive keys do not bump
  `schema_version`, while a removal/rename/retype or a nullability change
  does and requires a new ADR; old CLIs fail closed on a version they do not
  know rather than misreading the file.
- The release asset contract (ADR-0067, ADR-0068, ADR-0069) gains a fourth
  consumer, the website file; changing asset names or the `SHA256SUMS`
  format now requires updating the publisher in the same change.
- ADR-0069's fetch step (API `releases/latest`, asset ids, the
  octet-stream asset endpoint) and its `GITHUB_TOKEN` consequence are
  superseded for the fetch/download mechanism; its version comparison,
  ADR-0115 verification, atomic replace, and skill-refresh steps are
  unchanged. A **Later references** note is added to ADR-0069 per the
  decision-log rule.

## Later references

ADR-0330 corrects the §1 assumption that `deploy-pages.yml` needs no change.
Nothing deployed Pages after a release, so the committed `releases.json`
went live only with the next push to `main`. At v0.12.4 the URL returned 404
and `upgrade` failed. The release run now calls `deploy-pages.yml` after
`publish-releases-json` and the other `gh-pages` writers. The publisher
pushes through `.github/scripts/push-gh-pages.sh`, which rebases onto a
concurrent `gh-pages` writer instead of failing on a rejected push.

ADR-0358 supersedes §3 (the download URL template as the only route), §4's
no-API-fallback rule ("not even when `GITHUB_TOKEN` is set") and §5's token
claim (that the optional bearer header makes the direct route work while the
repository is internal). The browser download route 404s for internal
repositories regardless of token, so the download route becomes a
probe-then-fallback pair: the direct route first, and on a 404 the
authenticated API asset route for token holders. Metadata discovery, the
schema and the publisher contract stand unchanged.
