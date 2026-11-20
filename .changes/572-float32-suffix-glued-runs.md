### Fixed

- Number literals glued to FLOAT32 suffix runs (`1.0F32`, `1.0f32`,
  `1.0e0F32`, `.4d32`) now fail as one `InvalidNumberLiteral` malformed
  literal (Neo4j's 42I07 shape) instead of a confusing "unexpected
  token" parse error, and a combining mark glued to any number is the
  same malformed run. The bare suffix spellings (`1.0f`, `.4d`, `1e4D`)
  and the `FLOAT32` vector type expression are unchanged.
