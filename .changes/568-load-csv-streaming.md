### Changed

- `LOAD CSV` feeding an aggregation (or an ordered window) streams its
  records: the rows are handed to the consumer as they are read and never
  collected, so peak memory stops scaling with the file's record count and
  a part-sized file loads under the default per-allocation cap where the
  materializing form refused (ADR-0555, LABS-1845).
