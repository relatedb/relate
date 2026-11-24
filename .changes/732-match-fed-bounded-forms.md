### Fixed

- `MATCH`-fed `ORDER BY … LIMIT` and a named `DISTINCT` now fold into their
  bounded forms instead of holding the whole input: the ordered page folds
  into the bounded top-k window and the `DISTINCT` into a first-seen pass as
  rows land, so memory scales with the page or the distinct keys, not the
  input row count. A provably empty page (`LIMIT 0`) answers without
  collecting its input. Answers are unchanged.
