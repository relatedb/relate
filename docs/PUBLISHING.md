# Publishing checklist (USER-ACTION-REQUIRED)

> **This is an operational runbook, not an automated step.** Every item below is a
> deliberate, user-driven action (creating secrets, pushing tags,
> standing up the website repository, uploading release assets). Nothing here should be automated by a
> tool or agent. Work top to bottom — later steps assume earlier ones succeeded.

The repository ships with the machinery for a checksum-verified, fail-closed
release already in place (the `release.yml` workflow).
The WASM and Swift surfaces were withdrawn in ADR-0065; WASM returned as npm
packages (ADR-0084) and Swift as a SwiftPM package whose static XCFramework the
release attaches (ADR-0433). The docs site no longer has an interactive
playground. What remains is operational: point
everything at the `relatedb/relate` repository (private for now) and turn it on.

## 0. Preconditions

- [ ] Create the **`release` GitHub environment** (repo Settings →
  Environments) with a deployment branch rule limited to `main` and **no
  required reviewers** (the chained call from `release.yml` must not
  block). Every publish job in `publish-registries.yml` runs in it. This
  rule is **not optional**: the in-code `github.ref == 'refs/heads/main'`
  guard lives in the workflow file itself, so it stops accidents, not an
  adversary — a write-access collaborator can dispatch a branch copy with
  the guard edited out. The environment and its branch rule are
  repository settings, which a branch cannot rewrite. If the plan cannot
  set branch rules on a private repository, do not configure the
  registry publishers below (keep the `&& false` guards) until it can.
  When configuring the registry-side trusted publishers, set their
  **environment field to `release`** as well, so a token minted outside
  the environment is rejected by the registry even if the repository
  rule is ever weakened.

- [ ] `main` is green: `make lint` and `make test` pass; the spec-harness lane (ADR-0482) reports language coverage on the release PR — reporting-only, never a gate.
- [ ] The old repository’s `v*` tags did **not** come across with the move —
  the release workflow seeds the first tag at `v0.1.0`
  (ADR-0067; see `RELEASING.md`). Verify with
  `git ls-remote origin 'refs/tags/v*'` (empty), and never push the old
  tags: they would poison the next-version computation.

## 1. The repository stays private (for now)

The release workflow is **fail-closed to `relatedb/relate`**: the
`release-owner` job (`.github/workflows/release.yml`) refuses to publish
official artifacts from any other repository
(`github.repository != 'relatedb/relate'`). So the canonical home
**must** be `relatedb/relate`.

- [ ] Ensure the repository lives at `github.com/relatedb/relate`.
- [ ] The repository is **public**: release-asset downloads need no
  token — `install.sh` fetches every asset from the direct release
  download route and consults no GitHub API endpoint (ADR-0585);
  `relate upgrade` probes the same route first (ADR-0358). Should the
  repository ever go private again, `upgrade` keeps its authenticated
  fallback; the installer's one-liner would need a new decision.
- [ ] Confirm the `origin` remote used for pushing tags points at
  `relatedb/relate` (a fork's tags cannot publish official artifacts).

## 2. Cut the first release

- [ ] Dispatch the Release workflow (manual dispatch only — it computes the
  version from the existing `v*` tags, builds, tags, and publishes; there are
  no hand-pushed tags, ADR-0067):
  `gh workflow run release.yml -f bump=patch`

## 3. Verify the release workflow and its assets

The workflow builds and uploads, then creates the GitHub Release. Confirm each
artifact landed on that first release:

- [ ] CLI binaries (default) for Linux amd64/arm64, macOS amd64/arm64, Windows amd64.
- [ ] `SHA256SUMS` covering all CLI binaries (the checksum manifest the README
  install section links to), with its first line
  `# relate-release-version: vX.Y.Z` naming the release tag (ADR-0457).
- [ ] `SHA256SUMS.sig` over those exact bytes (ADR-0115).
- [ ] Python wheels (and sdist) publish to PyPI as **`relate-db`** (not
  `relate`, which is taken) via trusted publishing (ADR-0560). The one-time
  setup needs no first publish: on PyPI, add a *pending publisher* for
  project `relate-db`, repository `relatedb/relate`, workflow
  `publish-registries.yml` (the OIDC token names the *top-level*
  workflow — `publish-registries.yml` on a direct dispatch, but
  `release.yml` on the chained release call, where it does not match
  this registration and the publish fails closed; run the publish legs
  by direct dispatch); repeat on TestPyPI for dry runs. Validate with
  `gh workflow run publish-registries.yml -f tag=<tag> -f npm=false
  -f crates-io=false -f pypi-target=testpypi` before the first real
  release.
- [ ] crates.io (ADR-0560): the publishable set is the `relate` facade's
  transitive workspace closure — 17 crates, all lockstep with the release
  tag. The bootstrap claims the names without publishing source: each of
  the 16 unclaimed names gets a manual `0.0.1` placeholder publish
  (empty crate, description, Apache-2.0, a coming-soon README); `relate`
  itself is already owned (the predecessor CLI project, ADR-0560).
  Create a **scoped, short-lived** crates.io API token for the bootstrap
  (Account Settings → API Tokens): scopes `publish-new` and
  `publish-update` only, crate pattern `relate*` (or exactly the 17
  names), expiry of a day — never an unscoped token — then `cargo login`
  and publish the placeholders (crates.io rate-limits new-crate creation
  in bursts; a retry-with-backoff loop rides the 429 windows). The
  first **real** publish is the pipeline's: deferred to the v1.0.0
  release, which publishes the closure from the tag via the `crates-io`
  job — the first registry version is v1.0.0 on every registry.

  Then, per crate on crates.io (Settings → Trusted Publishing): GitHub
  Actions, repository `relatedb/relate`, workflow `publish-registries.yml`
  (direct dispatch only — the chained call's OIDC token names
  `release.yml`, the mismatch the v1.0.0 run exposed). The `crates-io`
  job exchanges the OIDC token for a 30-minute registry token via
  `rust-lang/crates-io-auth-action` (cargo has no OIDC credential
  provider of its own) and fails closed at authentication until all 17
  are configured; its publish loop skips crates whose
  versions are already on the registry, so an interrupted run can be
  re-dispatched to finish. Once the publishers are in, **revoke the
  bootstrap API token** (crates.io → Account Settings → API Tokens, then
  `cargo logout` locally): trusted publishing has taken over, and no
  long-lived token should survive the bootstrap.
- [ ] npm (ADR-0278): the `@relate` npm scope exists and the releaser has
  publish rights.
- [ ] Licensing: the project's licensing decision is resolved and the
  `license` fields of all seven npm packages (`@relate/relate`,
  `@relate/relate-wasm`, and the five platform packages) match the
  decision in force at publish time (Apache-2.0 per ADR-0055 today).
  Re-enabling npm publishing means dropping the `&& false` guards from the
  `npm-platform-wasm` and `npm-main` jobs in `publish-registries.yml` —
  do both in the same change.
- [x] npm: the packages exist on the registry — all seven names were
  bootstrapped with manual `0.0.1` placeholder publishes (npm trusted
  publishing needs a package to exist before it can be configured).
- [ ] Swift (ADR-0433): `RelateFFI.xcframework.zip` is attached to the
  release, and the tagged `Package.swift` carries `releaseTag` = the tag and
  `releaseChecksum` = `swift package compute-checksum` of that zip (the
  `version` job stamps both into the release commit). A smoke: a fresh
  package depending on `.package(url: …, exact: "<version>")` resolves and
  builds.
- [x] npm: trusted publishing is configured on npmjs.com for
  `relatedb/relate`, workflow `publish-registries.yml`, for all seven
  packages (main, wasm, and the five platform packages), with the
  environment `release`. The `&& false` guards on the npm jobs were
  dropped once this was done.
- [ ] Slack (optional): set the `SLACK_WEBHOOK_URL` repository secret to
  enable the release announcement — the release run's final job
  (`slack-announce`) posts it after the website deploy and the self-update
  canary (ADR-0465); without the secret that job logs a skip and the
  release stays green.

  If a publish job dies mid-loop, re-dispatch with that registry
  enabled: the npm-publish action skips tarballs whose versions are
  already on the registry (the same arm the crates.io loop carries), so
  the re-run finishes the remainder.

The release payload is the CLI binaries and their checksums, the Python wheels,
the FFI archives, the npm tarballs, and the Swift XCFramework zip. The
XCFramework is not code-signed yet (no Apple Developer identity in the release
secrets; ADR-0433).

## 4. The website lives in relate-web

There is no `gh-pages` branch in this repository and no Pages deploy here
(ADR-0546). The homepage, the cookbook pages, and the WASM playground are
published by the [`relatedb/relate-web`](https://github.com/relatedb/relate-web)
repository, which pulls what it needs from this repository's releases.

- [ ] Stand up `relatedb/relate-web` with its publish workflow
  (trigger: `repository_dispatch` with `event_type: relate-release`,
  payload `{kind: cli|extension, tag}`; it pulls `releases.json`,
  `install.sh`, `install.ps1`, and the signed `extensions.json` pair from the named
  release's assets and assembles its Pages artifact from build outputs).
- [ ] Create the `RELATE_WEB_DISPATCH_TOKEN` repository secret here (a
  token with `contents:write` on `relate-web`) so the release pipeline's
  poke stops skipping.
- [ ] Point `https://relatedb.ai/` at the relate-web Pages deployment and
  confirm the site loads.
- [ ] Confirm the install scripts are served at
  `https://relatedb.ai/install.sh` and `https://relatedb.ai/install.ps1`, and `releases.json` at
  `https://relatedb.ai/releases.json` (the document `relate upgrade`
  reads).
- [ ] Until relate-web is live, a manual run of the **Publish
  releases.json** workflow here re-attaches the files to the latest
  release and re-fires the poke.

## 5. Confirm the absolute URLs resolve

Several `README.md` and docs links use the public absolute forms
(`https://relatedb.ai/...` and
`https://github.com/relatedb/relate/releases/...`). The
`github.com/relatedb/relate/...` links resolve only once the repository is
public; the `relatedb.ai/...` links resolve once relate-web serves them
(step 4). Spot-check:

- [ ] The README install-table download links (`.../releases/latest/download/...`).
- [ ] The `SHA256SUMS` link and the checksum-verification flow in the README.
- [ ] The install script at
  `https://relatedb.ai/install.sh`.
- [ ] The docs links from the README (Python/Rust library references,
  architecture, and the new [Limitations](limitations.md)
  page).
- [ ] The ~20 absolute `https://github.com/relatedb/relate/blob/main/...`
  links in the docs. Link checkers skip them while the host is not yet
  public, and they are pinned to `blob/main` paths, so a renamed target would
  silently 404. Once the repo is public, open each one (or run a link check
  without the not-yet-public host exclusion) and confirm every target
  resolves.
- [ ] Turn on the cookbook's GitHub links (ADR-0133): delete the not-yet-public
  entries from `lychee.toml`'s `exclude` list on `main` once the repository
  is public, and confirm the cookbook pages' source and companion links
  resolve on the relate-web build. (The navigation's GitHub link is already
  on, ADR-0288.)

Once every box is checked, the first release is fully published:
binaries and checksums downloadable, and the documentation site live.

## Maven Central (Java client, `ai.relatedb:relate`)

Unlike PyPI, npm and crates.io, Maven Central has no trusted publishing:
the `maven-central` job in `publish-registries.yml` signs with a GPG key
and uploads with a Central Portal token, both stored as secrets in the
`release` environment. One-time owner setup, in this order:

1. **Central Portal account.** Sign in at <https://central.sonatype.com>
   (GitHub, Google or email). Use a shared team account, not a personal
   one, so the token outlives any one person.
2. **Claim and verify the namespace.** Portal → Namespaces → Add
   Namespace → `ai.relatedb`. The Portal shows only a verification key.
   Add a DNS record of type `TXT`, host `@` (the domain root of
   `relatedb.ai`), value = the key, without quotes; keep the domain's
   other TXT records. Check it with `dig +short TXT relatedb.ai`, then
   press Verify. This requires control of the domain; keep the record
   afterwards, the Portal may re-check it.
3. **Create a user token.** Portal → Account → Generate User Token. It
   shows a username and a password once; they become
   `MAVEN_CENTRAL_USERNAME` and `MAVEN_CENTRAL_TOKEN`.
4. **Create the signing key.** On a trusted machine:

   ```sh
   gpg --full-generate-key            # RSA 4096, no expiry (or 2 years), a team address
   gpg --list-secret-keys --keyid-format LONG   # note the key ID after rsa4096/
   gpg --keyserver keyserver.ubuntu.com --send-keys <KEYID>   # Central must find the public key
   gpg --armor --export-secret-keys <KEYID> | base64 | tr -d '\n' | pbcopy
   ```

   The clipboard now holds the secret key as one base64 line — that is
   `MAVEN_GPG_KEY`. The key's passphrase is `MAVEN_GPG_PASSPHRASE`. Keep
   an offline backup of the private key and revocation certificate; the
   key cannot be replaced for artifacts already published.
5. **Store the four secrets** (Settings → Environments → `release` →
   Environment secrets): `MAVEN_CENTRAL_USERNAME`, `MAVEN_CENTRAL_TOKEN`,
   `MAVEN_GPG_KEY`, `MAVEN_GPG_PASSPHRASE`.
6. **Publish Maven only**, against an existing release tag:

   ```sh
   gh workflow run publish-registries.yml -f tag=vX.Y.Z \
     -f maven=true -f npm=false -f pypi=false -f crates-io=false
   ```

   The job verifies the five FFI archives' build attestations, stages the
   natives, signs and uploads the fat jar plus the five classifier jars.
   The pom sets `autoPublish=false`: nothing goes live until you open
   Portal → Deployments, check the validation result and press Publish.
   Do that by hand for the first releases. Published versions are
   immutable.

## Between releases

After a release, the workspace version on `main` is the **next
unreleased** one (ADR-0580): the release workflow's `advance` job — the
release bot performing the same version-surface sweep the release bump
performs — commits it to `main`, so the released version exists only on
the tagged commit and on the registries, and every version `main`
carries is unpublished by construction. `cargo package -p relate` on
`main` therefore fails at dependency resolution, by design, until the
next release publishes that version: the verification resolves the
facade's dependencies against crates.io, which does not hold `main`'s
workspace version yet. Between releases that failure is the expected
state, not a packaging regression.

The advance is the release-bot step in `release.yml` that ADR-0580
records — never a manual checklist item in this runbook. A manual step
is skipped under pressure, and nothing detects the omission until the
next release collides with the registry.

A PR that changes workspace crates may need package evidence — proof
that the packaged facade still builds, which `cargo package -p relate`
would otherwise give. Two forms are sanctioned:

- Cite a pre-drift base commit — a commit from before the drift window
  opened, e.g. the release tag, where the workspace sits at the released
  version and `cargo package -p relate` passes against the registry.
- Run `cargo xtask check-package` — it packages the facade, extracts
  the `.crate` tarball, and builds the extracted lib and examples
  against the workspace: the same verification with the workspace
  standing in for the registry (`testing/xtask/README.md`).
