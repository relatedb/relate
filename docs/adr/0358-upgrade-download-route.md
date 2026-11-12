# ADR-0358: Probe-then-fallback upgrade download route for the internal repository

Status: Accepted
Claim: e3e101e00c62f812e33185f7052d3a629881913d

## Context

`cypherlite upgrade` / `cypherlite update` (ADR-0069) discovers the latest
release from the website's static `releases.json` (ADR-0250) and downloads
three assets: the manifest `SHA256SUMS`, its signature `SHA256SUMS.sig`, and
the platform binary `cypherlite-<os>-<arch>`.

ADR-0250 §3 chose the non-API download route
`https://github.com/neo4j-labs/cypherlite/releases/download/{tag}/{asset}`
and §5 assumed that an optional `Authorization: Bearer $GITHUB_TOKEN` header
makes that route work "while the repository is internal". That assumption is
false. The repository's visibility is `internal`, and GitHub's browser
download route does not honor tokens for internal repositories: it answers
404 for every request, with or without a token. Measured on 2026-10-18
against the `v0.12.5` release:

- `curl -H "Authorization: Bearer $(gh auth token)" \
  https://github.com/neo4j-labs/cypherlite/releases/download/v0.12.5/SHA256SUMS`
  → HTTP 404 — identical with the `token` scheme and with no auth at all.
- `curl -H "Authorization: Bearer $(gh auth token)" \
  -H "Accept: application/octet-stream" \
  https://api.github.com/repos/neo4j-labs/cypherlite/releases/assets/654066833`
  → HTTP 200, 453 bytes.

`distribution/install.sh` already documents this exact limitation in its
`download_asset` comment: "The browser download URL does not honor tokens
for internal repos (it 404s), and the API endpoint also works unauthenticated
on public repos, so this single endpoint serves both cases." That is why
`install.sh` installs fine while `cypherlite update` fails.

The user-visible failure: `cypherlite update` 0.12.4 → 0.12.5 aborts with

```
Error: upgrade: GET https://github.com/neo4j-labs/cypherlite/releases/download/v0.12.5/SHA256SUMS: http status: 404
```

with and without `GITHUB_TOKEN` set. Every shipped binary still carries the
direct-only code path, so no installed 0.12.4/0.12.5 can self-update past
this failure (the remedy is in Consequences).

Integrity is transport-independent: the manifest is authenticated by the
OpenSSH SSHSIG over the exact `SHA256SUMS` bytes against the key pinned in
the binary (ADR-0115), and the binary is checksum-verified against the
authenticated manifest. Where the bytes came from is not part of the trust
chain, so adding a second transport does not weaken verification.

Two existing properties make the fallback safe to add:

- The CLI already reads `GITHUB_TOKEN` (parity with `install.sh`, ADR-0068)
  and sends it as a bearer header only on asset downloads; it is never
  printed and never sent to the website origin.
- `ureq` 3.4's redirect policy never forwards the `Authorization` header
  across a redirect (`RedirectAuthHeaders::Never` is the default), so the
  token cannot leak to the `objects.githubusercontent.com` asset CDN when
  the API asset endpoint redirects.

## Decision

`upgrade` keeps the direct browser download route as its first and preferred
route and falls back to the authenticated GitHub API asset route only when
the direct route answers 404. The route policy:

1. **The direct route is tried first.** `upgrade` fetches `SHA256SUMS` from
   `https://github.com/neo4j-labs/cypherlite/releases/download/{tag}/{asset}`
   before anything else. That one manifest request is the probe; there is no
   separate probe request.
2. **A 2xx probe serves the whole upgrade.** The signature and the binary
   then come from the same direct route and no GitHub API request is made at
   all. This is the tokenless path for the public future and stays API-free.
3. **A 404 on the probe, with `GITHUB_TOKEN` set, falls back to the
   authenticated API asset route for all three assets.** Asset ids are
   resolved with
   `GET https://api.github.com/repos/neo4j-labs/cypherlite/releases/tags/{tag}`
   (consuming only `assets[].name` and `assets[].id`, and failing closed when
   one of the three required names is absent), then each asset is downloaded
   from
   `GET https://api.github.com/repos/neo4j-labs/cypherlite/releases/assets/{id}`
   with `Accept: application/octet-stream` and the bearer token.
4. **A 404 on the probe without a token aborts** with a single
   `Error: upgrade: …` line naming `GITHUB_TOKEN` and the documented
   reinstall one-liner (exit 1, nothing downloaded, nothing modified) — the
   same actionable shape `install.sh` prints when its release lookup fails.
5. **Any other probe failure aborts exactly as today — no fallback.** 403,
   5xx, network and read failures are not a route signal; only the typed
   HTTP 404 status triggers the fallback, never string matching on an error
   message.
6. **The `Authorization` header is sent only on `api.github.com` requests**
   (and on the direct request when a token is set); it is never forwarded to
   the asset CDN on a redirect.
7. **The ADR-0115 verification chain is unchanged** — SSHSIG over the exact
   manifest bytes against the pinned key, then the checksum over the binary —
   and runs before the binary is trusted, whichever route delivered the
   bytes.
8. **Two base-URL test seams join the existing `CYPHERLITE_RELEASES_JSON_URL`**:
   one overriding the direct-route base (default
   `https://github.com/neo4j-labs/cypherlite/releases/download`) and one
   overriding the GitHub API base (default `https://api.github.com`). Both
   are documented dev/test seams; an empty value means the default.
9. **No publisher change.** `releases.json` keeps its schema (no asset ids)
   and no backfill publish is needed: the CLI resolves asset ids at upgrade
   time from the release tag it already has.

## Alternatives considered

- **Always the API asset route** (what `install.sh` does) — rejected: on the
  future public repository every tokenless upgrade would spend about four
  anonymous `api.github.com` requests, re-exposing the shared-IP 60/hour
  limit that ADR-0250 (LABS-316) removed.
- **Token presence selects the route** (token → API, no token → direct) —
  rejected: a token holder on the public repository would be routed off the
  direct route for no benefit, and the two populations would exercise
  different code paths; one cheap probe keeps a single primary path for
  everyone.
- **Carry asset ids in `releases.json`** (an additive schema key) —
  rejected: it needs a publisher change plus a backfill publish of the
  current release, to save one authenticated API request per upgrade.
- **Make the repository public** — rejected: not a code change, and the
  update path must work while the repository is internal.
- **Shell out to `gh release download`** — rejected: an external binary
  dependency the CLI does not otherwise need (the same reason ADR-0250
  rejected `git ls-remote --tags`).
- **Host the release assets on the website** — rejected: GitHub Pages is not
  a release-asset mirror, and the binaries must not become public while the
  repository is internal.

## Consequences

- `cypherlite update` works against the internal repository with the same
  `GITHUB_TOKEN` `install.sh` already needs; without it the failure is
  actionable — it names the variable and the reinstall command — rather than
  a bare 404.
- The tokenless direct route keeps working unchanged once the repository is
  public, with zero API requests; the steady state ADR-0250 chose is
  preserved.
- While the repository is internal, each upgrade pays one extra request (the
  probe that 404s) before the API route; the manifest is small, so the cost
  is one round trip.
- ADR-0250 §3 (the download URL template as the only route), §4 (no GitHub
  API fallback, "not even when `GITHUB_TOKEN` is set") and §5 (the token's
  role as an optional direct-download header) are superseded for the
  download route; its metadata discovery, schema and publisher contract
  stand unchanged. A **Later references** note is added to ADR-0250 per the
  decision-log rule.
- The CLI gains a small GitHub API response parse surface (asset names and
  ids only), which fails closed when a required asset name is missing.
- Users on 0.12.4/0.12.5 cannot self-update to the fixed release — their
  binaries still use the direct route and know no fallback — so the one-time
  remedy is the documented `install.sh` reinstall; subsequent updates work.

## Later references

ADR-0585 takes this route policy to the installer: `install.sh` now
downloads every asset from the direct route and consults no GitHub API
endpoint at all, so the "always the API asset route (what `install.sh`
does)" alternative above describes the pre-0585 installer. The
authenticated fallback arm recorded here remains `upgrade`'s alone.
