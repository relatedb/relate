---
type: adr
area: infra
date: 2026-09-28
deciders: CypherLite maintainers
name: 0265-release-notes-explicit-chain
status: accepted
tags:
  - release
title: Trigger release notes via an explicit workflow_call chain from release.yml
REFERENCES:
  - "[[adr/0124-release-notes-automation]]"
  - "[[adr/0125-release-notes-amendments]]"
---

# 0265. Trigger release notes via an explicit workflow_call chain from release.yml

- **Renumbered**: from 0127 (shared-number cleanup)

## Context

**[ADR-0124](./0124-release-notes-automation.md)** decided that `.github/workflows/release-notes.yml` runs on
`release: [published]` — the event `release.yml` fires when
`softprops/action-gh-release` publishes the GitHub release. That trigger is
dead: it has never fired and never will in this repository.

The root cause, verified 2026-09-28 against the live GitHub repository
`neo4j-labs/cypherlite`, is token suppression. `release.yml` publishes releases
via `softprops/action-gh-release@v2` with the default `GITHUB_TOKEN`, so the
release author is `github-actions[bot]`. GitHub does not create new workflow
runs for events triggered by `GITHUB_TOKEN` (the exceptions are
`workflow_dispatch` and `repository_dispatch`), so a `release: [published]`
event fired by the release pipeline never triggers downstream workflows.

The evidence is conclusive:

- `.github/workflows/release-notes.yml` has 0 runs ever.
- `.github/workflows/update-website.yml` has the identical
  `release: [published]` trigger and 0 release-triggered runs across v0.7.3,
  v0.8.0, v0.8.1, and v0.9.0 — all four published by `github-actions[bot]`.
  Its 5 runs are all manual `workflow_dispatch`.
- v0.9.0 was published 2026-09-28T12:38:57Z, over an hour after
  `release-notes.yml` landed on `main` (merge commit `cddf813a` at
  11:33:17Z), and still nothing ran — proving token suppression, not file
  absence, is the cause. (v0.8.1 additionally predated the workflow file:
  published 08:55:44Z, workflow merged 11:33Z.)

This also corrects the record in [ADR-0124](./0124-release-notes-automation.md): its Context claims that
`release: published` "is already an established trigger in this repository"
via `update-website.yml`. That claim is factually wrong — the trigger exists
in the file but has never fired. [ADR-0124](./0124-release-notes-automation.md)'s trigger decision is therefore
amended here; its other decisions and the [ADR-0125](./0125-release-notes-amendments.md) amendments are unaffected.

## Decision

**Make `release-notes.yml` a reusable workflow and call it explicitly from a
`release-notes` job in `release.yml`, chained with `needs:` after the release
exists.**

1. **`release-notes.yml` gains a `workflow_call` trigger.** Inputs: `version`
   (required string, the bare `X.Y.Z` version) and `publish` (boolean,
   default `false`). The existing `workflow_dispatch` dry-run trigger is kept
   unchanged. The dead `release: [published]` trigger is dropped.
2. **`release.yml` gains a `release-notes` job.** It has
   `needs: [version, create-release]` and calls
   `./.github/workflows/release-notes.yml` with
   `with: {version: <bare X.Y.Z>, publish: true}` and `secrets: inherit`, so
   notes generation runs explicitly after the release exists — mirroring how
   the inline `docs` job ("Sync installer to gh-pages") is chained with
   `needs:` in the same pipeline.

## Alternatives considered

- **PAT or GitHub App token on `action-gh-release` so `release: published`
  actually fires** — rejected: it introduces a new secret family with its own
  rotation and lifecycle for a mechanism the repository otherwise avoids, and
  fixes an event no consumer should rely on when an explicit in-pipeline chain
  is available.
- **A `gh workflow run release-notes.yml` dispatch step at the end of
  `release.yml`** — rejected: it is fire-and-forget (the release run shows no
  status for the notes run and no `needs:` ordering in the run graph) and
  requires granting the job `actions: write`; the reusable-workflow call keeps
  both run status and ordering visible in one run.

## Consequences

- Release notes now run as part of the release pipeline run. A notes failure
  turns the release run red even though the release itself is already
  published; operators can re-run the failed job.
- `secrets: inherit` passes `FIREWORKS_API_KEY` and `SLACK_WEBHOOK_URL` to the
  called workflow within the same repository trust boundary; no new secret
  exposure is introduced.
- The [ADR-0125](./0125-release-notes-amendments.md) amendments — input validation before the `$GITHUB_ENV` write,
  deny-by-default opencode permissions, and the graceful Slack skip — are
  unaffected and remain in force.
- `update-website.yml` has the same dead `release: [published]` trigger; it is
  explicitly out of scope here.
- [ADR-0124](./0124-release-notes-automation.md) remains Accepted and immutable; this ADR is the
  amendment-of-record for its trigger decision, noted there under
  **Later references**.
