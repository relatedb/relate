### Fixed

- A single `|` after a label chain inside a comprehension WHERE now reads
  as the comprehension's map separator, matching Neo4j: `[x IN xs WHERE
  x:A|B]` fails with `UndefinedVariable: variable 'B' not defined`
  instead of silently returning the label disjunction's rows, and the
  operator tail (`x:A|B AND y]`) yields the same way. The disjunction
  reading survives when a later pipe in the same comprehension keeps it
  alive (`x:A|B | x.p1` is unchanged), as are the map-expression
  spellings that already worked.
