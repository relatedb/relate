### Changed

- A grouped count over a fixed pattern whose keys are properties of one node
  and whose aggregates are plain counts (`MATCH (f)-[:R]->(p) RETURN
  p.name, count(f)`) now counts per key node and collapses nodes with equal
  key values at the end, instead of hashing a key per binding: a 2.4M-edge
  in-degree top-3 runs in about half the time with the same rows.
