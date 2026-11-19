---
type: adr
area: infra
date: 2026-09-22
deciders: CypherLite maintainers
name: 0258-automated-pr-reviews
status: proposed
tags:
  - review
title: Automated PR reviews with OpenCode
REFERENCES:
  - "[[adr/0302-review-model-env-override]]"
---

# 0258. Automated PR reviews with OpenCode

- **Renumbered**: from 0080 (shared-number cleanup)

## Context

Every pull request to this repository is reviewed by hand today. neo4j-cli
has automated PR reviews running in CI via claude-code-action
(`.github/workflows/claude-review-security.yml`,
`.github/workflows/claude-review-conventions.yml`): skill-driven review
prompts, inline comments on the diff, one self-updating summary comment per
review kind, and a fail-closed verdict gate script
(`enforce-claude-verdict.sh`) that turns a review's verdict file into a
check status. Linear issue **LABS-68** asks for the same capability for
CypherLite, but with OpenCode as the review runner instead of
claude-code-action.

The review kinds to run are maintainability (stellar-review), Rust
correctness and idiom (rust-reviewer), and security (security-review).
Each produces a structured verdict (findings with severity, plus a
pass/fail determination) that must gate the PR check, and each must keep
its PR comment surface tidy — one summary comment per review kind, updated
on every push, not a growing stack of bot comments.

The decisions below are already agreed; this ADR records them before
implementation, per the repository's decision-log rule.

## Decision

Automated PR reviews run headless in GitHub Actions with OpenCode as the
runner, one workflow per review skill, gated fail-closed on severity.

1. **Runner: `opencode run` (OpenCode V2 CLI) headless in GitHub Actions.**
   The review is a headless OpenCode invocation with a skill-driven prompt,
   not claude-code-action. OpenCode shells out to `gh` for PR commenting.
2. **Model: `fireworks-ai/accounts/fireworks/models/kimi-k3` (Kimi K3).**
   The shorthand `fireworks-ai/kimi-latest` does not resolve on a cold
   runner — it is not a models.dev catalog key — so the pinned catalog ID
   is used. Authentication is a
   `FIREWORKS_API_KEY` repository secret surfaced through the models.dev
   catalog environment variable (verified against the catalog). One secret
   covers all three review workflows.
3. **Skills vendored in-repo at `.agents/skills/`.** The three review
   skills live at
   `.agents/skills/{stellar-review,rust-reviewer,security-review}/SKILL.md`
   — a path OpenCode V2 discovers natively as a project skill source.
   stellar-review is copied from neo4j-cli; rust-reviewer and
   security-review come from the author's local OpenCode skill set.
4. **One workflow per skill.** Three workflows —
   `.github/workflows/opencode-review-stellar.yml`,
   `opencode-review-rust.yml`, and `opencode-review-security.yml` — each
   running exactly one review per skill per push, so each review kind is an
   independent check. The three are thin `pull_request` callers of one
   shared reusable workflow
   (`.github/workflows/opencode-review-reusable.yml`, `on: workflow_call`)
   so the shared steps live in exactly one place; this does not change the
   separation — each caller is its own workflow, run, and check, and the
   rejected alternative below was a single combined run, not a shared body
   with three callers.
5. **Severity gate: Critical/High/Medium fails.** A review fails its check
   when any finding is Critical, High, or Medium; a review with only Low
   findings or no findings passes. The gate is enforced fail-closed by
   `.github/scripts/enforce-opencode-verdict.sh`, ported from neo4j-cli's
   `enforce-claude-verdict.sh`: verdict file →
   `<!-- opencode-verdict: ... -->` HTML comment → marker-line phrase →
   fail-closed error. If any stage of that chain is missing or malformed,
   the check fails rather than silently passing.
6. **Comments as `github-actions[bot]` via `GITHUB_TOKEN`.** No GitHub App
   or OAuth token exchange; the workflow's built-in `GITHUB_TOKEN` is
   sufficient for OpenCode's `gh`-based commenting. Each review kind keeps
   exactly one summary comment per PR via discover-then-edit: the workflow
   finds its own previous summary comment (identified by a marker) and
   edits it in place instead of posting a new one.
7. **Reviews are read-only.** Findings are posted as comments only; the
   review never commits or pushes to the PR branch.
8. **Prompts and scripts maintained in-repo.** Review prompts live under
   `.github/prompts/` and the enforcement script under `.github/scripts/`,
   versioned with the workflows that use them.

## Alternatives considered

- **claude-code-action as in neo4j-cli** — rejected: LABS-68 mandates
  OpenCode as the runner. Using `GITHUB_TOKEN` with OpenCode's `gh`-based
  commenting also avoids the App-token exchange failure mode neo4j-cli
  documents, where a PR that edits the review workflow cannot run its own
  review because the App token exchange is blocked on modified workflows.
- **Fireworks router `accounts/fireworks/routers/kimi-latest` as model
  ID** — rejected: the router tracks future Kimi releases, so the review
  gate's model would drift; LABS-68 names Kimi K3, and a pinned catalog ID
  keeps the CI gate reproducible.
- **AIR (Neo4j LiteLLM gateway, `air/kimi-k3`) as model provider** —
  rejected: no CI-suitable credential exists today — gateway auth is a
  personal Cloudflare Access token — whereas Fireworks needs only a single
  API-key repository secret.
- **One combined workflow running all three skills in a single run** —
  rejected: LABS-68 mandates one separate review per skill; separate
  workflows also isolate failures, token cost, and re-runs per review kind,
  so a flaky security review does not block re-running the maintainability
  review.

## Consequences

- A **`FIREWORKS_API_KEY` repository secret must be provisioned** before
  the workflows can run; without it every review check fails closed.
- PRs gain a **new required-check surface**: three per-skill review checks
  (`opencode-review-stellar`, `opencode-review-rust`,
  `opencode-review-security`) that fail when any finding is Critical, High,
  or Medium, or when the verdict chain is broken (fail-closed).
- `.agents/skills/` becomes repository surface that contributors' local
  OpenCode setups will also discover as project skills; changes to these
  SKILL.md files affect both CI reviews and local OpenCode sessions, and
  must be reviewed accordingly.
- Every push to an org-member PR consumes Fireworks tokens for three
  review runs; cost scales with push frequency and diff size.
- Review prompts (`.github/prompts/`) and the gate script
  (`.github/scripts/enforce-opencode-verdict.sh`) are maintained in-repo;
  changes to them change merge-gating behavior and follow the normal review
  process.
- Reviews are advisory-by-comment only: findings land as a single
  self-updating `github-actions[bot]` summary comment per review kind and
  never as commits, so reviewer judgment stays with humans even though the
  check gates the merge.

## Later references

- [ADR-0302](./0302-review-model-env-override.md) amends the pinned-model decision: the review-agent models are
  overridable through repository secrets or variables
  (`OPENCODE_REVIEW_MODEL`, `STELLAR_NIGHTSHIFT_MODEL`, and the four
 `STELLAR_MODEL_*` pass tiers). [ADR-0302](./0302-review-model-env-override.md) also switches the default from
  the `fireworks-ai/accounts/fireworks/models/kimi-k3` pin to the
  `fireworks-ai/accounts/fireworks/routers/glm-fast-latest` router,
  accepting the drift this ADR's router alternative rejected; a pinned
  catalog ID set through the override restores a reproducible gate. The
  runner, skill, and gate decisions here stand unchanged.
