### Fixed

- A FULLTEXT index's configured `fulltext.analyzer` now analyzes its stored
  tokens and the query the procedures and a `SEARCH` without `WITH
  ANALYZER` run: an `english` index drops English stop words and
  Porter-stems both sides, so `refunds` finds `refund` and `charge` finds
  `charged` — previously the option was echoed but its stemming never ran.
  `WITH ANALYZER null` is no override (the query follows the index's
  analyzer) rather than a silent switch to the standard analyzer. Default
  (`standard-no-stop-words`) and `standard-folding` indexes are unchanged
  (ADR-0636).
