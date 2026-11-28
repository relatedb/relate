### Changed

- Vector values now store their coordinates packed per family (4/8/8 bytes
  per coordinate instead of 48): a 10k × 384 vector store uses ~45% less
  process memory, vector `SEARCH` answers ~2× faster than the previous
  packed-matrix release on the same workload, and bulk loads are ~3×
  faster. Answers, scores, orderings and the durable JSON are unchanged.
