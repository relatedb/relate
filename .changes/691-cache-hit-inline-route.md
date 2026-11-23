### Changed

- Warm point-lookup statements (an index seek or label scan with a simple
  return or count/sum/avg fold, and their one-hop forms) run on the
  calling thread from their second execution: the per-connection
  query-worker handoff — the largest fixed cost left on a warm statement —
  no longer applies to them (ADR-0663).
