### Changed

- A checkpoint reopen now decodes and installs across all cores — one
  Parquet reader per row-group span, a slice-based bulk install that
  reads rows in place — so reopening a large store drops from 1225 ms
  to 461 ms on the measured fraud shape (2.66×; 1.36× on a shape whose
  restore work was already sharded). Restore rows are read in place and
  installed in bounded slices, so peak restore memory is
  `max(rows, graph)`, not `rows + graph`.
