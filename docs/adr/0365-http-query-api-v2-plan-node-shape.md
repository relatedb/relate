---
type: adr
area: surface
claim: 3c58bc85e11d6188a86c46ae434985cb1e13a4fe
name: 0365-http-query-api-v2-plan-node-shape
status: proposed
tags:
  - http
  - observability
title: The HTTP Query API v2 plan node shape
REFERENCES:
  - "[[adr/0357-plan-summary-placement-and-neo4j-plan-args]]"
---

# ADR-0365: The HTTP Query API v2 plan node shape

## Context

[ADR-0357](./0357-plan-summary-placement-and-neo4j-plan-args.md) put the structured plan on every surface in the Neo4j-facing shape
the Bolt driver reads: a node is `operatorType` + `args` (the metrics inside
`args`) + `identifiers` + `children`. That shape is what the **driver**
expects — the driver converts `args` to its own `arguments`.

The HTTP Query API v2 client is not the driver: Neo4j Browser's HTTP path
decodes the response with its typed-JSON codec and reads each node's args
under **`arguments`**, with the metrics as **node fields**. Serving the Bolt
shape there crashed Browser's plan view with
`TypeError: Cannot read properties of undefined (reading
'string-representation')` (LABS-376). Capturing Neo4j 5.26's own
`POST /db/neo4j/query/v2` PROFILE response settled the contract:

```
{"data":{"fields":[…],"values":[…]},
 "profiledQueryPlan":{
   "operatorType":"ProduceResults@neo4j",
   "arguments":{"Id":0,"Details":"1 item(s)","EstimatedRows":1.0,
                "planner-impl":"IDP","string-representation":"…","version":"5", …},
   "identifiers":[],"children":[…],
   "dbHits":0,"records":1,"time":0,"hasPageCacheStats":false,
   "pageCacheHits":0,"pageCacheMisses":0,"pageCacheHitRatio":0.0}}
```

## Decision

1. **The HTTP Query API v2 plan node is the v2 shape, not the Bolt wire
   shape.** `queryPlan` / `profiledQueryPlan` carry nodes with
   `operatorType` (suffixed `@cypherlite`; Neo4j writes `@neo4j`),
   **`arguments`** (never `args`), `identifiers`, `children` (always
   present; a leaf's is the empty list, matching Neo4j), `warnings` when
   the node carries any, and the metric fields `dbHits`, `records`, `time`
   (fractional milliseconds), `hasPageCacheStats` (always `false` — the
   engine is in-memory), `pageCacheHits`/`pageCacheMisses` (0) and
   `pageCacheHitRatio` (0.0).
2. **`arguments` is the engine's own args map, projected, not rebuilt.**
   It carries the display and identity args (`Id`, `Details`,
   `EstimatedRows`, and on the root `version`, `planner*`, `runtime*`,
   `Memory`, `GlobalMemory`, `string-representation`) *and* the engine's
   PROFILE metric entries (`Rows`, `TimeNs`, `DbHits`, `MemoryBytes`) —
   Neo4j's own `arguments` keeps its `Rows`/`DbHits` there too, so the
   richer internal form survives the projection instead of being stripped.
   The node-level metric fields (`dbHits`, `records`, `time`) are the same
   values in Neo4j's field spelling: the surface adapts the seam, the
   engine keeps one form.
3. **The node is read through the canonical accessor**
   (`cypherlite_model::wire::plan_node_wire`) and the single-sourced key
   vocabulary (`wire::plan_keys`): the wire form is engine-built and
   invariant, so a malformed node fails loudly there instead of being
   defaulted at the boundary. The v2 args key `arguments` is a local
   constant in the HTTP surface (the engine's and Bolt's key is `args`).
4. **The CORS preflight allow-list admits the headers Browser-like tooling
   sends** — `sentry-trace` and `baggage` beyond `Authorization` and
   `Content-Type` — so the discovery probe is not rejected before any query
   runs.

## Alternatives considered

- **Serve the Bolt wire shape on both surfaces** — rejected: it is the
  driver's shape, and the HTTP client decodes `arguments`; serving `args`
  there crashes the plan view (the defect this ADR fixes).
- **Serve the v2 shape on Bolt too** — rejected: the Bolt summary is
  consumed by official drivers, which expect `args` and convert it
  themselves; changing it would break every driver to satisfy one HTTP
  client.
- **Strip the engine metric args from `arguments`** — considered and
  rejected on the evidence: Neo4j 5.26's own response keeps `Rows` and
  `DbHits` in `arguments` alongside the node fields, so stripping them
  would diverge from the reference and drop the engine's richer
  `TimeNs`/`MemoryBytes` from the projection. The metric riding both
  `arguments` and the node fields is Neo4j's own shape, not a duplicated
  fact invented here.
- **Synthesize JSONL for the `application/vnd.neo4j.query.v1.0+jsonl`
  Accept entry** — rejected: Neo4j 5.26 answers that media type with
  `Not Acceptable` and serves `application/json` for the same client list,
  so the engine negotiates to a supported entry instead of inventing a
  framing Neo4j does not ship.

## Consequences

- `docs/api/http/spec.md` documents the v2 node shape (fields, the
  `arguments` rename, the metric placement, the CORS allow-list) in the same
  change.
- Browser-like HTTP tooling renders this engine's plans unmodified; the Bolt
 wire shape stays as [ADR-0357](./0357-plan-summary-placement-and-neo4j-plan-args.md) defined it, so the drivers are untouched.
- Result metadata stays aligned: the divergence between the Bolt wire shape
  and the HTTP v2 shape is this ADR's subject, not an accident — both carry
  the same facts, each in the shape its consumer reads.
