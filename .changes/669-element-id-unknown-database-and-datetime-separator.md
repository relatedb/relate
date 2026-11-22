### Fixed

- `db.nameFromElementId` over a well-formed element id that names an unknown
  database now fails with the `EntityNotFound` class (GQLSTATUS `42001`),
  as Neo4j does, instead of a semantic error; a malformed id keeps its
  unexpected-format error.
- `datetime()` and `localdatetime()` reject a space-separated date-time text
  (`'2024-01-02 03:04:05'`) as Neo4j does; only the `T` separator parses.
