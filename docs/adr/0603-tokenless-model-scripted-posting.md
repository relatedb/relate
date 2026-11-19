---
type: adr
area: infra
claim: c33e656bfe606e62e96c5834b1d0828eb62710d0
name: 0603-tokenless-model-scripted-posting
status: proposed
tags:
  - review
title: Tokenless review model, scripted posting on every lane
REFERENCES:
  - "[[adr/0258-automated-pr-reviews]]"
  - "[[adr/0271-stellar-review-deterministic-pipeline]]"
---

# ADR-0603: Tokenless review model, scripted posting on every lane

## Context

[ADR-0258](./0258-automated-pr-reviews.md) runs three OpenCode review lanes off one reusable workflow. The
stellar lane was rebuilt as a deterministic pipeline ([ADR-0271](./0271-stellar-review-deterministic-pipeline.md)): scripts
compute the inputs before the model and post after it, so its model step
runs with no GitHub token. The rust and security lanes kept [ADR-0258](./0258-automated-pr-reviews.md)'s
original shape: the checkout persists its token, the model receives it as
`GH_TOKEN`, and the model itself reads the PR diff through `gh`, posts
every inline comment, writes the verdict file, and upserts the summary
comment.

That shape hands the model — a shell-capable process reading
PR-controlled content — the runner's write credentials. Any prompt
injection that reaches a shell becomes a write under CI's identity:
approving its own review, editing the ledger the stellar gate reads, or
rewriting the workflow in a later push. The stellar restructure closed
exactly this class; the remaining two lanes reopened it.

## Decision

Every lane runs the model tokenless; all posting is scripted.

1. **No token on any pipeline path.** `actions/checkout` sets
   `persist-credentials: false` unconditionally; the model step's
   `GH_TOKEN` is the literal string `none`, an invalid token that makes
   any `gh` call fail loudly; the reusable workflow's two remote actions
   are SHA-pinned with resolving version comments.
2. **Scripted inputs before the model.** A per-kind pre-step fetches the
   PR endpoints, computes the merge-based diff with the stellar
   pipeline's facts extractor, and decides the no-op gate per kind: rust
   reviews run only when `*.rs`, `Cargo.toml`, or `Cargo.lock` changed;
   security reviews run on any code change and always on the CI-control
   paths (`.github/workflows`, `.github/scripts`, `.github/prompts`,
   `.agents/skills`). The model reads the diff artifact — never `gh`.
3. **Scripted posting after the model.** A per-kind poster validates
   `.stellar/findings.json` — accepted only as a regular untracked file —
   and owns the inline comments (one reviews call), the single summary
   comment with the marker line, `.opencode-verdict.txt`, and the
 verdict ([ADR-0258](./0258-automated-pr-reviews.md)'s severity gate: any Critical/High/Medium finding
   fails). It reuses the stellar poster's gh mechanics and choreography,
   and the stellar post-step hardening: pinned PATH, neutralized hooks,
   reset to the event payload's merge SHA (local HEAD is model-moveable),
   the untracked sweep outside `.stellar`, and the choreography script
   itself staged from trusted content — git show of the event payload's
   PR head SHA into a fresh temp file, never the model-writable worktree
   copy. The model step's env blanks `GITHUB_ENV`/`GITHUB_PATH`
   (empty-string entries), so the model shell publishes no environment
   variable or PATH entry — a `BASH_ENV`-style payload included — into
   the later steps. A timed-out run never passes — a killed
   single-shot review's document is not trusted for a verdict. The
   fail-closed chain through `enforce-opencode-verdict.sh` is unchanged.

## Alternatives considered

- **Narrow the token's scopes, keep model-driven posting** — rejected:
  scopes cannot distinguish a wanted review comment from an injected
  one, so a forked-permission token still authorizes the exact writes an
  injection wants.
- **A GitHub App with fine-grained credentials** — rejected: new
  credential infrastructure and a token exchange that breaks on workflow
  changes, to reach the same conclusion — the model does not need write
  access to do its job.
- **Harden the prompt against shell use instead** — rejected: the prompt
  is advisory to a process reading attacker-controlled diff content;
 [ADR-0271](./0271-stellar-review-deterministic-pipeline.md) already established that posting is deterministic work a
  script does better and cheaper.

## Consequences

- The rust and security prompts shrink to skill invocation plus the
  findings-document contract; their no-op gates and summaries are
  spelled once, in scripts, and pinned by a script test.
- A review's GitHub surface is determined by a findings document, so a
  round can be replayed and its verdict derivation tested offline.
- `gh` is needed only after the model step, on a workspace the post step
  restored first; the model's only outputs are the findings documents
  under `.stellar/`.
- The kind lanes gain the stellar lane's timeout discipline: a killed
  run fails closed with a distinct reason instead of a generic
  missing-verdict failure.
