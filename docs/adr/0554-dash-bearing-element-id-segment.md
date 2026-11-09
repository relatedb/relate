# ADR-0554: The element-id namespace segment is dash-bearing

Status: Proposed
Claim: dc9769cbeeecd1b90570f963ba0c48125e6a0b46
Revision: the first landing proposed `relate-neo4j` / `relate-system-ontologies`; the maintainers shortened the tokens to `rl-id` / `rl-onto` for wire economy before this ADR was accepted

## Context

Real Neo4j element ids are `<kind>:<database-uuid>:<id>` — the middle
segment is a UUID, so every id carries dashes (`4:d8d172ec-96d8-4364-…:0`).
Some clients classify an id as "element id" by exactly that observable
property: Neo4j Browser's neighbour-expansion chooses between the
`elementId()` and legacy `id()` query forms by whether the id string
contains a dash, and parses it as an integer otherwise.

Relate's segment was the bare word `relate` (the pre-rename ADR-0116
spelling was `cypherlite`), so ids looked like `4:relate:n4` — no dash.
Against such a client every expansion collapses to the wrong node: the
legacy path parses the leading kind digit as the node id, expands node 4
regardless of which node was clicked, and hands the renderer relationships
whose endpoint node was never on screen.

## Decision

The element-id middle segment is a short, dash-bearing product token —
short because ids travel in every result row and in every client query,
and dash-bearing because that is the property clients observe:

- the writable namespace's segment is `rl-id`
  (ids `4:rl-id:n5`, `5:rl-id:r9`)
- the ontology namespace's segment is `rl-onto`
- `system` still has no elements and no segment

The three-part `<kind>:<segment>:<n|r><id>` shape, the shared parser, and
the segment-to-namespace mapping are unchanged; only the segment spelling
moves. The change is a cutover, not an addition: the old spellings are no
longer accepted. Element ids are emitted from `(id, namespace)` at read
time — nothing persists them — so no store migration is involved;
recordings and fixtures that pinned the old spelling are regenerated.

A regression test pins the client-facing property itself: every emitted
element id contains a dash.

## Alternatives considered

- Emit a fixed UUID as the segment, byte-shaped like Neo4j's. Rejected: a
  fabricated UUID claims an identity the store does not have, and a long
  segment pays its bytes on every row; a short honest token carries the
  same dash.
- Keep `relate` and accept only the third segment changing. Rejected: the
  third segment is the parsed entity reference; a dash there breaks the
  engine's own decoder for no benefit.
- Accept both old and new spellings in the decoder. Rejected: two
  spellings for one fact is a silent fallback; the cutover is loud and
  one-shot.

## Consequences

Clients that sniff the dash now take the element-id path and address the
node actually clicked. Clients that matched an earlier segment spelling
(`relate`, or the first proposed `relate-neo4j`) must move to `rl-id`;
`db.nameFromElementId` and
`graph.byElementId` follow the shared table and keep their failures for
anything else. ADR-0116's pre-rename `cypherlite` spelling remains as
written; this ADR is the current record.
