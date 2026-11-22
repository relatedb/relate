### Fixed

- A function call as the map expression of a comprehension whose WHERE
  filters by label now parses and runs —
  `[x IN xs WHERE x:L2 | substring(x.p1, 0, 1)]` executes like its
  property-access and projection siblings, instead of failing with
  `expected RightBracket, got "("`. Label disjunctions in a comprehension
  WHERE keep their reading (`x:A|B AND …` still commits on operators).
