---
type: adr
area: engine
claim: 0c11dae5e09dd5a91d97f096d708f70affc47033
name: 0666-specialized-path-change-verification
status: accepted
tags:
  - testing
  - conformance
  - code-health
title: Specialized execution paths change under a two-sided verification duty
REFERENCES:
  - "[[adr/0209-batched-global-bulk-writes]]"
  - "[[adr/0482-spec-harness-coverage-lane]]"
  - "[[adr/0520-move-cypher-tests-to-spec-harness]]"
  - "[[adr/0661-spec-harness-regression-gate]]"
---

# 0666 — Specialized execution paths change under a two-sided verification duty

Date: 2026-11-23

## Status

Accepted

## Context

The engine answers most statements through a general executor, and beside it
runs a family of **specialized paths** — batched auto-commit writes
(ADR-0209), match folds, count/exists probes, the chain and hub-star
kernels, the scan-predicate gate, the frontier engines — each of which
re-derives, for a recognized statement shape, rows the general path also
computes. An external correctness review of the engine (2026-11, 66
findings, 25 of them wrong results) traced the largest class of silent
wrong answers — including silent data loss reaching writes — to exactly
these paths: the specialization's recognition scope drifts from the
general path's semantics (an unnamed factor the write does not target, a
second `MATCH` clause's scope, a second occurrence of a repeated
variable), and the specialized answer diverges with no error. The general
path was right in every case.

Two properties of the class make it resistant to the existing gates:

- The divergence is usually **shape-gated, not data-gated**: a
  batched-write bug appears only above the batching threshold, so
  small fixtures pass while a large store silently corrupts.
- The specialized paths' own unit tests pin the recognizer's intent,
  which is precisely what drifts: the test and the bug agree with each
  other and both disagree with the language.

The spec harness (ADR-0482, ADR-0520) already runs the language contract
per pull request, and ADR-0661 added the id-level regression gate that
fails a pull request on a scenario the engine-main baseline does not
already carry as failing. What was missing is the rule that ties a
*change to a specialized path* to both verification sides up front.

## Decision

A change that adds or modifies a specialized execution path — a new
recognizer, a widened or narrowed eligibility, a fold, kernel, gate or
batched arm — carries, in the same change:

1. **An engine-side regression test** in this repository, built around a
   statement whose answer the change could silently alter, with the
   specialization actually engaged (forced through the path's own seam —
   the batching env knobs, a pattern-engine override, the A/B hooks —
   not by accidentally sitting above a size threshold) and a control
   spelling that takes the general path and must agree. Both pattern
   engines (`RELATE_PATTERN_ENGINE=auto|nfa|walk` where the shape
   allows) must agree on the answer.
2. **A spec-harness scenario** in the cypher compat repository for every
   user-visible behavior the path decides — the engine's language
   contract lives there (ADR-0520), and a scenario-level pin is what the
   ADR-0661 gate ratchets for every later pull request. Expectations are
   pinned to live Neo4j, never to the engine's current answer.

The duty is symmetric on purpose: the Rust test fails at build time in
this repository, the harness scenario fails in every future pull request
that regresses the behavior. One side alone has a known hole — the unit
test can share the recognizer's mistake, the harness lane can be
skipped or baseline-muted.

When a specialized path's premise breaks and the general path already
answers the shape correctly, **declining the specialization is the
preferred fix** over patching the specialized arm: a decline costs
performance only, and removes a divergence surface. The 2026-11 fixes
for the batched `DELETE` (an empty unnamed factor), the multi-clause
existence probe and the repeated-variable scan gate are the template:
two declines and one eligibility narrowing, each with the recognition
rule stated once in one home.

## Consequences

- Review of specialized-path changes checks for the pair: no scenario +
   no Rust test = incomplete change; a fix that only patches the
   specialized arm where a decline exists gets asked for the decline.
- The bulk-batch env seam (`RELATE_BULK_BATCH_THRESHOLD`,
   `RELATE_BULK_BATCH_BUDGET`) and the pattern-engine override are
   load-bearing test infrastructure: changing their spelling is a
   breaking change to the verification duty and updates every caller.
- Specialized paths without a forcing seam (folds, kernels, gates)
   owe one: the duty cannot be met by fixtures that reach the path only
   above a size threshold.
- The external-review corpus that motivated this ADR lands as owned
   scenarios in the compat repository's correctness suite; findings
   still open stay as scenarios the ADR-0661 baseline records as
   failing, so every family fix flips its scenarios to passing and the
   gate protects it thereafter.

## Later references

_None yet._
