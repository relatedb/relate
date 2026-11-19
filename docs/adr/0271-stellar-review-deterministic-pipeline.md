---
type: adr
area: infra
date: 2026-09-29
deciders: CypherLite maintainers
name: 0271-stellar-review-deterministic-pipeline
status: proposed
tags:
  - review
title: Stellar review as a deterministic, incremental pipeline
REFERENCES:
  - "[[adr/0258-automated-pr-reviews]]"
  - "[[adr/0275-stellar-review-scoped-incremental-rounds]]"
---

# 0271. Stellar review as a deterministic, incremental pipeline

- **Renumbered**: from 0142 (shared-number cleanup)

## Context

[ADR-0258](./0258-automated-pr-reviews.md) introduced automated PR reviews with OpenCode; commit 8be9e40b
("stellar-review as deterministic fan-out pipeline", PR #135) restructured
the stellar-review skill from one monolithic pass into a fan-out of four
focused passes plus a verify pass, to stop findings from trickling out over
successive review rounds.

Running it in CI exposed two structural problems:

1. **Rounds are still stateless and unbounded.** Every push re-reviews the
   whole diff from a cold start. Fixes get re-reviewed wholesale together
   with untouched code, so "new useful findings" keep surfacing on code no
   round flagged before — each round re-samples the latent finding pool,
   and there is no convergence: a review can never be *done*, only
   re-started.
2. **Cost is dominated by invocation mechanics, not judgment.** Measured
   on CI (September 2026): the OpenCode step alone takes 40–85 minutes on
   code PRs (e.g. run 36565552783: 43m36s of pure model time). The drivers
   are structural — the orchestrator serially generates the full doctrine
   into every sub-agent prompt (~15–25k output tokens before any review
   starts), four passes plus a verify pass each re-read the entire diff
   and re-explore files (~5–8 full-diff reads per round), and posting
   burns one model turn per inline comment plus verdict/summary handling.

The fan-out commit's own dogfood run also showed *what* the expensive
verify pass actually catches: not new defects, but **inconsistencies
between similar sites** (an asymmetric port-parse refusal vs a silent
`.ok()` swallow) — a defect class that is visible only when similar code
is placed side by side, which no amount of re-reading the same diff finds
reliably.

The premise of this ADR: everything mechanical about a review — what
changed, how big things are, where the branches/casts/fallbacks are, which
units look alike, what was already decided — belongs in deterministic
code. Model tokens should buy per-unit judgment over pre-digested facts,
and verdicts should persist so rounds are incremental.

The decisions below are already agreed; this ADR records them before
implementation, per the repository's decision-log rule.

## Decision

Stellar review becomes a deterministic, incremental pipeline: a facts
extractor and a cross-round ledger (both plain Python, zero model tokens)
surround a small set of static review agents; a poster script turns their
structured output into GitHub comments and the verdict.

1. **Deterministic facts pack.** A Python-stdlib-only extractor
   (`.agents/skills/stellar-review/tools/facts.py`) computes, from base/head
   SHAs alone: changed-file list and numstat, current and base file sizes
   with 1k-line crossings, a **unit inventory** (changed functions / impl
   blocks / types, with stable IDs `path::name`, signatures, spans, and
   content hashes), a **site index** (typed occurrences of unwrap/expect,
   silent `.ok()`/`let _ =` fallbacks, `as` casts, new bool/Option params,
   new `pub` items, branches added to existing functions, with line
   numbers), **similarity groups** (token-shingle near-duplicate candidates
   across the diff), and **module dependency edges** among changed files.
   Output is `.stellar/facts.json`, byte-identical for identical input. CI
   passes base/head from the pull-request payload, so no merge-base is
   needed in the shallow checkout; locally it defaults to
   `merge-base HEAD origin/main`.
2. **Cross-round ledger with sealed units.** A ledger tool
   (`tools/ledger.py`) keeps per-unit state (content hash, verdict, open
   findings) and finding lifecycles (open / resolved / waived) across
   rounds. Round N+1 scope is exactly: units whose hash changed, plus
   units whose **dependencies' public signatures** changed, plus units
   carrying still-open findings (for a closed-form "addressed? yes/no"
   resolution check — not a re-review). Every other unit is **sealed**: it
   cannot produce new findings that round. Waived findings stay waived
   until their unit changes. This is what makes rounds converge: untouched
   code is structurally incapable of generating new comments.
3. **Ledger persistence in the summary comment.** The CI ledger round-trips
   as a trimmed block inside the existing self-updating
   `github-actions[bot]` summary comment (hidden HTML comment), read and
   written by scripts — not by the model. Unit state is stored compactly
   (`uid → "hash:sighash"`); past a size budget the block is zlib+base64
   compressed, and at the extreme the unit map is dropped entirely —
   sealing then degrades to merge-base-relative scoping (bigger rounds,
   never wrong), because units absent from the ledger count as changed
   only when they differ from the base. Locally the ledger is a gitignored
   `.stellar/ledger.json` file. No Actions cache, no artifacts, no new
   permissions.
4. **Static review agents, no prompt pasting.** The review passes become
   project agent definitions (`.opencode/agents/stellar-*.md`, mirrored to
   `.claude/agents/` for local Claude runs) whose own prompt text carries
   their mandate and doctrine. The orchestrator dispatches them with a
   short pointer prompt (facts file, assigned units) instead of serially
   generating the doctrine into every sub-agent prompt. The old
   `orchestration` pass is absorbed into the sites-based boundaries
   mandate; a **comparison pass** replaces the verify pass — it receives
   similarity groups and same-kind site clusters side by side and judges
   consistency, the defect class the verify pass was actually catching.
5. **Closed-form judgment over facts, not diff search.** Passes receive
   the facts pack and their assigned unit list with pre-extracted context,
   and emit verdicts per checklist item. Open-ended "read the diff and find
   problems" — the low-recall, high-variance mode that caused the trickle —
   is no longer part of the pipeline.
6. **Scripted posting and verdict.** The model's only output artifact is
   `.stellar/findings.json` (schema-validated). A poster script
   (`tools/post.py`) dedupes findings, maps severities (blocker → High,
   medium → Medium, nit → Low), posts all inline comments in **one**
   `POST /repos/.../pulls/N/reviews` call, writes
   `.opencode-verdict.txt` itself, and updates the summary comment
   (discover-then-edit) including the ledger block. The model never calls
   `gh` for posting and never writes the verdict; the existing fail-closed
 enforcement chain ([ADR-0258](./0258-automated-pr-reviews.md)) is unchanged.
7. **Shared core for CI and local use.** The same tools and agent
   definitions drive the GitHub workflow and local invocations of the
   `stellar-review` skill; CI only adds the ledger-in-comment round-trip
   and the posting step. The gbuild plugin's auditor is unaffected and may
   adopt the machinery later, in its own repository.

## Alternatives considered

- **Keep the fan-out pipeline and tune prompts/model** — rejected: tuning
  cannot fix statelessness (rounds would still re-review untouched code)
  and does not touch the serial prompt-generation and redundant-read
  costs, which are inherent to that invocation shape.
- **One big single-pass review with a stronger model** — rejected: the
  fan-out experiment showed parallel focused contexts with exhaustive
  mandates have materially better recall than one monolithic read; a
  bigger model also worsens the latency problem instead of fixing its
  structural causes.
- **Actions cache or workflow artifacts for ledger state** — rejected:
  cache eviction silently loses state (reviews would randomly reset to
  full scope), and artifact hand-off requires brittle
  find-the-previous-run plumbing. The summary comment already exists, is
  per-PR, self-heals when missing, and keeps the state inspectable.
- **Rust (`syn`) or tree-sitter extractor for precise ASTs** — rejected:
  CI runners only guarantee Python; review units need function-ish
  granularity and stable hashes, not exact ASTs; and a stdlib regex/token
  extractor covers the non-Rust client languages for free. Precision can
  be upgraded later without changing the pipeline contract.
- **CypherLite database as the inventory/ledger store** — rejected for
  now: JSON plus deterministic scripts is sufficient at PR scale and keeps
  the CI path dependency-free; a database would add operational friction
  (checkout, build, store lifecycle) for query power the pipeline does not
  yet need. If cross-PR or repo-wide queries ever justify it, the facts
  JSON is a straightforward import source — and a good dogfood candidate.
- **Keep model-driven posting (per-comment `gh api` calls)** — rejected:
  dozens of extra model turns per round on a slow model, and a fragile
  hand-written verdict chain, where one script and one review call are
  deterministic and testable.

## Later references

- [ADR-0275](./0275-stellar-review-scoped-incremental-rounds.md) tightens decisions 2 and 6: the poster enforces scope
  membership and incremental-diff anchoring on findings (not just
  resolutions), and the ledger keeps a capped tail of resolved findings
  so later rounds cannot contradict earlier remedies.

## Consequences

- New repository surface: `tools/` (facts, ledger, poster) with unittest
  suites wired into `review-scripts-test.yml`; agent definitions under
  `.opencode/agents/` and `.claude/agents/`; a gitignored `.stellar/`
  runtime directory.
- The stellar-review SKILL.md describes the pipeline instead of performing
  it: orchestration is dispatch-and-collect over facts, and all review
  doctrine lives in the static agents and shared references.
- Fix rounds on a PR become cheap (only changed units + signature-affected
  dependents are re-reviewed) and **new findings on untouched code become
  impossible by construction**; authors can waive findings, and waivers
  persist until the underlying unit changes.
- Signature-level dependency invalidation means a public-signature change
  in a central module legitimately widens the next round's scope; body-only
  changes never do.
- The poster's single `POST /reviews` call means inline comments that
  cannot anchor to a diff line are degraded to file-level or summary-only
  entries by the script, deterministically.
- The ledger block grows with a PR's lifetime; the writer trims it to live
  state (open findings, current unit hashes, waivers) and compresses past
  a budget, so comment size stays under GitHub's 65536-char limit (a
  6000-unit PR's ledger compresses to ~20 KB).
- [ADR-0258](./0258-automated-pr-reviews.md)'s verdict chain (verdict file → HTML comment → marker line →
  fail-closed gate) is preserved verbatim; this ADR changes only *who*
  writes the verdict (a script, not the model).
- The findings document is model output and is treated as untrusted at
  the script boundaries it crosses: resolutions are honored only for ids
  in the round's `resolve_checks` (a compromised or confused model cannot
  silently close a finding on untouched code), the scripted model step
  runs without a GitHub token, and the posting/verdict steps restore the
  workspace before executing the tools from the tree.
- **Accepted risk**: the comment-embedded ledger trusts the
  `github-actions[bot]` summary comment, which any user with repo write
  access (every PR author here — forking is disabled) can edit. A member
  could forge a ledger to seal code from review on their own PR, but the
  edit leaves a public audit trail (GitHub marks edited comments with
  editor and timestamp), and a forged ledger can only *shrink* a round,
  never skip the review entirely — the fail-open direction is a full
  round. If the repo ever opens to fork PRs, the ledger channel needs
  signing (e.g. HMAC with a repository secret) before the check trusts
  it again.
