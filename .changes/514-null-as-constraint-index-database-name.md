### Changed

- A constraint, index or database can now be literally named `null`:
  `CREATE CONSTRAINT null …`, `CREATE INDEX null`, `DROP CONSTRAINT
  null`, `DROP INDEX null`, `ALTER DATABASE null` and
  `SHOW DATABASE null` parse and run, as in Neo4j. String-literal names
  and the continuation keywords stay rejected.
