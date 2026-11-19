---
type: adr
area: cypher
date: 2026-09-16
deciders: CypherLite / MemoryDB merge
name: 0046-cypherlite-parser-memorydb-runtime
status: accepted
tags:
  - parser
title: CypherLite parser in front of the MemoryDB runtime
REFERENCES:
  - "[[adr/0061-ci-host-loadtest-baselines]]"
  - "[[adr/0066-retire-memorydb-naming]]"
  - "[[adr/0070-docker-baseline-loadtest-environment]]"
  - "[[adr/0071-ci-owned-loadtest-freeze-fingerprint]]"
  - "[[adr/0116-fixed-namespaces-community-edition-semantics]]"
  - "[[adr/0119-cip-eval-drops-enterprise-database-scenarios]]"
  - "[[adr/0277-one-cypher-parser]]"
---

# 0046. CypherLite parser in front of the MemoryDB runtime

## Context

The merge target (`cypherlite/`) keeps CypherLite’s dialect and UX and MemoryDB’s WAL + CSR + planner/executor. Growing `memorydb-parser` for QPP would duplicate a parser that already exists in `cypherlite-parser`. Grafting a WAL onto CypherLite `GraphStore` HashMaps would abandon MemoryDB’s write path.

## Decision

1. **Front-end.** `Database::query` uses `cypherlite-parser` + `memorydb-front`. The historical parser is available only with `MEMORYDB_PARSER=memorydb` for diagnostics; it is not a product query path.
2. **TCK floor.** MemoryDB TCK remains 100% (pin 2024.3, 0 skips). CypherLite’s 3 TCK skips are not imported into that gate.
3. **CIP-eval stretch.** Source `cyperlite-original` cip-eval is the language baseline (pass counts only ratchet up). In-tree MemoryDB-backed cip-eval uses `baselines/cip-eval-memorydb.json` until it matches that baseline.
4. **Loadtests.** Fraud / identity / checkpoint must not regress vs frozen `baselines/` summaries (`errors == 0`, ops/s ≥ 90%, p99 ≤ 115%, logical bytes ≤ 110%).
5. **Tier 2 theater.** `SHOW ROLES/SERVERS/USERS`, `CREATE DATABASE`, composite `USE` are documented deviations, not silent fails. User docs may link this ADR.
6. **Contract.** `AGENTS.md` in this tree is the working contract (copied from MemoryDB, updated only via ADR).

## Alternatives considered

- Extend `memorydb-parser` for QPP — rejected (duplicate front-end).
- WAL under `GraphStore` JSON — rejected (wrong storage).
- Replace CypherLite GitHub Actions with MemoryDB’s smaller `ci.yml` — rejected; extend CypherLite workflows.

## Consequences

- New dialect/runtime behavior needs an ADR here plus `docs/api/` when it is a public surface.
- `memorydb-parser` is no longer the query front-end after the lowerer covers the TCK subset.

## Later references

- The diagnostic-parser env var in Decision item 1 was renamed: it is
  `CYPHERLITE_PARSER` (e.g. `CYPHERLITE_PARSER=memorydb`), not
  `MEMORYDB_PARSER`. See `AGENTS.md` ("Cypher dialect") and
  `crates/cypherlite-front`. The Decision text above still carries the old
  name; this note is the correction.
- Canonical query-language registries are layered under this ADR's
  front-end design, and `cypherlite-planner` now depends on
  `cypherlite-front` to bind against them (dependency direction: planner →
  front → ir; no cycle). Ownership: `cypherlite-ir` owns the aggregate-name
  set (`AGGREGATES`, matched by `is_agg_fn`/`is_agg_expr`);
  `cypherlite-front` owns the SHOW-column tables (`show_columns`), the
  known-function registry (`KNOWN_FUNCTIONS`/`is_known_function`), and the
  aggregate alias/display table (`agg`, which re-exports the ir set); the
  executor owns the procedure signature registry (`procedure_registry`).
- [ADR-0061](./0061-ci-host-loadtest-baselines.md): the loadtest gate (Decision item 4) now runs against two host
  classes. Repo-root `baselines/` remains the local reference-host
  comparison set; CI compares against `baselines/ci/`, seeded from CI runner
  artifacts. The bands (errors == 0, ops/s ≥ 90%, p99 ≤ 115% + 250,000 ns,
  logical bytes ≤ 110%) are unchanged on both host classes.
- [ADR-0070](./0070-docker-baseline-loadtest-environment.md): the two-host-class split is superseded. The loadtest gate
  (Decision item 4) freezes and compares fraud/identity/checkpoint in a
  Docker baseline environment (2 vCPU / 2 GB). One comparison set:
 repo-root `baselines/`. The [ADR-0046](./0046-cypherlite-parser-memorydb-runtime.md) bands themselves are unchanged.
- [ADR-0071](./0071-ci-owned-loadtest-freeze-fingerprint.md): committed loadtest envelopes are produced by CI on
  `ubuntu-24.04` and carry an `environment` class. Wall-clock bands apply
  only when that class matches.
- [ADR-0066](./0066-retire-memorydb-naming.md): the diagnostic parser value in Decision item 1's env var was
  `CYPHERLITE_PARSER=planner-ir` (was `=memorydb`), and the CIP scoreboard is
  `baselines/cip-eval.json` (was `baselines/cip-eval-memorydb.json`). The
  MemoryDB name is retired from live surfaces; the runtime work imported here
 is unaffected. [ADR-0277](./0277-one-cypher-parser.md) withdraws that diagnostic parser.
- [ADR-0277](./0277-one-cypher-parser.md): `CYPHERLITE_PARSER` is not read. `cypherlite-ir` keeps the planner
  IR AST and no longer parses; `parse_and_lower_statement` is
  `cypherlite-parser` plus `lower_statement`.
- [ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md): the Tier-2 theater deviations of Decision item 5 are superseded.
  Every store has the three fixed namespaces `neo4j`, `system` and
  `system_ontologies`; `USE` routes a statement and unknown names fail
  (42N00); `CREATE DATABASE`, composite `USE` and every other unsupported
  database administration command fail with 51N27 instead of recording
 metadata rows; `SHOW DATABASES` lists the three fixed rows. [ADR-0119](./0119-cip-eval-drops-enterprise-database-scenarios.md)
  removes the 24 Enterprise-only CIP-eval scenarios this invalidates from
  the frozen set.
