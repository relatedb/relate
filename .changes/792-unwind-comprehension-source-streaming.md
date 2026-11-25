### Changed

- `UNWIND` of a list comprehension (`[x IN range(…) WHERE … | …]`) streams
  its elements: each element is bound, filtered and projected as the
  consumer pulls it, so the projected list is never built and an
  aggregating query's memory no longer scales with the list length
  (ADR-0555, REL8-1372). A pattern-comprehension source still materializes
  — the eval walk is not resumable; tracked on the same ticket.

### Fixed

- List and map literals are charged to the per-allocation expression
  budget: a query-sized literal (bounded by the query-text cap, so up to
  hundreds of MB of allocation) now refuses under a small expression
  memory limit instead of allocating silently.
