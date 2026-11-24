### Changed

- The `SEARCH` clause's complex patterns and entity-constrained window join
  the default dialect (ADR-0678): a searching `MATCH` keeps every pattern
  shape the general `MATCH` machinery accepts — the
  `RELATE_PREVIEW=complex-search-patterns` opt-in and the pattern-shape
  rejections are retired with the reference implementation's own
  graduation — and `SKIP`/`LIMIT` page over the entities the whole MATCH
  statement keeps (its labels, property predicates, outer `WHERE` and
  pre-bound bindings restrict candidates before the window; the `LIMIT`
  counts survivors, not raw hits), for both index kinds and every pattern
  shape. The inner `WHERE` now serves full-text searches too, and its
  predicate family grows the entity match filter (`n IN <expr>`) and its
  id-spelled twin (`id(n) IN <expr>`), with a correlated `COLLECT`
  subquery allowed on the right-hand side. An index created inside the
  open transaction fails a search with the transient `IndexStillPopulating`
  rejection until the transaction ends.
- The `BINDINGS` spellings of the match modes parse and run (ADR-0679):
  `MATCH REPEATABLE ELEMENT BINDINGS` / `MATCH DIFFERENT RELATIONSHIP
  BINDINGS` (also after the plural forms) map onto their element twins'
  relationship-uniqueness semantics.
