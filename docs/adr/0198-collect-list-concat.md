---
type: adr
area: cypher
date: 2026-10-09
deciders: CypherLite maintainers (agreed with live Neo4j CE 2026.09.0 evidence)
name: 0198-collect-list-concat
status: accepted
tags:
  - semantics
title: "collect + list in WITH: `WITH *` grouping keys and aggregates in CASE arms"
REFERENCES:
  - "[[adr/0188-remove-superseded-tck-scenario-from-pin]]"
---

# 0198. collect() + list in WITH: `WITH *` grouping keys and aggregates in CASE arms

## Context

LABS-199 reports that `collect(x) + [y]` in a single `WITH` fails. The report
was investigated with a 24-shape battery (s01–s21 valid, i01–i03 invalid;
script and results in this branch's git history, commits 780b0343 and
f5b0c119, and in the LABS-199 verdict comment) run against the v0.11.5
release binary and HEAD (96df497f), plus live probes against Neo4j CE
2026.09.0 (`neo4j:2026.09-community`) as the dialect ground truth.

The literal repro **does not reproduce**: the reported shapes pass on both
v0.11.5 and HEAD. For example s01,
`MATCH (n) WITH collect(n.x) + [3] AS xs RETURN xs`, returns
`{"xs":[1,2,3]}` on both binaries.

The battery and probes did find two genuine dialect divergences from Neo4j
ground truth:

1. **s20 — `WITH *` does not register grouping keys for aggregate-containing
   expressions.** On the seed `(a {x:1,y:10}), (b {x:2,y:10})`, the query

   ```cypher
   WITH 99 AS z MATCH (n) WITH *, collect(n.x) + [z] AS xs RETURN xs
   ```

   fails on CypherLite v0.11.5 and HEAD with
   `syntax error: AmbiguousAggregationExpression: non-aggregated expression
   must be a grouping key`. Neo4j CE 2026.09.0 (live probe) accepts it and
   returns two rows, `xs=[1,99]` and `xs=[2,99]`: `WITH *` expands to all
   in-scope variables, which become the grouping keys (one group per carried
   row). CypherLite already accepts the explicitly projected form
   `WITH z, collect(n.x) + [z]` (battery s04 passes), so the gap is specific
   to `*` expansion.
2. **p-case — aggregates inside CASE arms are rejected.** The query

   ```cypher
   MATCH (n) WITH CASE WHEN true THEN collect(n.x) + [3] ELSE [] END AS xs RETURN xs
   ```

   fails on CypherLite HEAD with
   `semantic error: collect() is an aggregating function`. Neo4j CE
   2026.09.0 (live probe) accepts it and returns `xs=[1,2,3]`.

Context that is **not** decided here: the rejections CypherLite and Neo4j
agree on stay unchanged — an unprojected carried variable and an unprojected
property leaf in an aggregate-containing expression (battery i01–i03) are
GQLSTATUS 42I18 on Neo4j's side, and a same-clause alias reference is 42N62.
CypherLite's error-class mappings for these stay as-is.

## Decision

**When a `WITH`/`RETURN` projection combines `*` with aggregating
expressions, every variable carried by `*` is a grouping key. An aggregate
function call in any ordinary expression position of an aggregating
projection — including a CASE arm — belongs to that projection.** Two
rulings, one per fixed shape; no other dialect changes.

1. **`WITH *` registers grouping keys (s20).** When a `WITH`/`RETURN`
   projection combines `*` with aggregating expressions, every variable
   carried by `*` is a grouping key, so non-aggregated leaves inside
   aggregate-containing expressions (such as `z` in `collect(n.x) + [z]`)
   resolve against them. This matches Neo4j's expansion of `*` to all
   in-scope variables and the openCypher grouping model: one group per
   carried row, exactly as when the variable is projected explicitly.
2. **Aggregates in CASE arms belong to the enclosing projection (p-case).**
   An aggregate function call inside a CASE branch — or any other ordinary
   expression position, such as a list literal (already supported) — belongs
   to the enclosing aggregating projection. Validation must walk these
   positions when classifying aggregate calls instead of falling back to the
   scalar-expression path that rejects them.

Both rulings are against Neo4j CE 2026.09.0 live-probe ground truth and are
pinned by the evidence above: LABS-199, the battery results (commits
780b0343/f5b0c119 in this branch's git history), and the probe results. The
durable behavior pins are `testdata/cypherlite-tck/CollectListConcat.feature`.

## Alternatives considered

- **Keep rejecting both shapes.** Rejected: the rejections diverge from
  Neo4j CE 2026.09.0 ground truth and from the openCypher grouping model
  (carried variables are grouping keys whether projected by name or by
  `*`, and aggregate calls group wherever they appear in the projection).
  The pinned TCK corpus does not cover these shapes, so rejection buys no
  conformance — it only keeps a known wrong answer.
- **Special-case only list concatenation with `+`.** Rejected: the defect is
  in grouping-key resolution (s20) and aggregate-position classification
  (p-case), not in the `+` operator. A narrow patch on list concatenation
  would leave the same bug in every other expression position — arithmetic,
  CASE arms, function arguments — and the next battery finds it again.
- **Align error classes to GQLSTATUS 42I18/42N62 as part of this change.**
  Rejected: out of scope. CypherLite's error kinds for the agreed rejections
  (`AmbiguousAggregationExpression`, `UndefinedVariable`, same-clause alias)
  already map to the same semantics; changing error classes is a separate
  decision that would touch every surface's error contract at once.

## Consequences

- Implementation nodes fix the validator/executor per the two rulings:
  `WITH *` expansion registers the carried variables as grouping keys, and
  aggregate-call validation walks CASE arms (and other ordinary expression
  positions) of an aggregating projection.
- Both shapes are pinned in the CypherLite TCK extension corpus as
  `testdata/cypherlite-tck/CollectListConcat.feature`. Per that corpus's
  README, the scenarios run through the same strict `run_tck_directory`
  runner as the official corpus but are not part of the 3892-scenario
  pinned denominator, the skip list, or the pass allowlist.
- The agreed rejections (i01–i03: unprojected carried variable, unprojected
  property leaf, same-clause alias) stay rejections with unchanged error
  kinds; the literal LABS-199 shapes (s01–s19, s21) keep passing.
- No on-disk format change and no public-API change: this is a query-language
  acceptance change in the validator/executor only, so no client surface or
  `docs/api/<surface>/` update is required.
- The TCK floor must not regress: 3892 of 3892 pinned scenarios passing with
 zero skips ([ADR-0188](./0188-remove-superseded-tck-scenario-from-pin.md)), and the CIP-eval baselines
  (`baselines/cip-eval.json`) ratchet up only.

## Later references

The audit of this feature branch found a silent wrong-answer regression in
the ruling-1 implementation: `MATCH (n:N) WITH *, count(*) AS c GROUP BY
n.x RETURN n.x, n.y, c` grouped by the explicit keys only, yet still
projected the star's carried non-key variables with arbitrary
first-row-of-group values (the zero-aggregate form `WITH *, n GROUP BY n`
takes the same executor path, since an explicit GROUP BY acts as
DISTINCT). Live Neo4j 2026.09.0 rejects the aggregate form with GQLSTATUS
42I18 ("reference to non-grouping sub-expression"). Ruling: a `*`
projection item under an explicit GROUP BY key list or the empty grouping
set (`GROUP BY ()`) stays rejected — the shape errored before this branch
("* is not a scalar expression") and errors now, raised at validation time
in the planner (`check_agg_projections`,
`cypherlite-planner/src/agg.rs`) as `AmbiguousAggregationExpression`, for
both the aggregate and the zero-aggregate forms. `GROUP BY ALL` is
unaffected: lowering places the star into the key list and the executor
expands it to the carried variables there.
