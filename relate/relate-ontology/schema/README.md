# Vendored Ontology Graph Spec files

Pinned from `neo4j/graph-spec` tag `release-spec/v1.1.0` @
`5ff2c20aa7e428e7c563b17f573157293959a009` (the published 1.1.0; current
main @ `1656fb506543f78cd2f73a7897bd8437632de509` serves a byte-identical
schema). See ADR-0487 for the bump and ADR-0364/ADR-0227 §6 for the
original vendoring decision.

- `ontology-graph-spec.schema.json` — the JSON Schema (2020-12) for the
  published Ontology Graph Spec 1.1.0. Its `$id` is
  `https://neo4j.com/docs/ontology-graph-spec/1.1.0/schema.json`, the only
  value the root `schema` gate accepts (ADR-0487 §1). The validator in
  this crate is hand-rolled structural checking of exactly this schema;
  the file is vendored as the human-readable source of truth and for
  fixture comparisons.
- `example.json` — a **Relate-authored** document in the 1.1.0 shape,
  used as a validation fixture in this crate's tests. It is validation
  only: it deliberately contains spec-valid constructs that
  `db.ontology.apply` rejects (a node with no identifying label, an
  empty endpoint, an empty constraint property list, a
  `PROPERTY_TYPE` constraint and an empty declared property name), so it
  pins the accepted schema rather than convertibility (ADR-0364 §6,
  reworked for 1.1.0 by ADR-0487 §4, the revision recorded by
  ADR-0667). It covers every `Neo4jType` token, all four constraint
  types, the nullable/empty forms and every typed extension member.
- `movies.json` — a movies ontology in the 1.1.0 shape, the converter's
  fixture (ADR-0667): applying its graph type creates 21 constraints —
  15 derived from the element types (the property type declarations,
  the `NOT NULL` existences, Movie's implied label and the four
  endpoint label references) and the 6 named `KEY`/`UNIQUE`
  definitions — and its descriptive fields produce exactly the seven
  skip reports the converter test pins (optional labels on Movie,
  one_of on Movie.genre, cardinality on ACTED_IN.to, and the aliases,
  description and reference decorations on DIRECTED and its score
  property).

Upstream still ships **no root-level `example.json`**: the pinned
README's curl URLs are stale. Upstream's
`go/model/testdata/graph-spec-example.json` is main-shaped but does not
validate against main's own schema —
`relationships.ACTED_IN.from.property` and `.to.property` are extra
members that `RelationshipTarget`'s closed shape rejects. It is not
vendored: verbatim it is invalid, and stripped it duplicates coverage the
authored example already provides.

Fetch command used to vendor the schema:

```sh
curl https://raw.githubusercontent.com/neo4j/graph-spec/5ff2c20aa7e428e7c563b17f573157293959a009/ontology-graph-spec.schema.json
```

Bumping the supported spec version is a new ADR: a new vendored schema, a
validator update, and a raised gate (ADR-0364 §1; done once by ADR-0487).
