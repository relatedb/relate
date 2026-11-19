### Changed

- A pattern predicate inside a quantifier's WHERE (`all`, `any`, `none`,
  `single`) is now accepted in WITH/RETURN projections and SET values,
  as in Neo4j: `RETURN all(x IN $p1 WHERE (:L1 {p1: x})<-[:R]-(:L2
  {p1: $p2}))` evaluates the pattern per element; bare pattern
  projection items and patterns in value subexpressions keep their
  rejections.
