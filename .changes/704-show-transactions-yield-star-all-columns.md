### Changed

- `SHOW TRANSACTIONS YIELD *` now carries all forty live columns in alphabetical order, so `YIELD * RETURN elapsedTime` and `YIELD * WHERE ...` resolve every live name. Columns the embedded engine has no state for (timings, lock/page/heap counters, query id, planner/runtime, ...) are Null; `parameters`, `metaData` and `currentQueryStatus` are filled. A bare `SHOW TRANSACTIONS` still renders the seven defaults.
