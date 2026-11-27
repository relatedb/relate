### Notes

- Planner (REL8-2249, stacked on the per-key statistics change, ADR-0696):
  an unindexed equality of a key whose statistics are built now prices its
  endpoint from them (the heavy-value count or rows over distinct values),
  and a function-wrapped equality such as `tolower(p.gender) = ...` from the
  raw distinct count, as a new `Estimate::Statistic` that carries the
  provenance; the default table (ADR-0694) answers only where the statistics
  return nothing. No query answers change.
  A cached plan that now reads the statistics replans once when they are
  first built (ADR-0700), then hits again.
