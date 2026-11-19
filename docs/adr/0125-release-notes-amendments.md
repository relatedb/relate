---
type: adr
area: infra
date: 2026-09-28
deciders: CypherLite maintainers
name: 0125-release-notes-amendments
status: accepted
tags:
  - release
title: Post-review amendments to release-notes automation
REFERENCES:
  - "[[adr/0124-release-notes-automation]]"
---

# 0125. Post-review amendments to release-notes automation

## Context

**[ADR-0124](./0124-release-notes-automation.md)** decided to generate release notes headless with `opencode run`
and publish them to the GitHub release and to Slack. The implementation on the
feature branch then went through the stellar code-quality review and the
security review, and both returned blocking findings. This ADR records the
three amendments agreed in that review loop to resolve the findings.

None of the amendments contradicts [ADR-0124](./0124-release-notes-automation.md). [ADR-0124](./0124-release-notes-automation.md) pinned the *behavior* of
the idempotent GitHub release update — replace the generated section in place
on re-run — but never pinned the delimiter mechanism that implements it, so
replacing the delimiter refines the decision rather than reversing it. Its
graceful-skip decision (a missing `SLACK_WEBHOOK_URL` is never a failure) is
likewise refined, not reversed: the Slack artifact validation now follows the
same principle. The findings are: the stellar review's blocking items (1) a
silent data-loss hole in the `---` delimiter heuristic and (2) a contradiction
between unconditional `slack-payload.json` validation and the graceful-skip
decision; and the security review's blocking items (1) persisted write
credentials plus an unconstrained Bash tool on prompt-injectable input and
(2) a `GITHUB_ENV` injection path via the unvalidated `workflow_dispatch`
`version` input.

## Decision

**Amend the release-notes workflow in three ways: HTML comment markers as the
generated-section delimiter, Slack payload validation conditional on the Slack
leg firing, and runner hardening for credentials, tool use, and the dispatch
input.**

1. **HTML comment markers replace the `---` separator as the
   generated-section delimiter.** The generated section in the GitHub release
   body is delimited by `<!-- release-notes:start -->` and
   `<!-- release-notes:end -->` instead of a `---` horizontal rule. The `---`
   heuristic had a verified silent-truncation hole: a body starting with
   `## Release notes` and containing a hand-written `---` rule later would
   lose the hand-written content on re-run, because the replacement logic
   could not distinguish the generated delimiter from human-authored rules.
   HTML comments render invisibly on GitHub and cannot collide with human- or
   agent-authored markdown. This also deletes the notes-file `---` rejection
   guard and the prompt's no-horizontal-rule constraint, both of which existed
   only to prop up the fragile delimiter.
2. **Slack payload validation is conditional on the Slack leg firing.**
   `notes.md` missing or empty always hard-fails — the GitHub release update
   is the primary artifact. `slack-payload.json` missing or invalid hard-fails
   only when publishing AND `SLACK_WEBHOOK_URL` is set; otherwise the workflow
   warns and continues. A no-webhook or dry-run run must never be blocked by
 the Slack artifact, which is the graceful-skip principle of [ADR-0124](./0124-release-notes-automation.md)
   applied to the artifact itself.
3. **Runner hardening.** Three changes:
   - **No persisted credentials on checkout.** `actions/checkout` runs with
     `persist-credentials: false`, so the agent cannot reach the job's
     `contents:write` token through git.
   - **Deny-by-default tool permissions.** The agent runs with a Bash
     allowlist limited to `git show`/`log`/`describe`, `jq`, `wc`, `mkdir`,
     `cat`, `grep`, `ls`, and `head`, and with webfetch/websearch denied. The
     configuration is supplied via `OPENCODE_CONFIG` pointing at a committed
     CI config file; no repo-root `opencode.json` exists that could override
     it.
   - **Validate the dispatch input before any `$GITHUB_ENV` write.** The
     `workflow_dispatch` `version` input is validated (semver shape, newline
     rejected) before anything is written to `$GITHUB_ENV`, closing a
     GITHUB_ENV-injection path that could flip `PUBLISH` or swap
     `OPENCODE_SHA256`.

## Alternatives considered

- **Keep the `---` separator plus the prompt constraint forbidding horizontal
  rules in the generated notes** — rejected: the hand-written-rule truncation
  hole remains for bodies authored outside the workflow, and prompt rules are
  unenforceable — the model is not a reliable guard for a data-loss boundary.
- **Keep unconditional `slack-payload.json` validation and document the
  behavior instead** — rejected: a no-webhook or dry-run run must never be
  blocked by the Slack artifact; documentation cannot reconcile a hard-fail
 with [ADR-0124](./0124-release-notes-automation.md)'s graceful-skip mandate.
- **Rely on the pinned tarball-plus-sha256 install alone for runner
  safety** — rejected: it secures the tool supply chain but not the runtime:
  the agent still ran with persisted write credentials, an unconstrained Bash
  tool, and a `GITHUB_ENV` injection path, all on prompt-injectable input.

## Consequences

- The generated section in the release body is delimited by invisible HTML
  comments, so re-runs can never truncate hand-written content and no prompt
  constraint or notes-file guard is needed; bodies already published with the
  `---` scheme are superseded on the next re-run.
- A missing or invalid `slack-payload.json` only fails the run when the Slack
  leg is actually going to fire, aligning artifact validation with the
  graceful-skip decision; `notes.md` remains a hard requirement in every run.
- The agent runs without a persisted `contents:write` token, with a
  deny-by-default Bash allowlist and no web access, shrinking what a prompt
  injection through release content can reach; the CI config file is versioned
  with the workflow and follows the normal review process.
- The `version` dispatch input is validated before any environment write, so
  `PUBLISH` and `OPENCODE_SHA256` can no longer be flipped through
  GITHUB_ENV injection.
- [ADR-0124](./0124-release-notes-automation.md) remains Accepted and immutable; this ADR is the
 refinement-of-record, and [ADR-0124](./0124-release-notes-automation.md)'s delimiter-neutral wording keeps both
  consistent.
