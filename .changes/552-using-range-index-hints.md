### Changed

- `USING RANGE INDEX n:L1(p1)` now parses and validates like the plain
  `USING INDEX` hint, as in Neo4j: the hinted variable, label and WHERE
  predicate pass the same fulfillability checks (42N62/42N76), and the
  plan-time 01N31 warning names the hint as written
  (`'RANGE INDEX :L1(p1)'`). The hint still steers nothing — Relate's
  planner is not cost-based, so the kinds differ only in the rendered
  spelling.
