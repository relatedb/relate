### Fixed

- `valueType()` over a list mixing flat and nested list elements rendered the nested member ahead of the flat one (e.g. `valueType([1, [], [2, [null]], [2.0, 2]])`); the flat member now sorts first, matching Neo4j's canonical order in either input order.
