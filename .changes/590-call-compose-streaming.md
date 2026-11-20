### Changed

- A `CALL` feeding an aggregation or an ordered window holds constant
  memory however wide its input-rows × procedure-rows cross product is:
  the composition streams into folding consumers (ADR-0555), where it
  previously materialized every composed row. A `CALL` whose consumer
  cannot fold now bills its composed rows against the expression memory
  cap instead of silently ballooning (ADR-0463).
- Three classification corners align with the facade's compile-time
  validator: a user-registered procedure that declares no yields is void
  at execution too; a NULL fulltext query on `db.index.fulltext.query*`
  answers no rows (the policy the SEARCH clause and the vector CALL
  already answer) instead of erroring; a YIELD of the non-declared entity
  spelling on a vector call reads Null instead of a phantom alias, and a
  per-row builtin's produced rows are no longer re-filtered through the
  stored-set argument predicate.
