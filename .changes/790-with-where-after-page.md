### Fixed

- `WITH … ORDER BY / SKIP / LIMIT … WHERE` filters the page the projection
  body produced, as Neo4j does, instead of taking the page from the filtered
  rows: `WITH x ORDER BY x LIMIT 5 WHERE x > 3` over 1..20 now answers
  `[4, 5]`, and a write fed by such a clause no longer receives a row the
  page cut.
