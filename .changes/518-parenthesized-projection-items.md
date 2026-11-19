### Changed

- Redundant parentheses around a projection item no longer change its
  meaning: `WITH (n)`, `WITH DISTINCT(n)`,
  `RETURN DISTINCT(n.p1) ORDER BY n.p1` and `GROUP BY (a)` now behave
  exactly like their unparenthesized forms — alias rules, ordering under
  `DISTINCT`, grouping keys and exemptions, `CALL` imports and the
  rendered column names — as Neo4j treats them. A property access keeps
  Neo4j's verbatim column name (`(n.p1)` names its column `(n.p1)`), and
  no restriction is loosened: every pinned refusal keeps its verdict.
