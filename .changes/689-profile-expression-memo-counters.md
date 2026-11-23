### Added

- `PROFILE` now reports what the expression memos did. A statement that used
  a memo gets a `Memo` column in the plan table (`hits/probes`, suffixed `*`
  when a memo gave up at its hit-rate review), and the plan carries the
  counters on the operator that ran them: `MemoProbes`, `MemoHits`,
  `MemoInserts`, `MemoRefused`, `MemoBytes` and `MemoDisabledCount` in the
  Bolt and HTTP plan args, and `actual.memo` in the `relate.plan/v1`
  document (C ABI, Python, MCP, CLI JSON; `ClauseMetrics::memo` in Rust,
  `PlanActual.memo` in Java). The datetime constructor memo's counters sit
  on the plan root beside a `kinds` list. The group is present only when a
  memo was probed, and `MemoBytes` is never part of `MemoryBytes`.
