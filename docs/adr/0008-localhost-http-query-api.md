# 0008. Localhost-only HTTP/JSON query API for tools and the explorer

- **Status**: Accepted
- **Date**: 2026-08-28
- **Deciders**: MemoryDB maintainers

## Context

ADR-0057 rejected HTTP as the *remote* protocol: Bolt is how official Neo4j
drivers talk to MemoryDB, and the C ABI is the embed boundary. The Bloom-style
explorer and other local tools still need a simple JSON query surface that a
browser can call without a Bolt stack.

## Decision

Ship a **localhost-only** HTTP sidecar (`memorydb-http`):

- Default bind: `127.0.0.1` (not `0.0.0.0`). Binding another address is opt-in
  and documented as unsafe for untrusted networks.
- `POST /query` with `{ "cypher", "params" }` returns columns, rows, and stats.
- `GET /health` for liveness.
- Open a URI (`:memory:` or a Parquet snapshot) or a built-in dataset
  (IMDb / fraud) at process start.
- This API does **not** replace Bolt or the C ABI. It is not the public remote
  protocol.

Documented in [`docs/api/http/`](../api/http/).

## Alternatives considered

- **Reuse Bolt from the browser** — browsers do not speak Bolt; a JS Bolt
  client would be a large new surface. Rejected for the explorer.
- **Promote HTTP to the public remote protocol** — duplicates ADR-0057 and
  splits driver support. Rejected.
- **Unix socket + JSON** — awkward for a web UI. Rejected.
- **Embed the explorer in the Bolt server** — mixes protocol stacks. Rejected;
  keep a small sidecar.

## Consequences

- Tools can run Cypher against a local engine without Neo4j drivers.
- Operators must not expose `memorydb-http` on a public interface.
- Later remote HTTP would require a new ADR (auth, TLS, multi-tenant).

## Later references

ADR-0029 withdraws this sidecar. `memorydb-http` and `docs/api/http/`
are removed. Hosted HTTP belongs to clients implementing embedded
Relate, not to this engine. The “localhost HTTP for the explorer”
decision no longer holds.
