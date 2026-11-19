---
type: adr
area: cypher
date: 2026-10-11
deciders: "CypherLite maintainers (PR #235 follow-up, apoc PR #105)"
name: 0291-graph-examples-extras-and-schema-ddl
status: proposed
tags:
  - conformance
title: Graph-examples extras — apoc rewrites as extra queries — and schema DDL as runnable statements
REFERENCES:
  - "[[adr/0081-opt-in-load-csv-file-capability]]"
  - "[[adr/0144-graph-examples-differential-suite]]"
  - "[[adr/0176-graph-examples-report-only-nightly]]"
---

# 0291. Graph-examples extras — apoc rewrites as extra queries — and schema DDL as runnable statements

- **Renumbered**: from 0200 (shared-number cleanup)
- **Related**: [ADR-0144](./0144-graph-examples-differential-suite.md) (refined), [ADR-0176](./0176-graph-examples-report-only-nightly.md), [ADR-0081](./0081-opt-in-load-csv-file-capability.md)

## Context

[ADR-0144](./0144-graph-examples-differential-suite.md)'s differential suite skips every `apoc.*` statement with the
reason "blocked until the apoc PR lands" — today PR #105 (the optional
apoc implementation) is still open, so 24 apoc call sites (13 distinct
functions) across 8 of the 18 datasets never execute on either engine.
Meanwhile Cypher 25 (the reference server is neo4j:2026.09) grew native
replacements for several of those functions — `coll.max`/`coll.min`
list functions, correlated `CALL (x) {}` subqueries, `count {}` pattern
subqueries — so a growing subset of the skipped walkthroughs is
expressible in plain Cypher and testable **today**, without waiting for
apoc.

Separately, the suite's `DIALECT_PREFIXES` classified `CREATE INDEX` /
`CREATE CONSTRAINT` as "schema administration is out of the dialect",
skipping the walkthroughs' schema DDL (33 statements across the
datasets). That is stale: the engine runs and **enforces** both forms
(`CREATE CONSTRAINT … IF NOT EXISTS`, `CREATE INDEX … IF NOT EXISTS`,
and the old `create index on :Label(prop)`) — a uniqueness violation
raises `ConstraintValidationFailed`.

An inventory of the 13 distinct apoc functions against Cypher 25
(verified statement-by-statement against the built CLI and the pulled
stores) splits them three ways:

- **Rewritable today** — `apoc.coll.max`/`coll.min` (→ `coll.max`/
  `coll.min`), `apoc.node.degree.in` (→ `count { (n)<-[:T]-() }`),
  `apoc.meta.stats` / `apoc.cypher.run` dynamic-label counts (→
  `db.labels()` + correlated `CALL (x) {}`, `type(r)` counts,
  `labels(n)` membership), `apoc.agg.statistics` (→
  `min`/`max`/`avg`/`stDev`/`percentileCont`/`count` columns),
  `apoc.periodic.iterate` write (→ the plain `MATCH … SET`; the `-50`
  stores are samples, batching is irrelevant).
- **Blocked on an engine gap** — `apoc.date.format` (→
  `toString(datetime({epochMillis: t}))` + string slicing): the
  `epochMillis` constructor field is ignored today (LABS-261).
- **Not rewritable by design** — `apoc.algo.dijkstra` (weighted
  shortest path has no plain-Cypher equivalent; QPE `SHORTEST` is
  hop-based), `apoc.load.json` / `apoc.import.json` (remote fetch is
 denied by the LOAD CSV policy, [ADR-0081](./0081-opt-in-load-csv-file-capability.md)), `apoc.util.sleep` (a test
  throttle with no differential meaning), and `apoc.coll.intersection`
  in entity-resolution (the rewrite `[x IN l WHERE x IN r]` works, but
  the surrounding statements call `gds.alpha.similarity.*`, so the
  whole block stays GDS-skipped).

## Decision

1. **Extras**: the suite gains an `extras` module — plain-Cypher
   rewrites of the skipped apoc statements, one per rewritten adoc
   block, keyed by the original adoc line. They run differentially
   (both engines, same comparison as adoc statements) after every adoc
   statement of the dataset, so the extras' writes (the
   `apoc.periodic.iterate` rewrite) cannot shift any adoc comparison.
   Extras carry `apoc: false`: they are plain queries, not part of the
   apoc capture.
2. **Coverage policy**: rewrite only what plain Cypher 25 expresses
   **and** the engine runs today — probed against the built CLI before
   an extra is added. The blocked and not-rewritable functions are
   documented in the module with their reasons (LABS-261 for
   `date.format`); their extras join when the blocker lands.
3. **Schema DDL runs**: `CREATE INDEX` / `CREATE CONSTRAINT` leave
   `DIALECT_PREFIXES` and classify as plain statements. A syntax the
   reference accepts but the engine rejects surfaces as a DIFF, not a
 skip — the skip list only shrinks ([ADR-0144](./0144-graph-examples-differential-suite.md)'s contract).
4. **Apoc statements stay skipped** until PR #105 merges; then, with
   `CYPHERLITE_GRAPH_EXAMPLES_APOC=1`, they run on both engines. The
   extras do not replace that — they are the apoc-free ground truth the
   implementation is additionally tested against, and they keep running
   after apoc lands (a regression in `coll.max` or `count {}` must
   still fail the walkthrough shape).

## Alternatives considered

- **Run the apoc statements against the reference only** — rejected:
   a one-engine result has nothing to compare against; the suite's
   whole value is the differential comparison.
- **Rewrite the adoc walkthroughs in place** (edit the adoc files) —
   rejected: the adocs are the datasets' published documentation,
   downloaded sha-pinned from the source repos; rewriting them in the
   cache would desynchronize the suite from the documented product and
   break the sha pinning.
- **Add extras for every skipped statement, including GDS** —
 rejected: GDS algorithms are a different product ([ADR-0144](./0144-graph-examples-differential-suite.md)'s original
   scope decision); only the apoc subset that plain Cypher replaces is
   in scope.
- **Treat `CREATE INDEX`/`CREATE CONSTRAINT` as a new skip reason
   keyed on the reference's acceptance** — rejected: the engine already
   accepts and enforces the DDL, so a skip would hide working coverage;
   running it is strictly more signal.

## Consequences

- `fraud-detection`, `contact-tracing`, and `twitch` gain extras (4 +
  5 + 3 rewritten statements); their reports grow by those rows, keyed
  to the original adoc lines.
- The dialect skip count shrinks by the schema-DDL statements
  (~13 statement-level skips across entity-resolution,
  network-management and the second-wave datasets); those statements
  now exercise both engines' schema DDL differentially.
- When PR #105 merges, flipping `CYPHERLITE_GRAPH_EXAMPLES_APOC=1` in
  the nightly runs the real apoc statements **and** the extras in the
   same report — the implementation is tested against both Neo4j's apoc
   output and the plain-Cypher rewrite.
- LABS-261 (`datetime({epochMillis})` ignored) is the tracked blocker
  for the two `apoc.date.format` rewrites; when it lands, its extras
  join the module.
- New extras require a probe against the built CLI first (the
  coverage policy); a failing probe means the function is not
  rewritable yet, not that the extra ships red.
