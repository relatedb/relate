# ADR-0575: Shortest-path expressions require bound endpoint nodes

Status: Proposed
Claim: 46b4e1084ba8e7b8051484567b9f8014f12a9d0e

## Context

`shortestPath()`/`allShortestPaths()` appear both as a `MATCH`-clause
pattern and as an expression inside `RETURN`, `WITH` or a predicate. The
expression form historically accepted anonymous or unbound endpoints and
ran a layered search from the start pattern until it reached a node
matching the end pattern (ADR-0189). The expression form now lowers onto
the same selector pattern as the clause form (ADR-0568), whose partitions
are the row's bound start and end nodes; an unbound endpoint has no
partition to answer.

Live Neo4j 2026.10.1 rejects every shortest-path expression whose endpoint
node pattern does not name a bound variable, with `42001` `SyntaxError`
("A shortestPath(...) requires bound nodes when not part of a MATCH
clause"). The `MATCH`-clause forms keep their open endpoints.

## Decision

A shortest-path expression is admitted only when both endpoint node
patterns name a variable already in scope. The rule is enforced at compile
time for both spellings, before execution, with live's error class and
text. The unbound-endpoint and open-start closed-trail expression shapes
are not expressible.

The `MATCH`-clause forms are unchanged: open endpoints, closed trails for
the keyword selectors, and the zero-length path when the lower bound is 0
remain available.

## Alternatives considered

- **Keep the open-endpoint expression shapes.** Rejected: live rejects
  the syntax, the lowering has no partition for an unbound end, and a
  second search driver would have to answer it — the divergence ADR-0568
  removes.
- **Gate the narrowing behind a preview feature.** Rejected: a preview
  gate widens the floor's accepted syntax, while this is a compile-time
  rejection matching the floor; the registry gates syntax the floor
  rejects, not a narrowing of accepted syntax.
- **Leave the shapes to runtime `null`.** Rejected: live raises `42001` at
  compile time regardless of the graph, and a `null` would hide a
  non-portable query.

## Consequences

- ADR-0189's expression-form caller list is narrowed: the unbound-end and
  open-start shapes it specifies are rejected at compile time. The clause
  route and the shared search module are unchanged.
- ADR-0568's lowering contract is unchanged for bound endpoint pairs.
- The expression form's open-end search was already removed by the
  ADR-0568 lowering; this decision records the validation rule that keeps
  the shape out.
- Queries that relied on the old open-endpoint expression form must bind
  both endpoints.
