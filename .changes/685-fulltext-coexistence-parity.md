### Notes

- ADR-0372's Later-references note records the 2026.10.1 re-probe: the
  serving-pair conflict rule the ADR set is parity with current Neo4j,
  which now accepts the non-serving coexistence (a FULLTEXT index beside
  existence, type and uniqueness constraints and a graph type's
  `STRING NOT NULL` property, in both creation orders). Records and a
  parity pin test only — the engine behavior is unchanged since the
  serving-pair narrowing.
