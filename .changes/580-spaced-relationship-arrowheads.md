### Fixed

- Whitespace between the relationship dash and the arrowhead now parses —
  `MATCH (a) - [:R*2] - > (b)`, the spaced left head `< - [:R] - (b)` and
  the bracketless `-- >` all plan and run with identical plans to the
  glued forms, instead of failing with `expected node pattern after
  relationship`. A single bare dash with an arrowhead (`(a) - > (b)`,
  `(a)->(b)`) stays rejected as before, and comparisons like
  `RETURN (a) < -1` are unaffected.
