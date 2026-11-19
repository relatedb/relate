### Changed

- A GQL type predicate inside a comprehension's WHERE filter now leaves
  the trailing `|` to the comprehension's map separator, as in Neo4j:
  `[k IN keys(n) WHERE n[k] IS :: STRING | k]` plans and runs (a genuine
  `IS :: STRING | INTEGER` union inside the filter still parses
  greedily), and every `IS [NOT] ::` / `IS TYPED` / bare `::` spelling
  works in list, pattern, and CIP-272 map comprehensions.
