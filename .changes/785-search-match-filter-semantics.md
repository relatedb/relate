### Added

- `CALL db.index.fulltext.awaitEventuallyConsistentIndexRefresh()` now
  executes as a void no-op instead of failing `ProcedureNotFound` — the
  engine applies committed index updates synchronously, so the wait is
  trivially complete, exactly like `db.awaitIndexes()`
  (ADR-0680; the 2026-11-24 corpus sync put the call in the full-text
  acceptance suite's setup)

### Changed

- a SEARCH over an eventually-consistent full-text index now fails with
  `EventuallyConsistentIndexNotSupported` instead of silently scanning;
  the `db.index.fulltext.query*` procedures remain the supported surface
  for such indexes (ADR-0680)
- the full-text corpus memo keeps one corpus per distinct index and
  analyzer (up to sixteen, least-recently-used) instead of a single live
  corpus, charging every retained corpus to the statement's memory meter
  and the query's shared live-bytes counter under the kernel memos' own
  ceiling, releasing on eviction and drop — a `COMBINE` alternating
  sources stops rebuilding each corpus per row, an `UNWIND` over index
  names into the procedures can never pin unbounded uncharged memory,
  and the retained corpus bytes are counted into the statement's joint
  memo budget and folded into PROFILE's memo group at the clause's end
  (ADR-0680)
