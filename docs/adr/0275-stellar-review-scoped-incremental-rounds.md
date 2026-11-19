---
type: adr
area: infra
date: 2026-09-30
deciders: CypherLite maintainers
name: 0275-stellar-review-scoped-incremental-rounds
status: accepted
tags:
  - review
title: "Stellar review: script-enforced scope and resolved-findings memory"
REFERENCES:
  - "[[adr/0271-stellar-review-deterministic-pipeline]]"
---

# 0275. Stellar review: script-enforced scope and resolved-findings memory

- **Renumbered**: from 0145 (shared-number cleanup)

## Context

[ADR-0271](./0271-stellar-review-deterministic-pipeline.md) made the stellar review a deterministic, incremental pipeline:
round N+1 re-reviews only units that changed since round N, everything
else is **sealed**, and "new findings on untouched code become impossible
by construction". PR #154 (six review rounds) showed the construction has
three leaks:

1. **Sealing is a model instruction, not a script invariant.** The
   finding-schema rule "sealed means sealed" lives only in the prompt.
   The poster (`post.py`) schema-validates findings but never checks
   scope membership, so findings on sealed/context units entered the
   ledger, counted toward the verdict, and failed the gate (posted as
   "Findings on context units").
2. **Incremental rounds re-derive findings on unchanged lines.** A fix
   push re-opens the whole *unit*, and a fresh model context re-samples
   it: round N+1 flagged code that was already present (and passed over)
   in round N — e.g. a pre-existing `display_name() == "RANGE"` string
   comparison three hunks away from the push. The unit is legitimately
   in scope; the *unchanged lines* are not what the push asks anyone to
   judge.
3. **Resolved findings are forgotten.** `trim_for_comment` drops
   resolved findings from the comment-embedded ledger, so later rounds
   cannot see what was already litigated. On PR #154 this produced a
   direct self-contradiction: round 5's remedy mandated placing the
   legacy-encoding decoder next to the encoder in `catalog_schema.rs`;
   round 6 filed a new finding demanding it move to `redo/mod.rs`.

The result was an unbounded polish loop (11 → 6 → 7 → 9 → 3 open
findings per round) on a two-commit bugfix, ended by a human override.

## Decision

Three pipeline changes, all in deterministic code (plus the matching
prompt text):

1. **The poster enforces scope.** `post.py post` drops any new finding
   whose `unit` is not in the round's `review_units` (or, for unit-less
   file-level findings, whose `path` is not in `review_files ∪
   dependency_dirty_files`). Dropped findings never enter the ledger,
   never count toward the verdict, and are reported in the summary
   ("Dropped by the scope filter: N"). Without a scope document the
   filter is off — the same documented permissive bootstrap as
   resolutions.
2. **Incremental rounds anchor to the incremental diff.** In round ≥ 2
   (`scope.full == false`), a new finding's line range must intersect a
   hunk of the `last_head…head` diff. The post pipeline recomputes that
   diff from the ledger's `last_head` (scripts, never the model) and
   passes it to the poster via `--diff`; non-intersecting findings are
   dropped and reported like scope drops. One narrow exemption:
   `flag-1k-crossing` file-size findings on in-scope files survive —
   file growth is a whole-file fact the facts pack computes
   deterministically, and its natural quote (the file span) never
   intersects a hunk. Round 1 is unfiltered: the whole diff is fresh.
3. **The ledger remembers what was litigated.** `apply_update` maintains
   a capped `resolved_tail` (most recent 12 resolved findings: id, path,
   summary, remedy, round closed); `trim_for_comment` keeps it, and
   `brief_common` renders it as "Resolved in earlier rounds — do not
   re-report, and do not contradict the accepted remedy." A pass that
   sees its own predecessor's mandated remedy in the brief has no basis
   for filing its inverse.

The prompt-side text (`finding-schema.md`, `briefs.py`'s common header,
SKILL.md) is updated to state both rules, but enforcement does not
depend on the model obeying them.

## Alternatives considered

- **Keep filtering as prompt instructions only** — rejected: that is the
  status quo; PR #154 demonstrated the model does not reliably obey
  scope rules across six rounds and four sub-agent contexts.
- **Anchor findings to added (`+`) lines only, not whole hunks** —
  rejected: a finding legitimately quotes the context lines a new branch
  interacts with; hunk-span intersection (including the diff's context
  lines) still excludes every untouched region, which is the creep
  vector.
- **Cap the number of rounds or findings per round** — rejected: a
  numeric cap hides real findings on genuinely messy pushes; the goal is
  convergence by construction (each round can only judge what the push
  changed), not rationing.
- **Deterministic de-duplication against resolved findings by quote
  hash** — rejected as the primary mechanism: fixes rewrite the quoted
  code, so hash matching misses the contradiction class (F-5-2 vs
  F-6-1 quoted different code). Remedy memory in the brief attacks the
  cause; quote hashing stays as the ledger's resolution-check aid.

## Consequences

- Sealed code is sealed by construction at the posting boundary: neither
 a confused nor a compromised model can file on it (extends [ADR-0271](./0271-stellar-review-deterministic-pipeline.md)'s
  untrusted-findings stance from resolutions to findings themselves).
- Fix rounds converge: a push can only be judged on the lines it
  changed, so a PR that stops changing stops accruing findings; the
  154-style unbounded loop is structurally closed.
- The reviewer can no longer flag pre-existing issues in re-scoped
  units. That recall is round 1's job (full scope, unfiltered); a
  known-missed issue can still be raised by a human comment or a
  dedicated full review (`post.py` filters only apply to incremental
  rounds).
- The comment-embedded ledger grows by the capped resolved tail (12
  entries × ~0.5 KB before compression) — negligible against the 48 KB
  budget.
- Later references: [ADR-0271](./0271-stellar-review-deterministic-pipeline.md) (this ADR tightens its decisions 2 and 6;
 [ADR-0271](./0271-stellar-review-deterministic-pipeline.md)'s text is unchanged).
