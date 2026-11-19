---
type: adr
area: infra
claim: 648d7b4f137bed93d838d2dba65827214a66718e
name: 0465-slack-announce-after-pipeline
status: accepted
tags:
  - release
title: Announce the release on Slack after the pipeline is green
REFERENCES:
  - "[[adr/0124-release-notes-automation]]"
  - "[[adr/0125-release-notes-amendments]]"
  - "[[adr/0250-releases-json-update-discovery]]"
  - "[[adr/0265-release-notes-explicit-chain]]"
  - "[[adr/0330-release-chains-pages-deploy]]"
  - "[[adr/0358-upgrade-download-route]]"
---

# ADR-0465: Announce the release on Slack after the pipeline is green

## Context

The release pipeline (`release.yml`) publishes a release and then keeps
working. After `create-release` publishes the GitHub release, the chain runs
on: `release-notes` generates the narrative notes — the reusable
`release-notes.yml`, chained explicitly per [ADR-0265](./0265-release-notes-explicit-chain.md) — validates them, and
prepends them to the release body; `publish-releases-json` copies that body
into the website's `releases.json` ([ADR-0250](./0250-releases-json-update-discovery.md), gated on `release-notes`
precisely so the published notes are the ones `releases.json` carries);
`deploy-pages` makes the site live ([ADR-0330](./0330-release-chains-pages-deploy.md)); and `self-update-smoke`
canaries the user-facing upgrade path ([ADR-0358](./0358-upgrade-download-route.md)): it downloads the previous
stable binary, runs `cypherlite upgrade` against the live `releases.json`,
and asserts it reports this release's version.

The Slack post fires at the front of that chain. [ADR-0124](./0124-release-notes-automation.md) put generation
and publishing in one run: the agent writes
`target/release-notes/{notes.md,slack-payload.json}`, and the same job that
prepends the notes to the release body also POSTs the payload to the
webhook (`release-notes-slack.sh`, [ADR-0125](./0125-release-notes-amendments.md) amendment 2). When [ADR-0124](./0124-release-notes-automation.md)
landed, that run was the end of release automation. It no longer is — the
releases-json → pages → smoke tail has grown behind it, and the post stayed
where it was. At the moment the channel hears about a release,
`releases.json` does not yet list it, the site is not yet deployed, and the
canary has not yet run.

The same workflow is the manual path too. A `workflow_dispatch` run takes
`version` (validated — [ADR-0125](./0125-release-notes-amendments.md) amendment 3) and `publish` (default
`false`); a `publish: true` dispatch publishes in [ADR-0124](./0124-release-notes-automation.md)'s full sense —
the notes are prepended to the release body and the payload is POSTed to
Slack, inline, in the same run. The runs are serialized per tag by a
workflow-level concurrency group on `release-notes.yml`,
`release-notes-v<tag>`: the body prepend is a read-modify-write and the
inline POST is non-idempotent, so overlapping runs of the same tag must
never interleave. Any move of the Slack leg has to say what happens to
both of these.

The inversion is self-refuting for the canary in particular: the Slack
payload's first block after the version header is the literal
`cypherlite upgrade` command — the announcement's message is "a new version
is out, run this" — and `cypherlite upgrade` is exactly the path the canary
exists to prove. Today the channel can be told to upgrade through a path
the pipeline has not yet shown to work; if the tail fails after the post,
the message is already out and wrong.

## Decision

**Split the release-notes run — generate early, announce late — and hand
the Slack payload between the two halves as a workflow artifact. The
pipeline's announce job chains after the smoke canary; the per-tag
concurrency group stays on the generate run and is joined by the announce
job.**

1. **Generate early.** The `release-notes` job keeps its position and its
 work: chained after `create-release` ([ADR-0265](./0265-release-notes-explicit-chain.md)), it runs the opencode
   agent, validates the artifacts, and prepends the notes to the GitHub
   release body exactly as today. It additionally uploads
   `target/release-notes/slack-payload.json` as a workflow artifact. The
   in-run Slack POST step stays in the workflow, but on the pipeline path
   it no longer fires — the new `slack` input of Decision 5 gates it off.

2. **Announce late.** A new `announce` job in `release.yml`, with
   `needs: [version, self-update-smoke]`, downloads the payload artifact
   and POSTs it through the same `release-notes-slack.sh` policy — the
 graceful skip when `SLACK_WEBHOOK_URL` is unset ([ADR-0124](./0124-release-notes-automation.md)), the
   fail-closed payload validation, and the API-key exfiltration scan
 ([ADR-0125](./0125-release-notes-amendments.md) amendment 2). One script keeps the one Slack policy; the
   announce job adds no second leg. The payload is generated once, by the
   same agent run that writes the notes — announcing re-invokes no model.

3. **After the smoke, because of what the smoke proves.** The announce job
   chains after `self-update-smoke` — the pipeline's final step today — and
   becomes the final step itself. The canary proves the `cypherlite
   upgrade` path the announcement advertises; announcing only after it
   passes means the channel hears about a release whose upgrade path is
   proven, whose `releases.json` is live, and whose site is deployed. A
   failed job before the announce means no announcement — the release is
   already published; the announcement is visibility, not a gate — and the
   announce job can be re-run alone once the failure is fixed.

4. **The `release-notes-v<tag>` group stays on `release-notes.yml`, and
   the announce job joins it.** The group cannot move with the POST: the
   generate run still owns serialized side effects — its release-body
   prepend is a read-modify-write, and a manual `publish: true` dispatch
   still posts inline (Decision 5) — so the generate run keeps the
   workflow-level group exactly as today. The announce job declares the
   same group string at job level; GitHub concurrency groups are
   repository-global, so the two declarations are one group, and a manual
   `publish: true` run can never race the pipeline's post.

5. **Manual dispatch keeps its contract.** The `workflow_dispatch` path is
 not redesigned: `version` (required, validated — [ADR-0125](./0125-release-notes-amendments.md) amendment 3)
   and `publish` (default `false`) keep their meanings, a dry run still
   generates and prints without publishing, and a `publish: true` dispatch
 still posts to Slack inline, in the same run, exactly as [ADR-0124](./0124-release-notes-automation.md)
   promised. The one addition is the gate: a new boolean `slack` input
   (default `true`) on both entry points, so the inline POST fires only
   when `publish` and `slack` are both true. The default preserves the
   standalone contract; the chained release-pipeline call passes
   `slack: false`, so on the pipeline path the only post is the announce
   job's.

6. **The whole release-notes job cannot move; only its announce half
   can.** `publish-releases-json` needs the prepended notes — the body it
 copies into `releases.json` must already carry them ([ADR-0250](./0250-releases-json-update-discovery.md)) — and
   `self-update-smoke` needs `deploy-pages`, which needs
   `publish-releases-json`, because the canary upgrades against the live
   site. Chaining `release-notes` after `self-update-smoke` closes the loop
   `publish-releases-json → release-notes → self-update-smoke →
   deploy-pages → publish-releases-json`, a `needs` cycle GitHub rejects.
   The generate half is structurally early; this decision moves exactly
   the announce half.

## Alternatives considered

- **Move the whole `release-notes` job after `self-update-smoke`.**
  Rejected: it creates the needs cycle of Decision 6 — GitHub refuses the
  graph, and no reordering dissolves it, because the notes must precede
  `releases.json`, `releases.json` must precede the live site, and the
  live site must precede the canary the announcement waits for.
- **POST the payload from a final step of the `self-update-smoke` job.**
  Rejected: it grafts an announcement onto a job whose contract is the
 canary ([ADR-0358](./0358-upgrade-download-route.md)) — a failed POST would read as a smoke failure or force
  re-running the canary to retry an announcement — and the run graph would
  no longer carry the announcement as its own visible, independently
  re-runnable step.
- **Announce after `deploy-pages` but before the smoke.** Rejected: the
  gap this split closes is exactly the canary — the announcement
  advertises the upgrade path, and only the smoke proves it. Announcing
  between deploy and smoke reproduces the inversion one job earlier.
- **An announcer outside the pipeline, triggered on release-run completion
 (`workflow_run`).** Rejected: for the same reasons [ADR-0265](./0265-release-notes-explicit-chain.md) replaced the
  dead `release: published` trigger with an explicit chain — the ordering
  and the announce status would live outside the release run's graph,
  invisible to `needs` and to the operator reading the run.
- **Move the concurrency group off `release-notes.yml` with the POST —
  the generate run ungrouped, the group hung on the announce job alone.**
  Rejected: the generate run still serializes real side effects — the
  release-body read-modify-write, and the inline post of a manual
  `publish: true` dispatch — and the pipeline's post would no longer
  serialize against a concurrent manual run's. The group must cover both
  posters, so it stays on `release-notes.yml` and the announce job joins
  it.
- **Take posting away from the release-notes run on every path — announce
  only ever via the pipeline's job.** Rejected: it silently changes
 [ADR-0124](./0124-release-notes-automation.md)'s manual-dispatch contract (a `publish: true` dispatch would
  stop posting, with no signal to the operator). The `slack` input
  preserves that contract and costs the pipeline path nothing: the
  chained call passes `slack: false`.

## Consequences

- The channel hears about a release only after the entire pipeline —
  through the upgrade canary — is green: no announcement of a release
  whose `releases.json`, site, or upgrade path turned out broken.
- The announcement lands a little later than today (one pages deploy and
  one smoke job after the release exists). The GitHub release and its
 notes — [ADR-0124](./0124-release-notes-automation.md)'s primary artifact — are public exactly as before; only
  the Slack leg waits for the tail.
- `slack-payload.json` becomes a cross-job handoff: the generate run
  uploads it, the announce job downloads it. Re-running the announce job
  alone re-posts the same generated payload; re-running the generate job
  regenerates and re-uploads it. Either way the announce step re-invokes
  no model, so a release's Fireworks cost is unchanged.
- A missing `SLACK_WEBHOOK_URL` remains a logged skip — in the announce
  job on the pipeline path, in the inline step on a manual dispatch — and
 never fails the run [ADR-0124](./0124-release-notes-automation.md)).
- The per-tag concurrency group now spans two workflow files: a manual
  `publish: true` run and the pipeline's announce job serialize against
  each other, so neither a double post nor an interleaved body write is
  possible. The `release-notes-v<tag>` string is declared in both places —
  renaming it later means touching both.
- The `slack` input (default `true`) leaves the standalone workflow's
  contract intact; the pipeline passes `false`, so every path posts
  exactly once — inline on a manual dispatch, from the announce job on
  the pipeline.
- [ADR-0124](./0124-release-notes-automation.md) is amended, not reversed: the same agent run still writes both
  artifacts, and a manual `publish: true` dispatch still publishes both;
  on the pipeline path the Slack publish — its "publish it back … to
 Slack" — is executed by the pipeline's final job. [ADR-0124](./0124-release-notes-automation.md)'s
 Later-references note records this; the [ADR-0125](./0125-release-notes-amendments.md) amendments and the
 [ADR-0265](./0265-release-notes-explicit-chain.md) chain stand.
