### Notes

- Planner (REL8-2248, REL8-2241): when two orientations of a pattern price
  the same on cost and seed rows, the seed whose estimate is the more
  trustworthy now starts the walk (a key statistic over the default
  table), instead of always keeping the written direction. A basis never
  overrides a strictly cheaper price. The scan-floored estimate kinds fold
  into one `Estimate::Scanned { rows, scan, basis }`, and `Basis` is now a
  public ordered type.
