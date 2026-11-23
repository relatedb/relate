### Added

- `OpenOptions::load_threads` and `RELATE_LOAD_THREADS` cap how many
  threads opening a store may use (default: the machine's available
  parallelism; zero or invalid values fail the open). A new
  `Database::open_peak_transient_bytes` reports the most temporary memory
  the open's snapshot decode and adjacency rebuild held at once, and a
  follower's gap reload refreshes it.
