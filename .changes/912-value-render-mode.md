### Changed

- `relate_core::Value::to_cypher_literal` renders round-trippable text
  again: labels, relationship types and map keys whose spelling is not a
  plain identifier are backticked (`(:`special label`)`,
  `{`first name`: 'Alice'}`), and point coordinates keep the engine's
  float spelling (`point({srid: 7203, x: 1.0, y: 2.5})`, not
  `x: 1`). `to_display_string` output is unchanged.

### Removed

- `Node::to_cypher_literal`, `Relationship::to_cypher_literal` and
  `Path::to_cypher_literal` — public since the entity types' extraction
  but never called anywhere in the workspace; every caller renders
  through `Value::to_cypher_literal` (REL8-217/REL8-2300).
