---
type: adr
area: surface
claim: 4df4360904944100c47c59e38c426ffa11288f3c
name: 0425-typed-request-wire-alignment
status: proposed
tags:
  - http
title: Query API request media types select typed or plain parameters
REFERENCES:
  - "[[adr/0236-query-api-v2-value-encoding]]"
---

# ADR-0425: Query API request media types select typed or plain parameters

## Context

The Query API v2 distinguishes plain JSON parameters from typed
`{"$type": …, "_value": …}` envelopes, including nested `List` and `Map`
values. Under `application/json`, an envelope-shaped object is a user
map; decoding it as a typed value changes the query's meaning.

The typed request vocabulary includes the unversioned
`application/vnd.neo4j.query` as the legacy v1.0 alias. Neo4j registers
it with the same request reader as `application/vnd.neo4j.query.v1.0`.
Both spellings accept typed parameters on Neo4j 2026.10.1. The hosted
Neo4j Browser and Console send the unversioned type through their
shared Query API wrapper, so rejecting it prevents their connectivity
probe from running even though the typed payload is supported.

## Decision

1. **The request media-type vocabulary** is `application/json` (plain
   parameters), `application/vnd.neo4j.query` (the legacy v1.0 typed
   alias), and the versioned Query API family, including `.v1.0` and
   `.v1.2` (typed envelopes). The statement routes share one dialect
   resolver for auto-commit, begin, run, and commit. Media-type matching
   ignores case and parameters such as `charset=utf-8`.

   The browser guard owns only the CSRF property: it rejects
   browser-"simple" content types (`text/plain`, the form types, none)
   and passes everything else through to the route. Unsupported request
   types receive the route's 400 with the accepted spellings.

2. **Envelope decoding is bound to the request media type.** The one
   parameters fold (`typed_params`) takes the request dialect: the
   typed request decodes `{"$type","_value"}` envelopes
   (today's decoder, unchanged); a plain `application/json` request
   uses plain JSON semantics — an envelope-shaped object is a user
   **map**, byte-for-byte what a real server delivers to Cypher.

3. **MCP parameters are plain.** The MCP tools' parameters ride the
   MCP protocol's JSON — plain semantics; an envelope-shaped tool
   argument stays a map, exactly as over `application/json`.

4. **The response side is unchanged** — the typed Accept negotiation
 (the versioned ladder, [ADR-0236](./0236-query-api-v2-value-encoding.md)) is the real, verified response
   wire and stays as is.

5. **Regression coverage** checks typed request aliases, plain map
   semantics, the Browser connectivity probe over HTTP, and typed
   parameters across the managed-transaction routes.

## Alternatives considered

- **Keep the lenient decode (envelopes under any JSON request)** —
  rejected: envelope-shaped user maps must remain maps under
  `application/json`.
- **Accept only versioned typed media types** — rejected: Neo4j accepts
  the unversioned v1.0 alias, and the hosted clients still send it.
- **Reject envelope parameters entirely (plain-only requests)** —
  rejected: the v1.2 envelope dialect is real, verified, and what
  official clients send; our own push client sends it.

## Consequences

- Hosted Browser and Console requests reach the existing typed decoder
  without a proxy or a client-side header rewrite.
- Clients that relied on envelope parameters under plain
  `application/json` (none known — our surfaces send v1.2 or plain)
  must use a typed request content type.
- Response negotiation, authentication, and origin restrictions are
  unchanged. Accepting the alias does not add new parameter types.
