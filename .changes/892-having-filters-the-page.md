### Fixed

- An aggregating `WITH`'s trailing `WHERE` — the HAVING form, whose
  predicate carries an aggregate call — now filters the page the
  projection body produced instead of the groups before it, matching
  live Neo4j: `UNWIND [1,1,1,2,2,3] AS x WITH x AS g, count(*) AS c
  ORDER BY g DESC LIMIT 2 WHERE count(*) > 2 RETURN collect(g) AS gs`
  answers `[]` (was `[1]`), and a write fed by such a clause no longer
  receives rows the page had cut (REL8-2135).
