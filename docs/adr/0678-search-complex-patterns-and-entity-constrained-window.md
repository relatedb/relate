---
type: adr
area: cypher
claim: 9812a780d7421287c3a5987f3028c5cad9d7c34a
date: 2026-11-24
name: 0678-search-complex-patterns-and-entity-constrained-window
status: proposed
tags:
  - dialect
title: |-
  The SEARCH clause's complex patterns and entity-constrained window join the dialect
REFERENCES:
  - "[[adr/0371-preview-feature-gating]]"
  - "[[adr/0467-preview-gates-chronological-and-complex-search]]"
  - "[[adr/0256-vector-search-flat-scan-rewrite]]"
SUPERSEDES:
  - "[[adr/0467-preview-gates-chronological-and-complex-search]]"
---

# 0678: The SEARCH clause's complex patterns and entity-constrained window join the dialect

## Context

The SEARCH clause landed with a plain-pattern floor and one preview row
lifting it. The floor
([ADR-0467](./0467-preview-gates-chronological-and-complex-search.md)'s
`complex-search-patterns`): a searching MATCH binds one variable, in a
single node or single-hop relationship pattern, with predicates only on
the bound element; the preview row skips that shape family. The window
rule keyed on the same classification: a *plain* pattern's LIMIT was a
hard candidate window — its outer WHERE, labels and pre-bound values
post-filtered the window without freeing slots — while a *complex*
pattern's LIMIT counted the candidates the whole MATCH kept, so its
window grew until the count was met or the index ran out.

The 2026-11-24 acceptance-corpus sync moved the reference
implementation past all of that:

- The complex-pattern suites lost their opt-in configuration tag and the
  shape-rejection scenarios flipped to expecting results — multi-component
  patterns, variable-length and quantified hops, several bound variables,
  predicates and labels outside the binding are default behaviour now.
- New rows pin the window semantics for *every* shape: the MATCH
  statement's own constraints — label and property predicates, the outer
  WHERE, a pre-bound binding value — restrict the candidate set BEFORE
  the SKIP/LIMIT window, while a constraint the index definition already
  implies (its own labels or types, a superset of them) restricts
  nothing. A WHERE-passing candidate ranked below the raw top-K is
  returned; the K counts entities satisfying the pattern.
- The inner WHERE of the source specification serves FULLTEXT sources
  too, and its predicate family widens beyond per-property comparisons:
  the entity-set match filter (`<binding> IN <expr>`), the id-membership
  form (`id(<binding>) IN <expr>`), and a right-hand side that is a
  correlated `COLLECT` subquery evaluated per input row.
- An index created inside the open transaction is not searchable there:
  the reference raises a transient still-populating error until the
  transaction commits.

## Decision

1. **Graduate the shape family.** The `complex-search-patterns` registry
   row is deleted and the pattern-shape rejections with it: a searching
   MATCH keeps every pattern shape the general MATCH machinery accepts,
   ungated. The rules that hold for every shape — the binding-variable
   reference, the per-index kind×option compatibility, the search-query
   and control self-reference rules — are unchanged; the
   LIMIT-reads-pattern rejection is restated as the window-reads-pattern
   rejection (`SearchClauseWindowReadsPattern`), covering the SKIP
   expression as well as LIMIT.

2. **One window rule for every shape.** SKIP and LIMIT page over
   *entities* that satisfy the whole MATCH statement — the pattern, the
   outer WHERE, and a pre-bound binding value — best-first by score. The
   existing growth loop widens the fetched window until skip+limit
   survivors exist or the index is exhausted; the plain/complex split and
   the `complex_pattern` flag it keyed on are gone. Constraints the index
   definition implies filter no index member, so they change nothing —
   the growth loop reaches that outcome by construction, not by a
   special case.

3. **The inner WHERE serves both index kinds and a wider predicate
   family.** FULLTEXT sources accept the inner WHERE (the old
   WHERE-with-FULLTEXT rejection is deleted; SKIP/OFFSET and WITH
   ANALYZER stay vector-rejections and SKIP stays fulltext-only). The
   predicate grammar adds the id-membership form beside the entity-set
   match filter, and a right-hand side may be any expression over the
   incoming row, a correlated `COLLECT` subquery included. The filter
   applies before the window on both kinds — single-stage, the same
   place the scan scores candidates.

4. **An index created in the open transaction is populating.** SEARCHing
   an index whose creation is part of the current explicit transaction
   fails with the transient still-populating error naming the index;
   once the transaction commits the index is searchable. Nothing else
   about index resolution changes: a missing index, a wrong kind, a
   wrong dimensionality keep their rejections.

## Alternatives considered

- **Keep the gate and absorb the new rows as expected failures.**
  Rejected: the corpus is live-derived ground truth and now pins the
  graduated behaviour on both sides — the shape-rejection scenarios were
  rewritten to expect results, so the off state the gate preserved no
  longer exists upstream to model.
- **Keep the plain/complex window split, widening only the plain side.**
  Rejected: the new rows pin the unified semantics for plain patterns —
  a plain single-node pattern with an outer WHERE returns a
  WHERE-passing candidate ranked below the raw top-K — so the split's
  plain side (a hard window) contradicts the corpus exactly where the
  complex side already implements the wanted rule.
- **Post-filter the entity-membership predicates after the window.**
  Rejected: single-stage placement is the observable contract — LIMIT
  counts the entities satisfying the filter — and the scan already
   places its per-property predicates there; a second placement for the
  new predicates would be two policies for one clause.

## Consequences

- The preview registry loses its row; the two gate test binaries are
  replaced by default-on pins in `relate/relate/tests/`, and the front's
  shape validation, its classifier and the IR's `complex_pattern` flag
  are deleted. A `RELATE_PREVIEW=complex-search-patterns` spelling now
  fails the open with the unknown-name usage error — the graduated name
  is gone, the registry's documented end state for a shipped row.
- The front keeps one SEARCH validation battery; the executor's two scan
  paths share the growth policy without the shape flag, and the fulltext
  scan threads the inner filter into its scoring pass.
- `docs/reference/cypher-syntax.md`'s SEARCH section is rewritten: the
  window rule, the inner-WHERE predicate family, both index kinds, no
  opt-in.
- The vector, complex-pattern, single-stage and match-mode SEARCH suites
  of the acceptance corpus turn green with this change; the fulltext
  suite's remaining failures are its procedure family, not the SEARCH
  clause.

## Later references

- [ADR-0714](./0714-fulltext-search-where-is-the-match-filter.md)
  supersedes this record's "the inner `WHERE` serves both kinds" line:
  a 2026-11 re-probe of live Neo4j split the predicate families by index kind —
  a fulltext window's inner `WHERE` is the match filter only.
