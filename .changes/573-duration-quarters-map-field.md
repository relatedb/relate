### Fixed

- `duration({quarters: …})` now rejects the `quarters` map field with
  `Unknown field: quarters` (Neo4j's 22G0I) instead of summing it into
  the months lane — the integral, float and mixed years/months
  spellings all reject. The `d.quarters` accessor, the `q`/`Q`
  formatting-pattern letter and every other duration map field are
  unchanged.
