### Added

- Four Neo4j/APOC procedures that client tools call but Relate had no
  implementation for now work with no client change and no preview opt-in:
  `db.create.setNodeVectorProperty`, `db.create.setRelationshipVectorProperty`,
  `apoc.create.addLabels` and `apoc.merge.relationship` (ADR-0699). Each is
  re-implemented as a Cypher body the query front-end injects and rewrites
  away — `SHOW PROCEDURES` lists them as `WRITE` procedures with
  descriptions naming their Cypher form. `neo4j-graphrag`'s
  `SimpleKGPipeline` writer now runs unmodified against `relate serve`.
  `EXPLAIN`/`PROFILE` of such a call shows the rewritten subquery rather
  than a procedure node.
