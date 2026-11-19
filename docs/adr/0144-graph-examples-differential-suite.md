---
type: adr
area: cypher
date: 2026-09-30
deciders: CypherLite maintainers (agreed in conversation)
name: 0144-graph-examples-differential-suite
status: proposed
tags:
  - conformance
title: Graph-examples differential Cypher suite (Neo4j EE docker reference)
REFERENCES:
  - "[[adr/0080-cli-pull-command]]"
  - "[[adr/0158-streaming-single-process-pull]]"
  - "[[adr/0176-graph-examples-report-only-nightly]]"
  - "[[adr/0291-graph-examples-extras-and-schema-ddl]]"
---

# 0144. Graph-examples differential Cypher suite (Neo4j EE docker reference)

## Context

The TCK (3891 scenarios) is the language floor and cip-eval is the stretch
scoreboard, but both are synthetic. The neo4j-graph-examples sandbox datasets
are real, publicly documented graphs with walkthrough queries in their
`documentation/<name>.adoc` files, backed by Neo4j 5.x dumps
(`data/<name>-50.dump`). They cover query shapes the TCK corpus does not
exercise against real, densely populated data.

Testing cypherlite against them needs (a) the data, (b) an reference for what
the queries should return, and (c) a copy of the data inside cypherlite.
Neo4j itself is the natural reference: each dump loads into a Neo4j EE docker
container (enterprise for multi-database, one named database per use-case),
and `cypherlite pull` ([ADR-0080](./0080-cli-pull-command.md), Query API v2) already copies a Neo4j
subgraph into a `cypherlite::Database`.

Dumps are ~10 MB each — too large for git. The adoc queries mix plain
Cypher, `gds.*` (graph data science — out of scope), and `apoc.*`/apocf
functions (blocked until the apoc PR lands and merges).

## Decision

Add a differential suite as a new testing crate,
`testing/cypherlite-graph-examples`:

- **Datasets (core 5, data-driven)**: movies, recommendations, northwind,
  citations, pole. A checked-in manifest maps each dataset to its
  `data/<name>-50.dump` download URL + sha256 and its
  `documentation/<name>.adoc` URL. Dumps are fetched on demand into a
  gitignored cache dir, sha-verified; the suite skips (cleanly, with the
  reason reported) when the cache is cold and network/docker are absent.
  More datasets are manifest rows, not code.
- **reference**: one `neo4j:5.26-enterprise` docker container per suite run
  (license-accept env), one database named after each dataset. Dumps load
  through one-off `neo4j-admin database load` containers writing into a
  shared docker volume **before** the server boots — loading while the
  server runs leaves the store quarantined (`quarantine_marker`) and it
  never comes online. After boot, `CREATE DATABASE IF NOT EXISTS`
  registers each pre-loaded store and an online-poll gates the run. The
  suite self-skips when the gate env is unset; a gated run with docker
  missing fails loudly instead of passing vacuously.
- **Ingest into cypherlite**: end-to-end through the real `cypherlite pull`
  binary (env-file with the Query API v2 endpoint of the docker container,
  database = the dataset dir; shape query `MATCH (n) RETURN n UNION MATCH
  ()-[r]->() RETURN r`), verifying node/rel counts against Neo4j.
- **Query extraction/classification**: parse `[source,cypher]` blocks from
  the adoc. Each statement is classified `plain`, `gds` (`gds.` — excluded,
  the GDS sandbox queries are a different product), `apoc` (`apoc.`/apocf —
  gated behind an on-demand flag, env `CYPHERLITE_GRAPH_EXAMPLES_APOC=1`,
  default off until the apoc PR merges), or `dialect-skip` (PROFILE/EXPLAIN,
  multi-database `USE`, administration commands) with a recorded reason.
- **Differential run**: each plain query runs on the Neo4j database and on
  the pulled cypherlite copy; results are compared after normalization —
  bag comparison (order-insensitive) unless the query has ORDER BY, Cypher
  type mapping, float tolerance, temporal formatting alignment.
- **Reporting**: a per-query pass/fail/skip report plus a committed
  baseline of known diffs (cip-eval-style ratchet: regressions fail, new
  passes must update the baseline). The suite never claims compliance from
  partial results.
- **CI**: a separate opt-in path — the main matrix does not pay for docker
  image pulls or dump downloads. The suite runs in a **nightly** workflow
  (cron + manual dispatch) on a docker-capable runner: it builds the CLI,
  sets `CYPHERLITE_BIN`, runs with the on-suite env gate, caches the dump
  dir, and uploads the per-query report as an artifact.

## Alternatives considered

- **Run adoc queries only against a fresh cypherlite graph built from the
  dump via the engine** — rejected: parsing Neo4j's dump format is a new
  brittle surface; pull already exists for exactly this extraction.
- **Library-level pull (call `cypherlite-load` directly)** — rejected for
  the suite: the end-to-end `cypherlite pull` binary path is the product
  surface agents will actually use; the library stays covered by its own
  unit/integration tests.
- **Commit dumps into the repo** — rejected: ~10 MB each across a growing
  manifest; git-bloat and licensing hygiene. On-demand cached fetch with
  sha pinning keeps runs reproducible.
- **Treat GDS/apoc queries as failures** — rejected: GDS is a different
  product (excluded per task); apoc is coming via the apoc PR and needs an
  on-demand gate, not a permanent skip.
- **One PR for all ~15 datasets** — rejected: core 5 proves the harness
  cheaply; the rest are manifest rows later.

## Consequences

- A third testing axis: TCK floor (pinned scenario IDs), cip-eval stretch
  (ratchet), graph-examples differential (real-data reference) — each with its
  own baseline and skip contract.
- The suite depends on docker + network + the public GitHub datasets at run
  time; it self-skips with reasons when those are absent, so local and CI
  runs stay deterministic.
- Neo4j result shapes (e.g. `POINT`/temporal serialization, list ordering
  nuances) surface as normalization work; unknown-normalization rows land
  in the baseline as "investigate" rather than silently passing.
- A new `testing/` crate only (R4/R5 boundary rules); no engine or surface
  code changes except any `pull` fixes the suite uncovers (those land as
  their own fixes with tests).

## Later references

- [ADR-0158](./0158-streaming-single-process-pull.md) supersedes this ADR's manual paging (`ORDER BY elementId` + `SKIP`/`LIMIT` + carried endpoints): the suite pulls each dataset with one multi-statement `--query` in one process.
- [ADR-0176](./0176-graph-examples-report-only-nightly.md) supersedes this ADR's Reporting decision: the nightly is report-only — diffs are reported, only infrastructure failures fail the run.
- [ADR-0291](./0291-graph-examples-extras-and-schema-ddl.md) refines this ADR's skip contract: `CREATE INDEX` / `CREATE CONSTRAINT` are no longer dialect skips (the engine runs and enforces them, so the walkthroughs' schema DDL runs differentially), and the suite gains **extras** — plain-Cypher rewrites of the skipped apoc statements (coll.max/min, node.degree.in, meta.stats/cypher.run, agg.statistics, periodic.iterate), keyed by the original adoc line and run differentially after the adoc statements. The apoc statements themselves stay skipped until the optional apoc implementation (PR #105) merges; with `CYPHERLITE_GRAPH_EXAMPLES_APOC=1` they then run on both engines, and the extras keep testing the apoc-free rewrites.
