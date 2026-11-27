### Notes

- Planner (REL8-2352): a range predicate with a literal bound on an
  unindexed key whose statistics are built now prices from the key's numeric
  minimum and maximum (values assumed uniform between them) instead of the
  default third; a parameter bound still prices from the default table. A
  cached plan that reads the statistics replans once when they are first
  built (ADR-0700). No query answers change.
