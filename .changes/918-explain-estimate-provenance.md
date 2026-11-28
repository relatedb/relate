### Changed

- `EXPLAIN` and `PROFILE` plan nodes show the planner's own estimate, with a
  new `EstimateProvenance` (`measured`, `sampled`, `statistic` or `defaulted`) naming the
  ladder level behind `EstimatedRows`, in the plan wire, the `relate.plan/v1`
  document and the Go, Java, JavaScript and Python clients. A `defaulted`
  estimate is the default-selectivity table's guess, not a measurement. A bare
  relationship start (`MATCH ()-[:T]->()`) no longer shows the type's row
  count, and a filtered scan no index serves shows its default-selectivity
  estimate instead of the whole label count (ADR-0708, proposed).
