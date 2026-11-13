# 0228. Ontology list property types map to closed graph-type list types

- **Status**: Accepted
- **Date**: 2026-10-12
- **Deciders**: CypherLite maintainers (Linear LABS-132)

## Context

ADR-0226 §2 maps ontology property `type` tokens to graph-type value types:
"`LIST<X>` to the corresponding list type, `LIST<ANY>` to `LIST<ANY>`".
Implementing `db.ontology.apply` (ADR-0121) exposed that the literal
reading produces graph-type bodies the engine's own validator rejects:
Graph-type property types are closed, so a list's inner type is mandatory
`NOT NULL` (`LIST<STRING>` fails validation with "Lists cannot have a
nullable inner type"), and `LIST<ANY>` is rejected in both spellings
(`LIST<ANY>` for the nullable inner type, `LIST<ANY NOT NULL>` as an
unsupported property type). The Ontology Graph Spec's `LIST<X>` token has
no inner nullability marker at all, so there is no nullable-element reading
to preserve. Without this correction, every ontology declaring a list
property fails `db.ontology.apply` at body validation.

## Decision

In the ADR-0226 §2 mapping (`cypherlite_ontology::to_graph_type_body`):

- `LIST<X>` maps to its closed graph-type form `LIST<X NOT NULL>` — the only
  spelling of the token a graph type accepts, matching the parser's own
  rule for property types.
- `LIST<ANY>` declares nothing — like bare `ANY` and `BYTES`, it has no
  graph-type property-type counterpart, so the property is unconstrained. It
  remains queryable in `system_ontologies` through the ADR-0226 §1
  mapping.

## Alternatives considered

- **Keep the literal `LIST<X>` / `LIST<ANY>` mapping.** Rejected: the
  bodies fail `validate_graph_type_body`, so `apply` could never succeed
  for a document with a list property.
- **Map `LIST<ANY>` to `LIST<ANY NOT NULL>`.** Rejected: not a supported
  property type either — the validator rejects it outright.
- **Make graph-type property types accept nullable inner list types.**
  Rejected: that changes the engine's Cypher dialect for one mapping; the
  closed form is the language's own spelling of the same constraint.

## Consequences

- `db.ontology.apply` accepts ontologies with list properties; the
  enforced constraint is the closed list type.
- ADR-0226 §2's "`LIST<ANY>` to `LIST<ANY>`" clause is superseded; the
  rest of the token mapping stands.
- A spec-side nullable-element list marker, should one ever exist, arrives
  with a new vendored schema and its own ADR (ADR-0227 §7).

## Later references

- ADR-0229 voids the `LIST<ANY>` clause's last sentence ("It remains
  queryable in `system_ontologies` through the ADR-0226 §1 mapping"):
  nothing is queryable in `system_ontologies`, and the ADR-0226 §1
  mapping is not shipped. The closed `LIST<X NOT NULL>` mapping — the
  apply converter's rule — stands.
- ADR-0364 re-bases the token vocabulary on main's 39-token
  `Neo4jType` enum. This ADR's rules stand: `ANY`/`LIST<ANY>` declare
  nothing, and `LIST<X>` still maps to the closed `LIST<X NOT NULL>`.
  Main's `UUID` maps to the `UUID` predefined kind; `FLOAT32`, `INTEGER8`,
  `INTEGER16` and `INTEGER32` (and their list forms) have no plain
  graph-type counterpart and fail loudly at `apply` instead of declaring
  nothing or widening.
