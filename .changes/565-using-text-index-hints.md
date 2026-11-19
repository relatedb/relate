### Changed

- `USING TEXT INDEX n:L1(p1)` now parses and validates like the plain
  `USING INDEX` and `USING RANGE INDEX` hints, as in Neo4j: the hinted
  variable, label and WHERE predicate pass the same fulfillability
  checks (42N62/42N76), and the plan-time 01N31 warning names the hint
  as written (`'TEXT INDEX :L1(p1)'`). The hint still steers nothing —
  Relate's planner is not cost-based.
