### Fixed

- `PROFILE` runs the streaming path it reports: a `MATCH` feeding later
  clauses streams under `PROFILE` too instead of materializing its whole
  output, so the statement completes where the profiled form used to
  refuse with the per-allocation cap (a 1.66M-row cross product now
  profiles under the default limit), and every consumed clause reports
  its own rows, time and reads — the head clause's record states its own
  work, never the run's (REL8-1904).
