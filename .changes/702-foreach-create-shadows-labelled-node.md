### Fixed

- A `FOREACH` body's `CREATE (n:L)-[:R]->(m)` now declares a fresh `n` that shadows an enclosing `n`, instead of relabelling the enclosing node.
