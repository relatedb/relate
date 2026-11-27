### Changed

- Vector `SEARCH` (and `db.index.vector.queryNodes`) now reads the indexed
  vectors from a packed `f32` matrix cached per index and refreshed
  incrementally on writes, instead of re-reading every candidate node's
  property map per query: a 10,000-node × 384-dimension index drops from
  19–61 ms to ~2–5 ms per search, independent of how many other properties
  the indexed nodes carry, and the speed holds across writes to those nodes
  (an access-counter `SET` costs one row refresh, not a rebuild). Scores and
  answers are bit-identical; every case the matrix declines — FLOAT64
  vectors, older pinned readers, views with uncommitted writes — takes the
  previous scan unchanged (ADR-0705).
