# ADR-0236: Query API v2 value encoding follows the Neo4j contract

Status: Accepted

## Context

The HTTP Query API surface (ADR-0232 §3) initially encoded result values
with ADR-0223's kind-tagged wire JSON (`{"kind":"node","id":…}`). Neo4j
Browser / driver clients connecting to it failed to decode responses: their
Query API hydration dispatches on a `$type` tag, so our `kind`-tagged
objects read as "Unknown type: undefined" (a protocol error on connect).

The Query API v2 value contract, from the Neo4j server
(`community/server/.../queryapi/response/format/DefaultResponseModule`) and
the clients' hydration codecs (e.g. the Python driver's
`_codec/hydration/http`): the plain form (`application/json`) renders plain
JSON — scalars as-is, entities as plain objects (`elementId`, `labels`,
`properties`; relationships add `startNodeElementId`/`endNodeElementId`/
`type`) — and the typed form (`application/vnd.neo4j.query`) wraps every
value as `{"$type": <Cypher type>, "_value": …}` with `_element_id`,
`_labels`, `_properties` (and `_start_node_element_id` /
`_end_node_element_id` / `_type`) inside, integers and floats as strings in
`_value`, ISO-8601 strings for temporals, and a `Path` as an alternating
node/relationship array.

## Decision

`surfaces/cypherlite-http` encodes `data.values` per the Neo4j Query API v2
contract in both forms (plain for `application/json`, typed for the
negotiated `application/vnd.neo4j.query` media type, ADR-0232's negotiation
per the Accept header). Geometry — a CypherLite extension the Neo4j codecs
do not know — is `{"$type": "Unsupported", "_value": …}` in the typed form
and keeps the ADR-0223 shape in the plain form.

ADR-0223's kind-tagged codec is unchanged and stays the encoding for
snapshots, the CLI, and the MCP tools.

## Alternatives considered

- **Keep ADR-0223's encoding and document it for our own clients.**
  Rejected: the whole point of the surface is Neo4j tooling compatibility;
  no official client can decode it.
- **Extend ADR-0223's codec to the `$type`/`_value` shape everywhere.**
  Rejected: it would churn the snapshot format and every other surface
  (CLI recordings, MCP tool payloads) for the benefit of one HTTP contract
  that already exists and differs in details (string-encoded numbers).

## Consequences

- The Query API v2 surface is decodable by the official clients; the typed
  and plain forms are pinned by unit tests in `query_api.rs` and the
  Browser connectivity probe (`CALL db.ping()` with `includeCounters`) is
  an explicit fixture.
- Two value encodings exist by design, each documented where it lives
  (`cypherlite-model::wire` for ADR-0223; `query_api::v2_value_json` for
  this surface). `docs/api/http/spec.md` carries the contract.

Later references: landed on the `serve-network-surfaces` branch together
with ADR-0235 (`db.ping`) and the media-type negotiation.
  The request side of the same contract landed with it: request
  `parameters` values in the typed form are decoded to plain JSON before
  `params_from_value` (`cypherlite_http::typed_params`, also used by the
  MCP tools), with the exactly-two-keys ambiguity rule.
  ADR-0425 later aligned the request side with the real servers: the
  envelope decode binds to the typed Query API content types, including
  the unversioned v1.0 alias; plain `application/json` parameters keep
  an envelope-shaped object a map, and the MCP tools take the plain fold.
