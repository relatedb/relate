---
type: adr
area: infra
date: 2026-09-29
deciders: CypherLite maintainers (agreed in conversation)
name: 0129-website-release-chain
status: accepted
tags:
  - release
title: Trigger the website update via the explicit release.yml chain
REFERENCES:
  - "[[adr/0265-release-notes-explicit-chain]]"
  - "[[adr/0330-release-chains-pages-deploy]]"
  - "[[adr/0546-release-discovery-via-relate-web]]"
---

# 0129. Trigger the website update via the explicit release.yml chain

Later references: [ADR-0546](./0546-release-discovery-via-relate-web.md) supersedes this ADR in full — update-website.yml
is removed; the website lives in the relate-web repository.

## Context

[ADR-0265](./0265-release-notes-explicit-chain.md) established that `release: [published]` is a dead trigger in this
repository: `release.yml` publishes releases via `softprops/action-gh-release`
with the default `GITHUB_TOKEN`, and GitHub suppresses downstream workflow
runs for `GITHUB_TOKEN`-triggered events. `release-notes.yml` was converted
to an explicit `workflow_call` chain from `release.yml`.

`.github/workflows/update-website.yml` had the identical dead trigger. The
evidence: 0 release-triggered runs across v0.8.0, v0.8.1, v0.9.0, and
v0.9.1 (all published by `github-actions[bot]`); its only runs are manual
`workflow_dispatch`. The site last synced at v0.7.x.

## Decision

Apply the [ADR-0265](./0265-release-notes-explicit-chain.md) fix to the website workflow:

1. `update-website.yml` gains a `workflow_call` trigger and drops the dead
   `release: [published]` trigger; `workflow_dispatch` (manual run) stays.
2. `release.yml` gains an `update-website` job, chained with
   `needs: [version, create-release]`, calling the reusable workflow with
   `secrets: inherit` (FIREWORKS_API_KEY) — the same shape as the
   `release-notes` job.

## Alternatives considered

- **PAT or GitHub App token on `action-gh-release`** — rejected for the same
 reasons as [ADR-0265](./0265-release-notes-explicit-chain.md) (new secret family, and an explicit chain is
  available).
- **`gh workflow run` dispatch from release.yml** — rejected: fire-and-forget,
  no run-graph ordering, needs `actions: write`.

## Consequences

- Every release from v0.9.2 onward syncs the website automatically, with run
  status visible inside the release run's graph.
- A dry website sync remains `gh workflow run update-website.yml`.

## Later references

[ADR-0330](./0330-release-chains-pages-deploy.md) notes a side effect of this chain. A called `update-website.yml`
creates no "Update website" run, so `deploy-pages.yml`'s
`workflow_run: [Update website]` trigger never fired for a release, and the
synced page went live only with the next push to `main`. The release run now
calls `deploy-pages.yml` itself, after every `gh-pages` writer. The trigger
stays for manually dispatched website syncs.
