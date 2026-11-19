---
type: adr
area: infra
claim: ef2714799e6705c36e2e68c44443744911ab4e60
date: 2026-10-15
deciders: CypherLite maintainers
name: 0302-review-model-env-override
status: accepted
tags:
  - review
title: Review-agent models are overridable via environment variables
REFERENCES:
  - "[[adr/0258-automated-pr-reviews]]"
---

# ADR-0302: Review-agent models are overridable via environment variables

## Context

[ADR-0258](./0258-automated-pr-reviews.md) pinned `fireworks-ai/accounts/fireworks/models/kimi-k3` as a
literal in the review automation: the shared reusable workflow
(`.github/workflows/opencode-review-reusable.yml`, which backs all three PR
review kinds — stellar, rust, security) and the stellar night-shift fixer
(`.github/workflows/stellar-nightshift.yml`). The stellar-review skill
(`.agents/skills/stellar-review/SKILL.md`) describes MODEL TIERS for its
four passes — structure on the default/strong model, branching on a mid
model, boundaries and comparison on the cheapest fast model — with
Anthropic-tier examples (`model: haiku`, `model: sonnet`), and leaves the
concrete model to the dispatcher. Trying a different model on any of these
surfaces therefore means editing tracked files and pushing a commit before
a single review run can be observed.

LABS-320 asks for model trials to be a repository-setting change: an
operator swaps a model, watches review rounds, and swaps back, with no
tracked-file churn. Where a value is sensitive it must stay masked, and a
mis-set value must fail the run loudly rather than silently fall back to
an unintended model.

## Decision

Review-agent models are overridable through environment variables
populated from GitHub secrets or repository variables, with a literal
default in each workflow. Every consumer resolves its model in the order
**secrets → vars → default** — the GitHub Actions expression
`${{ secrets.X || vars.X || '<default>' }}`: a secret wins over a
repository variable, which wins over the literal default.

| Variable | Default | Purpose |
| --- | --- | --- |
| `OPENCODE_REVIEW_MODEL` | `fireworks-ai/accounts/fireworks/routers/glm-fast-latest` | Model for the three PR review kinds (stellar, rust, security) dispatched through the shared reusable workflow |
| `STELLAR_NIGHTSHIFT_MODEL` | `fireworks-ai/accounts/fireworks/routers/glm-fast-latest` | Model for the stellar night-shift fixer |
| `STELLAR_MODEL_STRUCTURE` | *(empty)* | Model for the stellar structure pass; empty = no override |
| `STELLAR_MODEL_BRANCHING` | *(empty)* | Model for the stellar branching pass; empty = no override |
| `STELLAR_MODEL_BOUNDARIES` | *(empty)* | Model for the stellar boundaries pass; empty = no override |
| `STELLAR_MODEL_COMPARISON` | *(empty)* | Model for the stellar comparison pass; empty = no override |

The four pass-tier variables have no literal default: `(empty)` in the
table means the empty string, and an empty value is no override:
locally the skill's MODEL TIERS guidance applies, so the dispatcher
picks the tier; in CI the pass inherits the run-level model (below).
The two run-level variables default to the Fireworks router
`fireworks-ai/accounts/fireworks/routers/glm-fast-latest`.

That default switches away from [ADR-0258](./0258-automated-pr-reviews.md)'s
`fireworks-ai/accounts/fireworks/models/kimi-k3` pin at the maintainers'
request. [ADR-0258](./0258-automated-pr-reviews.md) rejected a Fireworks router
(`accounts/fireworks/routers/kimi-latest`) because a router tracks future
releases, so the review gate's model drifts and the gate stops being
reproducible. That drift is accepted here: the default follows GLM fast
releases with no tracked change. It is reversible without a code change —
setting `OPENCODE_REVIEW_MODEL` / `STELLAR_NIGHTSHIFT_MODEL` to a pinned
catalog ID (for example `fireworks-ai/accounts/fireworks/models/glm-5p3`)
restores a reproducible gate. The router was verified on 2026-10-16: its
models.dev catalog entry — the catalog OpenCode resolves models from on CI,
using the existing `FIREWORKS_API_KEY` secret — is GLM 5.3 Fast (Latest)
with `tool_call: true`, and a local `opencode run --standalone` with it
answered a prompt and executed a tool call.

In CI the pass-tier values reach the passes as configuration, not as
dispatch guidance: an OpenCode subagent uses its configured model, or
inherits the parent session's model when none is configured, so the CI
dispatcher cannot pick a model per dispatch. Before the stellar review's
`opencode run`, the workflow runs
`.agents/skills/stellar-review/tools/pass_models.py`, which writes each
non-empty value verbatim as `agents.stellar-<pass>.model` in the
runner's global OpenCode config (`~/.config/opencode/opencode.json`);
an empty value writes nothing, so that pass inherits the run-level
model (a dispatcher without per-dispatch model choice has no tier to
pick). The repository's `.opencode/agents/stellar-*.md` set no `model`,
so the generated value survives OpenCode's config merge. Local
dispatchers that can set a per-dispatch model read the variables per the
skill's dispatch guidance and pass the value in their own model syntax
(an OpenCode `provider/model`, a Claude Code alias such as `haiku`).

This is a deliberate amendment to [ADR-0258](./0258-automated-pr-reviews.md)'s pinned-model decision, not a
revocation of its runner, skill, or gate decisions: the model becomes a
default value rather than a hard constant, and that default is the router
above. The implementation — workflow steps, the pass-model config
generator, and the skill's dispatch guidance — lands in the same change
set. Secret values are masked in run logs by GitHub Actions;
repository-variable values are not, which is part of why secrets take
precedence in the resolution order.

## Alternatives considered

- **Keep the literal pin and edit the workflow files per experiment** —
  rejected: every model trial becomes tracked-file churn that lands in
  review diffs, so reviewers cannot tell a model experiment from a
  deliberate change to the review gate.
- **Put the model choice in the tracked `opencode.json` or agent
  frontmatter** — rejected: the CI review steps need the
  `opencode run --model` value before any agent configuration is
  consulted. For the passes, OpenCode V2 config substitutes
  `{env:VAR}` references, so a tracked
  `"model": "{env:STELLAR_MODEL_STRUCTURE}"` could carry a per-run
  choice, but V2 replaces an unset variable with an empty string, which
  yields an empty agent model whose behaviour V2 does not document;
  tracked OpenCode config is also out of this feature's scope. The
  per-pass config the Decision generates is written at run time from the
  variables and never tracked.
- **A repository variable only, with no secret path** — rejected:
  repository variables are visible to everyone with repo read access and
  cannot hold a value the maintainers want restricted; honoring both
  contexts, with the secret winning, keeps a restricted trial model
  possible without a second mechanism.
- **One global `OPENCODE_MODEL` for every surface** — rejected: it cannot
  express the per-pass tiers the stellar skill defines — structure needs
  a strong model while boundaries and comparison are deliberately cheap —
  and it would couple unrelated surfaces (the PR reviews and the
  night-shift fixer) to a single knob.
- **Keep [ADR-0258](./0258-automated-pr-reviews.md)'s kimi-k3 pin as the default and run GLM through the
  override** — rejected: the maintainers want GLM fast as the standing
  default, and a permanently set override would move the effective default
  into repository settings, outside review.
- **Default to a pinned GLM catalog ID such as
  `fireworks-ai/accounts/fireworks/models/glm-5p3`** — rejected: the
  maintainers asked for the router so the default follows GLM fast releases
  without a tracked bump per release; the pinned ID stays one repository
  setting away for a reproducible gate.

## Consequences

- Swapping a review model becomes a repository-setting change: set a
  secret or variable and re-run; no code edit, no review diff.
- With nothing set, the PR reviews and the night-shift fixer run on the
 `glm-fast-latest` router instead of [ADR-0258](./0258-automated-pr-reviews.md)'s kimi-k3 pin, so review
  output can differ from earlier rounds.
- The router tracks GLM fast releases, so the unset gate's model can
  change between runs with no tracked change; a reproducible gate needs a
  pinned catalog ID in the override variable.
- The default still lives in the workflow files, so changing it remains a
  tracked change that goes through review; model releases behind the
  router do not.
- In CI, per-pass overrides are applied by the workflow as per-agent
  OpenCode config, independent of the dispatcher; locally they are
  honored only by dispatchers that can set a per-dispatch model and
  follow the stellar skill's dispatch guidance. The skill, the generator,
  and the workflows must stay in sync.
- The CI override holds only while the repository's
  `.opencode/agents/stellar-*.md` set no `model`; adding one there would
  shadow the generated value.
- A mis-set run-level value (`OPENCODE_REVIEW_MODEL`,
  `STELLAR_NIGHTSHIFT_MODEL`) fails the review run loudly at
  `opencode run` instead of silently falling back to another model; a
  mis-set pass value fails that sub-agent's model resolution, the
  skill's never-post-a-partial-review rule stops the review, and
  `post.py fail` fails the check.
