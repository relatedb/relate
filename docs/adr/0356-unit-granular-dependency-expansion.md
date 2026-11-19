---
type: adr
area: infra
claim: e2d41fc37b4c5239cf3392e90e624e442953bec1
name: 0356-unit-granular-dependency-expansion
status: accepted
tags:
  - review
title: Unit-granular dependency-dirty expansion
REFERENCES:
  - "[[adr/0271-stellar-review-deterministic-pipeline]]"
  - "[[adr/0347-round1-scope-intersects-diff-hunks]]"
---

# ADR-0356: Unit-granular dependency-dirty expansion

## Context

[ADR-0271](./0271-stellar-review-deterministic-pipeline.md)'s expansion contract dirties a whole file when any unit whose
module it imports changed signature: a small change to a foundational
unit (a `CypherValue` variant, a core error type, a trait method) pulls
in every unit of every importing file — one local round this week scoped
850 units for a few small changes. [ADR-0347](./0347-round1-scope-intersects-diff-hunks.md) shrank round-1 scope to
hunk-intersecting units but left the expansion inventory whole-file.

## Decision

The expansion goes unit-granular. A signature-changed unit exposes an
identifier set — its own name plus the capitalized identifiers of its
signature (types, variants, trait names). An importing file joins scope
only for the units whose body references one of those identifiers. The
whole-file fallback stays, conservatively, whenever the heuristic cannot
hold: a truncated unit body (facts caps large units), or a signature
with no extractable identifier (a semantic-only change — same name,
different behavior). The heuristic can only shrink review scope when the
symbol is genuinely nameable; fmt, clippy and tests still gate every
unit regardless, so a missed use site fails a real gate — it merely does
not get a maintainability finding pre-merge.

## Alternatives considered

- Keep whole-file expansion — rejected: the 850-unit round is the
  documented cost; review rounds stop being reviewable at that size.
- Type-checker-driven expansion (real use-def chains) — rejected: no
  rust-analyzer-grade pass in the pipeline; symbol-level matching is the
  honest ceiling without one, with CI as the correctness net.

## Consequences

- Public-API method changes expand to their actual use sites (tens, not
  hundreds); type changes cut to the units that spell the type name.
- Aliased imports (`use x as y`) and generic plumbing can evade the
  match; CI catches those, review does not — accepted.
- `dependency_dirty_files` stays (the fallback and review_files need
  it); the unit set is derived beside it, one place.
