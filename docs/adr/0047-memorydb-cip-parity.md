# 0047. MemoryDB reaches CypherLite CIP parity

- **Status**: Accepted
- **Date**: 2026-09-16
- **Deciders**: CypherLite / MemoryDB merge

## Context

ADR-0046 made the CypherLite parser the product front-end and established the
MemoryDB-backed CIP evaluation as an upward-only ratchet. It excluded embedded
catalog theater from the merge gate. That leaves the product runtime below the
language behavior of `cyperlite-original`: the original passes 5586 of 5629
CIP-eval scenarios while `memorydb::Database` passes 2079.

The merged product must not expose a smaller Cypher surface than the original.
Per-CIP counts alone are insufficient because one passing scenario can replace
a different passing scenario while keeping the same count.

## Decision

1. `memorydb::Database` must pass every CIP-eval scenario passed by the
   checked-in original CypherLite engine. Only scenarios that fail or are
   skipped by the original may fail or be skipped by MemoryDB.
2. The gate compares stable scenario identities as well as aggregate counts.
   Live Engine CIP is replaced by the frozen identity set in
   `testdata/cip-eval/ENGINE_PASSING.json` (ADR-0052). Aggregate CIP counts
   remain in `testdata/cip-eval/BASELINE.json` and
   `baselines/cip-eval-memorydb.json`.
3. Embedded catalog behavior previously excluded by ADR-0046 is now in scope,
   including SHOW users/roles/servers/databases, database DDL, graph types,
   Cypher-version selection, and composite USE where the original implements
   them.
4. `cypherlite-parser` remains the only product grammar. `memorydb-parser` may
   gain AST/IR types but its parser is diagnostic-only.
5. Semantics are ported onto the MemoryDB planner, executor, catalog, CSR,
   overlay, and WAL. The GraphStore storage path is not grafted onto MemoryDB.

## Alternatives considered

- Keep catalog theater excluded and target only language-only CIPs — rejected
  because it permits MemoryDB-only gaps that the original product did not have.
- Gate only on the total 5586 pass count — rejected because it can hide
  scenario substitution.
- Route unsupported queries to the legacy engine — rejected because that
  creates two storage and transaction authorities.
- Extend the historical MemoryDB grammar — rejected because it duplicates the
  CypherLite parser and violates ADR-0046.

## Consequences

- ADR-0046's catalog-theater exclusion is superseded by this decision.
- The MemoryDB ratchet remains useful during implementation, but completion
  requires scenario-set parity with the original.
- New implementations must preserve the openCypher TCK floor and the existing
  load-test thresholds.

## Later references

ADR-0119 supersedes Decision 1 for 24 frozen CIP-eval identities, in the
cypher-versions-default-language and call-variable-scope-clause suites,
that need Neo4j Enterprise database administration or composite
graphs. ADR-0116's Community Edition semantics reject those. Every other
frozen identity remains a hard gate.
