# 0068. CLI install script distributed as a GitHub Pages static asset

- **Status**: Accepted
- **Date**: 2026-09-19
- **Deciders**: CypherLite maintainers (Linear LABS-25)

## Context

CypherLite’s CLI ships as raw release binaries on GitHub Releases (see
ADR-0067 for the release and versioning policy). Today a user must find the
right asset on the releases page, download it by hand, `chmod +x` it, and place
it on `PATH`. Linear issue **LABS-25** ("CLI install script",
<https://linear.app/neo4j/issue/LABS-25/cli-install-script>) asks for a
one-liner installer in the style of `rustup`/`uv`: a
`curl … | bash` command that detects the platform, downloads the release
binary, verifies it, and installs it to a user-local directory.

The reference pattern is a GitHub Pages-served installer from another
self-updating CLI tool of the same author,
which serves its installer from GitHub Pages. That reference shapes most of the
decision below, with two deliberate deviations: the hosting mechanism (see
Alternatives considered) and the asset shape (this repository ships raw
binaries, not tarballs).

Some facts about the public surface constrain the design:

- The canonical home is `neo4j-labs/cypherlite` (ADR-0067, and the
  fail-closed `release-owner` job in `.github/workflows/release.yml`). Project
  Pages sites are served from `<org>.github.io/<repo>`, so the site lives at
  `https://neo4j-labs.github.io/cypherlite/`. There is **no** `cypherlite`
  GitHub organization, so a bare `https://cypherlite.github.io/…` URL cannot
  exist.
- The Docusaurus docs site (`docs/docusaurus.config.ts`, `baseUrl:
  '/cypherlite/'`) is already deployed to the `gh-pages` branch, as recorded in
  `docs/PUBLISHING.md` (section 4). A repository has exactly one Pages source.
- The release workflow uploads raw, extensionless CLI binaries named
  `cypherlite-<os>-<arch>` (for example `cypherlite-linux-amd64`,
  `cypherlite-darwin-arm64`) plus a `SHA256SUMS` manifest written in macOS
  `shasum` format (`<hex>  <name>`, two spaces).
- While the repository is internal, GitHub API and release-asset downloads
  require authentication. The repository will become public later
  (`docs/PUBLISHING.md`, step 1).

The decision below is already agreed; this ADR records it before it is
implemented, per the repository’s decision-log rule.

## Decision

Distribute the CLI install script as a static file on the existing GitHub Pages
docs site, using the release binaries ADR-0067 already defines.

1. **URL.** The canonical installer URL is
   `https://neo4j-labs.github.io/cypherlite/install.sh`. It is **not**
   `https://cypherlite.github.io/install.sh`: no `cypherlite` GitHub
   organization exists, and project Pages sites are served from
   `<org>.github.io/<repo>`.
2. **Hosting.** The script lives at `docs/static/install.sh`. The Docusaurus
   build copies everything under `docs/static/` to the site root, so the
   existing `gh-pages` deploy publishes it at the URL above. No new CI
   workflow is added and the GitHub Pages source setting is unchanged.
3. **Platforms.** macOS and Linux only, on both `amd64` and `arm64`
   (`cypherlite-linux-amd64`, `cypherlite-linux-arm64`,
   `cypherlite-darwin-amd64`, `cypherlite-darwin-arm64`). Windows stays on the
   manual `.exe` download documented in the README.
4. **Install directory.** The script installs to `$HOME/.local/bin`. It does
   not require `sudo` and does not write outside the user’s home directory.
5. **Assets.** The installer downloads the raw release binary for the detected
   platform directly. Assets are named `cypherlite-<os>-<arch>` (no tarballs)
   and are verified against the release’s `SHA256SUMS`, which is in macOS
   `shasum` format (`<hex>  <name>`, two spaces). This is the existing release
   contract; the installer changes nothing in `release.yml`.
6. **Authentication.** While the repository is internal, all GitHub API and
   release-asset requests carry `Authorization: Bearer $GITHUB_TOKEN`. When
   `GITHUB_TOKEN` is unset the script is a no-op auth-wise, so it works
   unchanged once the repository is public and the token is unnecessary.
7. **Invocation.** The documented one-liner is:
   ```
   curl -sSfL https://neo4j-labs.github.io/cypherlite/install.sh | GITHUB_TOKEN=$(gh auth token) bash
   ```
   The environment prefix **must** be on `bash`, not on `curl`. An environment
   prefix on the left of a pipe applies only to that command and never reaches
   the script on the right-hand side through the pipe; attaching it to `bash`
   is what makes `$GITHUB_TOKEN` visible to the installer.

Implementation of `docs/static/install.sh`, the README install section, and the
`gh auth token` guidance is owned by separate changes; this ADR only fixes the
distribution decision.

## Alternatives considered

- **Actions-based GitHub Pages deployment (the same reference tool's
  approach)** — rejected: a repository has exactly one Pages source, and
  `docs/PUBLISHING.md` already plans the `gh-pages` branch produced by the
  Docusaurus deploy. Adopting an Actions-based Pages source would force
  migrating the docs deployment to the same mechanism, coupling an installer
  change to a docs-infrastructure rewrite for no installer benefit.
- **The issue’s URL, `https://cypherlite.github.io/install.sh`** — rejected:
  impossible. There is no `cypherlite` GitHub organization; a GitHub Pages
  project site is served from `<org>.github.io/<repo>`, so the only valid host
  for this repository’s Pages site is `neo4j-labs.github.io/cypherlite`.
- **Shipping tarball assets to match the reference tool** — rejected: it would
  require changing `release.yml`’s asset shapes (currently raw binaries plus
  `SHA256SUMS`) with no installer benefit. The raw binary plus checksum
  manifest is simpler, is already the release contract ADR-0067 fixes, and the
  installer can download the binary directly.
- **A PowerShell installer for Windows** — rejected: there is no reference
  implementation to model it on and it adds an untested surface. The README’s
  manual `.exe` download already covers Windows, so the installer stays
  macOS/Linux-only.

## Consequences

- The installer is reachable at a stable, public, HTTPS URL with no new CI
  workflow and no change to the GitHub Pages source setting; adding it is a
  single static file under `docs/static/`.
- Because the script is published by the docs deploy, it only updates when the
  docs site is redeployed. The script must therefore be written to read the
  latest release at runtime, not to embed a version or asset list.
- `docs/PUBLISHING.md`’s Pages step (section 4) becomes a prerequisite for the
  installer URL resolving. Until Pages is live, the one-liner 404s.
- The installer inherits the docs site’s hosting and availability: if the Pages
  deploy breaks, both the docs and the installer go down together.
- Windows users get no one-liner and continue with the manual `.exe` download,
  an intentional scope limit recorded here so it is not mistaken for an
  oversight.
- The `GITHUB_TOKEN` requirement while the repository is internal is a
  temporary public-URL wrinkle: the documented one-liner includes
  `GITHUB_TOKEN=$(gh auth token)`, which becomes unnecessary (but harmless)
  once the repository is public.
- The release asset contract is now load-bearing for two consumers — the
  README’s manual download table and this installer. Changing binary names,
  the `SHA256SUMS` format, or the release workflow’s asset shapes requires a
  new ADR referencing this one and ADR-0067.
- Choosing a different hosting mechanism, URL, install directory, platform
  scope, asset shape, or auth model requires a new ADR referencing this one.

## Later references

ADR-0115 adds a release-signing step to this asset contract: the release
publishes an OpenSSH SSHSIG signature `SHA256SUMS.sig` over `SHA256SUMS`
(Ed25519, namespace `cypherlite-release`), and `install.sh` verifies it with
stock `ssh-keygen` against a pinned public key **before** the checksum step,
failing closed when the signature is missing or invalid. The installer's
hosting, URL, install directory, and platform scope are unchanged.

ADR-0571 changes the resolution mechanism behind the consequence that the
script must read the latest release at runtime: the release tag now comes
from the website's `releases.json` (ADR-0250) instead of the GitHub API's
latest-release endpoint, because the extension channel publishes full
releases into the same repository (ADR-0431) and that endpoint names
whichever release was published last. The hosting, URL, install directory,
platform scope, and the asset-request auth model of this decision are
unchanged.

ADR-0585 supersedes §6's authentication model for the installer: the
repository is public, `install.sh` downloads every asset from the direct
release route unauthenticated, reads no `GITHUB_TOKEN`, and consults no
GitHub API endpoint; §7's token-prefixed invocation is historical.

ADR-0586 lifts §3's macOS/Linux-only platform scope for Windows: a
PowerShell installer, `install.ps1`, ships beside `install.sh` with the same
resolution, transport, and fail-closed verification chain, and installs to
`%LOCALAPPDATA%\Programs\relate`. `install.sh` itself still installs only on
macOS and Linux; under Git Bash, MSYS2 and Cygwin it now exits naming the
PowerShell one-liner instead of `unsupported OS`.
