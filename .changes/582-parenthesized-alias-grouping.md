### Fixed

- `NOT(b)` and `(b)` over a boolean or scalar alias in WHERE now validate
  and execute — the parens read as grouping, matching Neo4j — instead of
  failing with `UndefinedVariable` (the alias invisible to its own WITH's
  WHERE) or the single-node pattern rejection. The same spellings over
  node and path variables keep the single-node pattern-predicate rejection.
