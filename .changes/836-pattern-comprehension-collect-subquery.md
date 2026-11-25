### Changed

- A pattern comprehension `[(n)-[:R]->(m) WHERE … | f]` evaluates as the
  `collect { MATCH … WHERE … RETURN f }` subquery that is its meaning
  (ADR-0685, REL8-1372): one evaluation path with the nested body's
  planning and the workspace admission's memory bound, instead of a
  dedicated materializing walk. One reconciliation edge follows subquery
  rules by design: a var-length relationship slot rebinding an outer
  single-relationship binding is now the plan-time
  `GroupVariableReuse` error rather than a null reconciliation (the
  list-bound and single-hop spellings keep their answers). The path
  variable keeps its own semantics: `[p = … | …]` shadows an outer name
  of the same spelling for the comprehension's whole scope, exactly as
  before (a nested subquery's references included).
