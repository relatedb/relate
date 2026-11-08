# ADR-0546: Release discovery files ship as release assets; the website pulls

Status: Proposed
Claim: 56a9b60b7880fcc173599dcc87a0ec4779a11e69

## Context

The website moves out of this repository into a dedicated `relate-web`
repository. Until now the release pipeline committed three pipeline-owned
files — `install.sh`, `releases.json` (ADR-0250), and the signed
`extensions.json` pair (ADR-0431) — into a local `gh-pages` branch, and
`deploy-pages.yml` published that branch. That branch never existed in this
repository after the move, and the multi-writer rebase machinery
(`push-gh-pages.sh`, per-writer concurrency groups, the `MAINTAINING.md`
contract) existed only because three workflows committed into one branch.

The CLI pins the canonical URLs (`https://relatedb.ai/releases.json`,
`/install.sh`, `/extensions.json`); only the transport behind them changes.

## Decision

This repository never pushes content into `relate-web`. The release
pipelines attach the pipeline-owned files to the release they belong to —
`releases.json` and `install.sh` on every core release, the signed
`extensions.json` pair on every extension release — and then fire one
`repository_dispatch` at `relate-web` carrying `{kind: cli|extension, tag}`.
`relate-web`'s publish workflow pulls the files from the release assets and
assembles its Pages artifact from build outputs; it commits nothing.

Signing stays in this repository: the release signing key (ADR-0115) lives
only in this repository's secrets, so the signed extension index is
generated here and rides the release as an asset. The extension-index
publisher skips its own output when scanning a release's assets.

The poke is authenticated by a new `RELATE_WEB_DISPATCH_TOKEN` secret and is
tolerant while `relate-web` is being stood up: an unset secret or a 404 logs
a skip and the release stays green; any other non-204 fails the run. The
manual backfill workflows (`publish-releases-json.yml`,
`publish-extensions-json.yml`) re-attach the files to the relevant release
and re-fire the poke.

The `gh-pages` apparatus is removed: the `docs` and `update-website`
release jobs, `update-website.yml`, `deploy-pages.yml`, and
`push-gh-pages.sh`. The cookbook and WASM playground publishing that
`deploy-pages.yml` assembled becomes `relate-web`'s job.

## Alternatives considered

- **Push the files into `relate-web` as commits.** Rejected: it duplicates
  the multi-writer rebase machinery across repositories, hands a
  cross-repository write token to every release lane, and splits the audit
  trail for pipeline-owned files between two histories.
- **`relate-web` polls releases on a schedule with no poke.** Rejected: the
  site lags every release by up to the poll interval, and the self-update
  smoke's live-site currency gate would skip (or, worse, fail) on a timer
  rather than on an explicit handoff.
- **Keep serving the discovery files from GitHub release asset URLs
  directly.** Rejected: the CLI pins the `relatedb.ai` URLs, release assets
  of a private repository require authentication, and escaping GitHub's API
  rate limits for tokenless clients was ADR-0250's reason for a static
  document in the first place.

## Consequences

- A release is fully published on GitHub without any website machinery;
  the site follows when `relate-web` answers the poke.
- The self-update smoke's live-site currency gate keeps its meaning: while
  the site lags the release list, the canary skips with a stated reason.
- `RELATE_WEB_DISPATCH_TOKEN` (a token with `contents:write` on
  `relate-web`) must exist before pokes stop skipping; until then the
  notice names the manual backfill path.
- ADR-0250's schema and canonical URLs are unchanged; its gh-pages
  publisher contract is superseded by this one. ADR-0330's Pages-deploy
  chain and ADR-0129's website-update chain are superseded in full.
