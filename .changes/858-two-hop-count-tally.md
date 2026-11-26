### Changed

- A directed two-hop `MATCH (a)-[:R]->(b)-[:S]->(c) RETURN count(*)` (or
  `count(c)`) is now answered by one tally per distinct middle node instead of
  one visit per path: on a 100k-node, 2.4M-edge graph the 58M-path count drops
  from 7.0 s to 0.29 s with the same answer (ADR-0693). Opposite-direction,
  undirected and longer chains are unchanged.
