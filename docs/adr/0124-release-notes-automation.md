# 0124. Generate and publish release notes with OpenCode

- **Status**: Accepted
- **Date**: 2026-09-28
- **Deciders**: CypherLite maintainers (Linear LABS-133)
- **Later references**: amended by ADR-0265 (trigger changed from
  `release: [published]` to an explicit `workflow_call` chain from
  `release.yml`; the `release: published` event never fired because the
  release is published with `GITHUB_TOKEN`); amended by ADR-0465 (the
  Slack publish step moved from the release-notes run to the release
  pipeline's final job); amended by ADR-0618 (the generator reads a
  harvested pull-request corpus and organizes its output under fixed
  area groups, the model is a pinned strong tier with a
  `RELEASE_NOTES_MODEL` override, and CI guards CHANGELOG.md entry
  placement)

## Context

Every CypherLite release is cut by `release.yml`: `softprops/action-gh-release`
publishes the GitHub release (step in the `create-release` job), which fires the
`release: published` event. That event is already an established trigger in this
repository — `.github/workflows/update-website.yml` hooks it to sync the site —
so a release-notes workflow can hang off the same event with no new release
mechanism. Today the published release body is whatever `action-gh-release`
generates from GitHub's own commit grouping; there is no narrative summary, and
nothing is posted to Slack.

Linear issue **LABS-133** ("Generate and publish release notes") asks for
generated notes on the GitHub release page plus a post to a Slack channel. The
channel is TBD and is determined by whichever webhook is configured. The
inspiration is neo4j-cli's
`https://github.com/neo4j-labs/neo4j-cli/blob/main/.github/workflows/post-release-notes.yml`,
which runs claude-code with `ANTHROPIC_API_KEY` and fails when its Slack secret
is missing. LABS-133 instead mandates OpenCode with `FIREWORKS_API_KEY`, and
requires a missing `SLACK_WEBHOOK_URL` to be a graceful skip rather than a
failure.

This ADR records the already-agreed decision before implementation, per the
repository's decision-log rule. Two precedents frame it: **ADR-0258** runs
OpenCode headless in CI for PR reviews (the reusable workflow that pins the
runner install and the model secret), and **ADR-0123** is the closest release
infrastructure precedent, adding a release-time step behind the same
`release.yml` pipeline. The decisions below are settled; this record states them
rather than reopening them.

## Decision

**Generate release notes headless in GitHub Actions with `opencode run`
(OpenCode V2 CLI) when a GitHub release is published, write the agent's output
under `target/release-notes/`, and publish it back to the GitHub release and to
Slack.**

1. **Trigger.** `.github/workflows/release-notes.yml` runs on
   `release: [published]` — the event `release.yml` already fires through
   `softprops/action-gh-release`, and the same event `update-website.yml`
   consumes — and additionally on `workflow_dispatch` for a dry run.
2. **Runner: `opencode run` (OpenCode V2 CLI).** The workflow installs a pinned
   OpenCode release using the tarball-plus-sha256 pattern from
   `.github/workflows/opencode-review-reusable.yml`: a pinned
   `OPENCODE_VERSION`, a `curl` download of
   `opencode-linux-x64.tar.gz` from `opencode.ai/files/bin`, and
   `sha256sum -c` against a pinned `OPENCODE_SHA256` before extraction, so a
   swapped or corrupted tarball aborts the install before anything runs.
   Authentication is a `FIREWORKS_API_KEY` repository secret (ADR-0258).
3. **Model: `fireworks-ai/accounts/fireworks/routers/kimi-latest`.** This full
   path is a real models.dev catalog key under the `fireworks-ai` provider.
   ADR-0258's caveat — that the bare `kimi-latest` shorthand does not resolve on
   a cold runner — applies to that shorthand, not to this fully-qualified
   `accounts/fireworks/routers/…` catalog path; release notes are not a
   merge-gating check, so tracking the router's current Kimi is acceptable here
   where ADR-0258 pinned `kimi-k3` for the review gate.
4. **Prompt file: `.github/prompts/release-notes.md`.** The generation prompt is
   maintained in-repo and versioned with the workflow, alongside the other
   `.github/prompts/` files.
5. **Agent artifacts: `target/release-notes/notes.md` and
   `target/release-notes/slack-payload.json`.** The agent writes the markdown
   notes and the Slack message payload to these paths; the workflow reads them
   back to publish, so the publishing steps do not re-invoke the model.
6. **Graceful Slack skip.** If `SLACK_WEBHOOK_URL` is unset, the workflow logs a
   skip and continues without posting to Slack; the missing webhook is not a
   failure and never fails the release-notes run.
7. **Idempotent GitHub release update.** The workflow prepends an `## Release
   notes` section to the GitHub release body. Re-running replaces the existing
   section in place rather than appending a second one, so the release body
   stays stable across retries and re-runs.
8. **Dry run.** `workflow_dispatch` generates and reports the notes without
   publishing; a `publish` input defaults to `false`, so a manual run is a dry
   run unless `publish` is explicitly set true.
9. **Source: `CHANGELOG.md` first, git log fallback.** The prime source is the
   per-version section in `CHANGELOG.md` matching the released tag. When that
   section is empty or missing, the workflow falls back to the git log range for
   the release. The `0.8.0` section is currently empty, so the fallback is
   mandatory, not optional.

## Alternatives considered

- **claude-code with `ANTHROPIC_API_KEY`, as in the neo4j-cli inspiration** —
  rejected: LABS-133 mandates OpenCode as the runner and `FIREWORKS_API_KEY`
  for authentication, matching the OpenCode CI agents already established in
  this repository by ADR-0258. The inspiration's runner and secret are exactly
  the two things the issue overrides.
- **Fail fast when `SLACK_WEBHOOK_URL` is missing, as in the
  inspiration** — rejected: LABS-133 mandates a graceful skip, because the
  Slack channel is TBD and the webhook may not be provisioned when the workflow
  first runs; a missing secret must not turn every release red or block the
  GitHub release update.
- **Publish the release body directly from the model's stdout instead of
  artifacts under `target/release-notes/`** — rejected: separating generation
  (write `notes.md` and `slack-payload.json`) from publishing gives the
  `workflow_dispatch` dry run something concrete to inspect, makes the publish
  steps deterministic and re-runnable, and keeps the Slack payload reviewable
  independently of the GitHub body.
- **Rewrite the release body to only the generated notes** — rejected: the
  workflow prepends an `## Release notes` section so GitHub's own generated
  notes and the rest of the body are preserved, and the prepend is idempotent
  so re-runs do not stack duplicate sections.

## Consequences

- Every published GitHub release gains an `## Release notes` section, and when
  `SLACK_WEBHOOK_URL` is configured it also produces a Slack post; the release
  page becomes the narrative summary LABS-133 asks for.
- A `FIREWORKS_API_KEY` repository secret must be provisioned (already required
  by the ADR-0258 review workflows, so no new secret family); an optional
  `SLACK_WEBHOOK_URL` secret enables the Slack leg, and its absence is a logged
  skip rather than a failure.
- The release body is edited after publication, so the GitHub release is no
  longer write-once; the idempotent `## Release notes` prepend makes retries
  safe, but anything else consuming the release body should expect the section.
- The `CHANGELOG.md` per-version section is now load-bearing for release-notes
  quality: an empty or missing section silently degrades to git-log output. The
  empty `0.8.0` section is the immediate case and is why the fallback exists;
  keeping changelog sections populated keeps the primary source useful.
- The pinned-tarball-plus-sha256 OpenCode install and the pinned model path
  become repository surface like ADR-0258's review workflow; bumping
  `OPENCODE_VERSION`, `OPENCODE_SHA256`, or the model path changes release-time
  behavior and follows the normal review process.
- Generation runs once per published release (and per manual dispatch), so the
  Fireworks token cost is bounded by release and dry-run frequency rather than
  by push volume, unlike the per-push review workflows of ADR-0258.
