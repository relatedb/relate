### Fixed

- A predicate function's own variable now shadows a same-named outer
  variable in the compile-time kind checks: `ALL(p IN nodes(p) WHERE
  size(labels(p)) > 0)` over a path `p` no longer fails with
  `labels() requires a node, got PATH` — the bound element is the
  runtime's to check, and the unshadowed spellings keep their rejection.
