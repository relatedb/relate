### Changed

- `MATCH (n:L) RETURN n.p, count(*)` and the min, max and `count(n.p)`
  aggregates over one labeled property now answer from the existing
  `(L, p)` index instead of scanning every node of the label — the
  `group_by_age` query of the SF0.1 latency suite drops from 17.3 ms to
  6.2 ms. Results are unchanged: whenever the index cannot serve the
  answer exactly, the aggregate falls back to the scan's exact answer
  (ADR-0611).
