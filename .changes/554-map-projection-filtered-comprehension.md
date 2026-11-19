### Changed

- A filtered list comprehension's map expression may now be a map
  projection: `[n IN nodes(p) WHERE n:L1 | n{.p1}]` keeps the WHERE's
  label predicate and projects each surviving element through `n{.p1}`,
  as in Neo4j. The `|` was previously eaten by the label disjunction
  (`n:L1|n`), leaving the projection's `{` an unexpected token; a
  genuine disjunction with no map expression (`[m IN list WHERE m:A|B]`)
  keeps its reading.
