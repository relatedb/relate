### Fixed

- An ontology relationship `type` declared across several endpoint pairs
  (one type, several `relationships` entries — a shape the Ontology
  Graph Spec allows) no longer breaks `CALL db.ontology.apply(…)`
  (`duplicate identifying label`) or the vault-committed
  `ontology.yaml` open (where every declared pair was refused with a
  sibling entry's expected label): such a type now defines no graph
  type at all — its relationships stay writable under any labels, the
  type still seeds the vault's link vocabulary, and types declared once
  enforce exactly as before (ADR-0668, REL8-2042)
- Two node entries sharing one identifying label are refused at the
  ontology mapping with a named diagnostic (`one label, one node type`)
  instead of a late generic `duplicate identifying label` at apply; an
  entry restating an earlier entry's endpoint pair with a different
  declaration is likewise refused naming both entries
