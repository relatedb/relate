### Added

- Experimental opt-in for the Vortex columnar property store
  (ADR-0665/0703), default-off everywhere: the `vortex-store` build
  feature plus `RELATE_PREVIEW=vortex-store`,
  `OpenOptions::vortex_store`, and the `--vortex-store` CLI flag on query
  and serve. Attaching changes the storage representation (point reads,
  equality and aggregate lookups route to a columnar store; writes
  publish through its delta overlay) — answers, the Cypher dialect, and
  the checkpoint format are unchanged. Python, JavaScript and the other
  C-ABI clients opt in through the environment variable only this round.
