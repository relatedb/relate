# ADR-0373: Export the plain-JSON Query API plan field

Status: Accepted

Claim: 2928220f99c4c103845827d87a9df560f0e271bb

## Context

ADR-0365 defines the HTTP Query API v2 plan node: `queryPlan` for
`EXPLAIN`, `profiledQueryPlan` for `PROFILE`, with `arguments` (not the
Bolt wire's `args`) and the metric fields Neo4j's own response carries.
That projection lives in `cypherlite-http` as `plan_field`, and it is
`pub(crate)`. The HTTP server is the only caller.

An embedder that builds its own success envelope — a client
implementing embedded Relate — receives a `QueryResult` whose
`plan` is already set, then has to encode the tree. Copying the walk
would be a second spelling of ADR-0365. The Bolt summary is the wrong
shape for an HTTP client.

## Decision

Publish `cypherlite_http::query_plan_field(tree) -> (&'static str, Value)`.

It calls `plan_field(tree, false)` and returns that function's key and
plain-JSON node. `EXPLAIN` yields `queryPlan`. `PROFILE` yields
`profiledQueryPlan`. A tree that is neither is not produced by the
engine; the key still follows `PlanNode::is_profiled`.

The typed `{"$type","_value"}` encoding stays inside the HTTP server,
which keeps calling `plan_field` with the flag its Accept negotiation
chose. Embedders that speak plain JSON do not grow a second entry point
for a negotiation they do not perform.

## Alternatives considered

- **Copy the node walk into each embedder** — rejected: ADR-0365 has one
  spelling of the v2 node, and a second copy drifts from it.
- **Also publish the typed variant** — rejected: the public entry is the
  plain-JSON contract. The server already selects typed encoding from
  Accept, and an embedder that needs that form is the HTTP server.
- **Return the Bolt `args` shape** — rejected: HTTP clients read
  `arguments` (ADR-0365). Serving `args` on this field is the defect
  ADR-0365 closed.

## Consequences

- `docs/api/http/spec.md` names `cypherlite_http::query_plan_field` as
  the embedder entry point for the plain-JSON plan field.
- Embedders attach the returned key and value on their own success
  envelope. They do not reimplement the walk.
- The HTTP server's typed plan encoding is unchanged.
