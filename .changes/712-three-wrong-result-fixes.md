### Fixed

- A `DELETE` whose `MATCH` held an empty pattern it did not name (`MATCH (a:A), (b:B) DELETE a`
  with no `:B` node) deleted every node of the named label once that label crossed the
  10,000-target batching threshold; the row space is the cross product of every pattern, so an
  empty factor now yields no targets at any size. A `WITH` between the `MATCH` and the write
  that drops a matched variable (`MATCH (a:A), (b:B) WITH a DELETE a`) now declines batching
  entirely — the dropped factor still empties or scales the row space, which the batched path
  cannot see.
- `EXISTS { MATCH … MATCH … }` and `count(DISTINCT x) > 0` over two `MATCH` clauses answered
  false when only the same relationship could satisfy both clauses (relationship uniqueness
  was applied across the clauses); `EXISTS` now agrees with `COUNT`, and a `NOT EXISTS`
  feeding a `DETACH DELETE` no longer deletes the node.
- A `WHERE` comparison on a node variable repeated inside one path pattern (a cycle) dropped
  the other occurrence's labels and property map, returning rows no node satisfies and
  letting `SET` / `DELETE` act on them; every occurrence's constraints are now checked on
  every pattern engine.
