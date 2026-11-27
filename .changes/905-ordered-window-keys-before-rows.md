### Changed

- `ORDER BY … LIMIT` over a node scan is much faster on large graphs. A
  candidate the top-k window would reject now costs its sort keys only, with
  no projected row built, and an `ORDER BY id(n)` page over a labeled scan
  (with an optional property comparison in `WHERE`) stops scanning once the
  page is full. `MATCH (n:Label) RETURN n.k ORDER BY id(n) LIMIT 100` over
  100,000 nodes went from about 52 ms to about 1.5 ms. Answers are unchanged,
  and a projection or `WHERE` that can raise on a row past the page keeps its
  full scan and still raises.
