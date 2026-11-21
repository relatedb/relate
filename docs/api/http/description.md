# HTTP Query API v2 — description

The HTTP surface lets HTTP-first tooling run Cypher against an embedded
Relate `Database` without a Bolt driver: one `POST` of a JSON statement
envelope to `/db/{databaseName}/query/v2`, one JSON response envelope back.
It speaks the Neo4j Query API v2 statement shape (ADR-0232 §3), so existing
Query API clients — and the reverse direction of `relate-load`
(ADR-0080), which reads this shape from Neo4j/Aura — work against Relate
unchanged.

Typed requests accept `application/vnd.neo4j.query`, the legacy v1.0
alias used by hosted Neo4j Browser and Console, as well as versioned
Query API media types. Plain `application/json` requests keep plain
parameter semantics.

The surface is deliberately the charter's minimal subset: no managed
transactions, no routing or discovery endpoints, no batching. Each request is
an independent auto-commit statement against one of the fixed namespaces
(`neo4j`, `system`, `system_ontologies`, ADR-0116).

Authentication is the same generated-token basic-auth story as Bolt
(ADR-0126, ADR-0232 §1): one credential — from `--auth`,
`RELATE_AUTH`, or a token generated at startup — carried here as an
`Authorization: Basic` header. Row values use the same kind-tagged wire JSON
as every other surface (ADR-0223), so a node decoded from the HTTP envelope
is byte-identical to one from the CLI's JSON mode or the C ABI.

Responses are gzip-compressed when the client advertises it and the
serialized envelope is at least 1 KiB (ADR-0385): an additive transport
extension — the JSON envelope, status and content type are unchanged, a
compressed reply carries `Content-Encoding: gzip` and `Vary:
Accept-Encoding`, and a client that sends no `Accept-Encoding` gets the
identity representation.

Answers whose serialized envelope reaches 65536 bytes are framed with
`Transfer-Encoding: chunked` instead of `Content-Length` (ADR-0386): the
success envelope is written `data.fields` first, then its materialized rows
one at a time, then the plan / counters / notifications / bookmarks tail.
Above the threshold that removes the body-sized serialized copy the
pre-ADR-0386 writer built with `serde_json::to_vec`; the result rows and the
envelope's parts are still materialized before the reply seam (the engine has
no cursor, ADR-0386 §6). The chunked bytes pass through a fixed 8 KiB frame
buffer and no frame payload exceeds it, so where one chunk frame ends is a
transport detail no client may depend on. Gzip composes above the chunk
writer, and clients that decode both (`curl`, `ureq`, the Neo4j drivers) see
the same JSON envelope as a small one-shot reply.

The server is minimal HTTP/1.1 (no hyper/axum; request-head parsing via
`httparse`, ADR-0234) in `surfaces/relate-http`, exported as `serve` /
`serve_handler` so the `relate serve --http PORT` CLI node (ADR-0232
Consequences) can embed it. Only HTTP/1.1 requests are answered; a request
line declaring any other version is refused with the malformed-request 400,
so the HTTP/1.1-only chunked framing never reaches a 1.0 peer (ADR-0386 §8).

See [spec.md](spec.md) for the exact route, envelope, and error-code
contract, and [examples/query.md](examples/query.md) for a runnable
walkthrough.

A seek-started `MATCH` in a `queryPlan`/`profiledQueryPlan` tree nests display-only fused children — the seek step and one `Expand` per hop — marked by `FusedInto` in `arguments` (ADR-0622). They carry no per-step rows, time or memory; the parent's totals stand.
