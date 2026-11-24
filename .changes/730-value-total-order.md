### Fixed

- `ORDER BY`, `min`/`max` and `coll.sort` now read one total order over
  every value kind: maps order by size, then keys, then the values under
  them (so a null or a cross-type pair under a key decides the order
  instead of making the maps equal), points by SRID then coordinates, and
  nodes, relationships and paths by entity id. The output of a sort over
  these kinds is now ordered, `min`/`max` answer the real extremes, and
  sorting more than 20 maps no longer fails the statement with an internal
  error.
- `percentileDisc` returns the input value it picks instead of an F64
  image: an INTEGER sample answers an INTEGER, exactly, above 2^53.
- A far-year `date` (beyond year ±262142) now compares and sorts with
  in-range dates instead of answering null for `<`/`>` and keeping the
  input order.
