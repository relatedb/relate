### Changed

- Label expressions now parse in every node pattern a query can write,
  not only MATCH patterns: a `WHERE` pattern predicate
  (`WHERE (a:L1|L2)<-[:R]-(b)`) and a pattern comprehension's leading
  node (`[(x:L1&L2)-[:R]->(y) | y]`) accept the full grammar — `|`, `&`,
  `!`, `%`, parenthesized groups, colon-chains and dynamic atoms — as
  Neo4j does. Both positions previously failed at the parse: the
  predicate read as a parenthesized `a:L1|L2` the relationship then
  broke, and the comprehension read as a list literal (ADR-0626).
