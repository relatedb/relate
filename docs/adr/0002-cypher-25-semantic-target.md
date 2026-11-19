---
type: adr
area: cypher
name: 0002-cypher-25-semantic-target
status: accepted
tags:
  - dialect
title: Cypher 25 semantics are the target where sources disagree
---

# Cypher 25 semantics are the target where sources disagree
The CIP documents, Neo4j's Cypher 5 behavior, and Neo4j's GQL-aligned
Cypher 25 behavior occasionally disagree (the docs are proposals; shipped
behavior sometimes deviates; the two Cypher versions differ deliberately).
We decided that live Neo4j behavior under `CYPHER 25` is authoritative for
cip-eval expected results and therefore for what CypherLite implements; CIP
text fills gaps only where Neo4j cannot verify a feature, and any observed
deviation from the doc is noted in the scenario file. Rationale: a large
share of the CIPs exist precisely to align Cypher with GQL, so Cypher 25 is
the direction of travel and the only coherent single target; implementing
doc-only behavior that no real system exhibits would make the Neo4j
verification step meaningless.
