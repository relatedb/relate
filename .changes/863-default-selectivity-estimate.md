### Notes

- Planner (REL8-2217, ADR-0694 proposed): predicates no index serves
  (unindexed equality and ranges, function-wrapped equality such as
  `tolower(x) = tolower($p)`) now narrow their endpoint's estimate through one
  documented default-selectivity table, tagged as a default rather than a
  measurement, so a multi-pattern `MATCH` anchors on the selective side. No
  query answers change.
