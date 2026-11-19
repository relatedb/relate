### Changed

- `EXPLAIN`/`PROFILE` plan trees show display-only child steps under an
  index-seek-started `MATCH` — the seek step plus one `Expand` step per
  hop, in the CLI's plan table and as fused-flagged children in the
  structured plan (Bolt `queryPlan`/`profiledQueryPlan`, HTTP v2, the
  CLI's machine modes). Execution is unchanged: the clause stays the
  executed unit; consumers that require per-operator metrics skip the
  children.
