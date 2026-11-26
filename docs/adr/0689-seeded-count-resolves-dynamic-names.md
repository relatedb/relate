---
type: adr
area: engine
claim: ac710b8c91e366a44ff6b4d079cbb291232de68e
date: 2026-11-25
deciders: Relate maintainers
name: 0689-seeded-count-resolves-dynamic-names
status: accepted
tags:
  - executor
  - statistics
title: A seeded count-only body resolves dynamic names and answers from the counts
REFERENCES:
  - "[[adr/0387-statistics-backed-counting-operator]]"
  - "[[adr/0351-cheapest-element-start-for-unbound-patterns]]"
---

# 0689: A seeded count-only body resolves dynamic names and answers from the counts

## Context

[ADR-0387](./0387-statistics-backed-counting-operator.md) answers a
whole-population `count(*)` from the maintained label, relationship-type and
pair counts. Its recognizer declines three things that the per-element count
family needs:

- a dynamic label or type operand (`MATCH (n:$(label)) RETURN count(n)`),
  which the recognizer cannot name statically;
- a bound seed row, because a seeded stage must answer once per seed; and
- a write-routed view, because the counts are exact only for a view with no
  scratch overlay. Every statement that carries a `CALL { … }` subquery is
  classified as write-routed (the access classification is conservative),
  so such a statement runs on a scratch overlay even when it writes nothing.

The common schema-exploration shape is
`CALL db.labels() YIELD label CALL (label) { MATCH (n:$(label)) RETURN count(n) }`
(and the `$(type)` form over `db.relationshipTypes()`). Each body invocation
carries the imported name in its seed, so every decline fires and every
invocation scans the store: one full scan per label or type.

## Decision

A `CALL (vars) { … }` body invocation seeded with exactly one bound row
resolves the body's dynamic names against that row before it runs:

1. every dynamic node-label and relationship-type operand that evaluates to
   a string becomes the static name (a label list becomes the conjunction
   `n:A:B`; a relationship type resolves only for one name), and the
   `WHERE n:$(x)` spelling over a single unlabeled node folds into the
   node's label. An operand that fails to evaluate, an `ANY` list, and a
   GPM-shaped pattern stay dynamic and the scan evaluates them;
2. the resolved body is then served in tier order:
   - **statistics first**: the count-only recognizer of ADR-0387 answers a
     plain `MATCH … RETURN count(…)` through its one count read,
     `CountProbe` over `Graph::windowed_stats_count` (the `Label`,
     `RelType`, `Pair` and total arms) — the same helper labels and
     relationship types already use, so the seeded rung adds no second
     count path. It requires that no pattern variable is already bound in
     the seed;
   - **scan last**: any other shape, a multi-label conjunction, and every
     view the statistics decline (an inexact view, a scratch that has
     already logged a write, an older pinned snapshot) run the ordinary
     clause loop, which now takes the label index and typed adjacency a
     hand-written static pattern takes instead of an all-nodes scan with a
     per-node label test.

No label-presence bitmap serves counts today, so none is consulted: the
label index is a set of node ids that keeps deleted ids until checkpoint GC
(an upper bound, never an exact count), `PresenceBits` has no consumer, and
the memo bitmap is a per-query boolean memo. Introducing an exact label
bitmap is a separate storage decision and would slot between the two tiers.

The exactness window also admits a scratch overlay that has logged no
change: every mutation appends a redo operation, so an empty log means the
view reads exactly the base's pinned state, and the pinned-timestamp and
write-window checks still apply. After the statement's first write the view
declines as before and the scan reads the statement's own writes.

## Alternatives considered

- **A new count-store operator in the plan.** Rejected for this change: the
  maintained counts already exist; the gap is recognition under a seed.
- **Resolving dynamic names in the planner.** Rejected: the name is a runtime
  value of the outer row; only execution knows it.

## Consequences

- Per-label and per-type count subqueries cost one count lookup per element
  instead of one store scan per element; PROFILE runs still take the plan
  they describe, as for the ADR-0387 shortcut.
- Results are byte-identical to the scan; the equivalence is pinned by tests.
- Deferred: `db.schema.visualization()` counting, degree and targeted-degree
  statistics, and feeding the statistics into estimates.
