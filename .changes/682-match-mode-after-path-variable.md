### Fixed

- A match-mode phrase after a path variable declaration —
  `MATCH p = REPEATABLE ELEMENTS (a)-[r]->(b)` — now rejects with
  `InvalidPathModePosition` (the phrase is legal only directly after
  `MATCH`, matching Neo4j), instead of executing with relationship
  uniqueness silently lifted. The mode-first spelling
  (`MATCH REPEATABLE ELEMENTS p = …`) and the CIP-51 path modes
  (`p = TRAIL (…)`) are unchanged.
