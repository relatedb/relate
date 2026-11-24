### Fixed

- A bulk-batched auto-commit write no longer changes its answer with data
  size: `MATCH ()-[r]-()` writes each relationship once per orientation,
  and two relationship patterns of one `MATCH` clause keep relationship
  uniqueness, on the batched and the unbatched path alike.
