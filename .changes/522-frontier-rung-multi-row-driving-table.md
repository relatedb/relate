### Changed

- A fixed-length clause entered by two or more rows now runs on the
  frontier kernel, one run per entering row, instead of falling to the
  per-row walker — the walker enumerates trails, exponentially on a
  dense cyclic component. A single row whose run declines at run time
  still drags the clause to the walker.
