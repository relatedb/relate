### Changed

- `ON CREATE SET labels(n) = …` keeps the merge pattern's own labels on the node it created: the overwrite unions the pattern's names into the written set, so re-running the same `MERGE` matches instead of creating a second node (ADR-0695). Plain `SET labels(n) =`, the `ON MATCH` arm, and `+=` / `-=` are unchanged.
