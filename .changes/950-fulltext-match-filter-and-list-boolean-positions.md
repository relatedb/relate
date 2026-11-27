### Fixed

- A property predicate inside a fulltext `SEARCH` window's inner `WHERE` (e.g. `WHERE p.age > 20` in `SEARCH p IN (FULLTEXT INDEX ix FOR 'red' …)`) now rejects with Neo4j's invalid-predicate error instead of silently filtering; the same filtering remains available through the MATCH's own `WHERE`. Vector windows keep their predicate forms unchanged.
- A builtin call whose declared return type is a list, used where a boolean is required (e.g. `WHERE tail([1,2,3])`, `x AND labels(n)`), now raises a compile-time coercion type error instead of taking the deprecated list-to-boolean conversion. Parameters, property reads and projections keep their answers.
