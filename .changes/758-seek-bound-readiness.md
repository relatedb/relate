### Fixed

- A `WHERE` comparison whose bound side is a subquery — `p.flag = EXISTS { … }`
  or `a.prop = COUNT { (b)… }` — no longer evaluates the subquery before
  the pattern's variables are bound and serves the result as an index
  seek. The EXISTS form kept only rows where both the comparison and the
  subquery held (a subquery with a free imported name matches anything,
  yielding `true`); the COUNT form returned 0 rows whenever the planner
  expanded the other pattern first (the free name made the count cover
  the whole side). Both now compare the value the subquery yields for
  each row.
