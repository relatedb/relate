### Fixed

- `vector()` integer coordinates are truncated toward zero before the
  kind's width test, matching Neo4j: `vector([127.9], 1, INT8)` returns
  `[127]` instead of raising an overflow. An out-of-range coordinate
  still overflows, and INT64's upper bound stays inclusive at 2^63 (the
  value `i64::MAX as f64` rounds up to), which the saturating cast clamps
  to `i64::MAX` (REL8-1897).
