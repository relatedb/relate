---
type: adr
area: cypher
date: 2026-08-28
deciders: MemoryDB maintainers
name: 0056-opencypher-tck-language-contract
status: accepted
tags:
  - conformance
  - dialect
title: OpenCypher TCK is the Cypher language contract
REFERENCES:
  - "[[adr/0002-cypher-25-semantic-target]]"
  - "[[adr/0006-tck-harness-and-skips]]"
  - "[[adr/0054-one-c-abi-env-python]]"
  - "[[adr/0057-embedded-c-abi-and-bolt]]"
  - "[[adr/0482-spec-harness-coverage-lane]]"
  - "[[adr/0520-move-cypher-tests-to-spec-harness]]"
---

# 0056. OpenCypher TCK is the Cypher language contract

## Context

“Cypher” is used for both openCypher and Neo4j’s evolving dialect (and GQL is
a separate ISO language). MemoryDB needs one unambiguous correctness target
for the query language so parsers, planners, executors, and clients do not
diverge.

## Decision

The language contract is the **openCypher Technology Compatibility Kit**,
pinned to release **2024.3** (feature files vendored under
`tests/tck/features/`). Cucumber-expanded scenario count is the compliance
denominator. We claim “TCK compliant” only at 100% of that corpus.

Neo4j-only clauses, procedures, and GQL are out of scope unless they appear
in this pinned TCK.

## Alternatives considered

- **Neo4j Cypher as implemented in Neo4j 5.x** — moving target, proprietary
 docs, not a kit we can vendor. Rejected; Bolt is transport only ([ADR-0057](./0057-embedded-c-abi-and-bolt.md)).
- **ISO GQL** — important long-term, not the TCK we committed to. Rejected
  for v1; a second language would be a new ADR.
- **A hand-written “Cypher subset” without TCK** — faster to start, no
  certification path. Rejected; the product goal is TCK compliance.
- **Live-fetch TCK from GitHub in CI** — non-reproducible. Rejected; vendor
 the pin ([ADR-0006](./0006-tck-harness-and-skips.md)).

## Consequences

- Parser, planner, executor, and error codes are driven by TCK scenarios.
- A version bump of the TCK pin is a new ADR.
- Clients document OpenCypher, not Neo4j Cypher.
- Mock TCK procedures must be implemented for 100% (harness registry).

## Later references

- [ADR-0054](./0054-one-c-abi-env-python.md): feature files live under `testdata/tck`.
- [ADR-0520](./0520-move-cypher-tests-to-spec-harness.md): the in-repo pin, its
  vendored corpus and its 100% gate retired on 2026-11-05. The openCypher TCK
  is still the language contract, measured by the spec harness
 ([ADR-0482](./0482-spec-harness-coverage-lane.md)) against the current upstream
  suite — reporting-only, with no in-repo pass claim.
- Renumbered from [ADR-0002](./0002-cypher-25-semantic-target.md) to [ADR-0056](./0056-opencypher-tck-language-contract.md): the CypherLite/MemoryDB merge
  imported the MemoryDB ADR series numbered from 0001 while this repo already
  had ADRs 0001–0003. The main-era three were the minority (50+ files use the
  imported numbering), so they moved to 0055/0056/0057 to keep every number
  unique. This ADR was formerly `0002-opencypher-tck-language-contract.md`.
