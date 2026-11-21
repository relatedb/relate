### Added

- A markdown vault can carry its schema in version control: a committed
  `ontology.yaml` at the vault root declares key properties, the
  relationship vocabulary, the ontology document (`db.ontology.get`
  serves it; `set`/`drop` refuse while it is present — edit the file),
  the graph type, and the indexes (the spec's native
  `extensions.indexes` envelopes, validated and defaulted the way a
  `CREATE INDEX` would be). Every machine and fresh clone opens with
  the declared schema installed — fulltext and vector search live out
  of the box; a schema change is a PR (ADR-0649).
- Rust API: `Database::checkpoint_if_pending()` — the close-path
  checkpoint shape; explicit `checkpoint()` is unchanged.

### Changed

- Markdown vaults at scale open and close dramatically faster: a clean
  session (open + exit) no longer re-walks and re-renders every vault
  file at exit — a checkpoint with nothing to publish writes nothing
  (ADR-0646) — and both the directory walk and the file read run
  concurrently. On a 27k-file vault: unchanged session 4.1s → ~1.25s
  warm, cache-evicted open 7s → 1.8s, fresh-clone bootstrap ~1.65s; a
  100k-file vault now works at all (the sidecar's own 256 MiB bound,
  ADR-0648) and bootstraps in ~5.7s.
- `relate --read-only` opens skip the exit checkpoint by design; the
  first open of a fresh clone needs `--snapshot-format markdown` once
  (then plain opens work).

### Fixed

- A vault copied through a normalizing tool (macOS `tar`, zip, some
  sync tools) opens and heals to the canonical filename spelling:
  Unicode normalization no longer makes the vault unopenable
  (ADR-0647).
