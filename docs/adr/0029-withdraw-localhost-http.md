---
type: adr
area: surface
date: 2026-08-31
deciders: MemoryDB maintainers
name: 0029-withdraw-localhost-http
status: accepted
tags:
  - http
title: Withdraw the localhost HTTP sidecar
REFERENCES:
  - "[[adr/0008-localhost-http-query-api]]"
  - "[[adr/0057-embedded-c-abi-and-bolt]]"
  - "[[adr/0232-network-surfaces-charter]]"
  - "[[adr/0237-query-api-tx-routes]]"
---

# 0029. Withdraw the localhost HTTP sidecar

## Context

[ADR-0008](./0008-localhost-http-query-api.md) shipped `memorydb-http` as a localhost-only JSON query API for
the explorer. This repository does not own a hosted HTTP plane; clients
implementing embedded Relate own any hosted HTTP query surface. Keeping
a second HTTP stack in this repo would split the client contract and
re-litigate [ADR-0057](./0057-embedded-c-abi-and-bolt.md)’s “HTTP is not the public remote protocol.”

Bolt remains the optional remote transport in this repo ([ADR-0057](./0057-embedded-c-abi-and-bolt.md)).

## Decision

Remove the HTTP product surface from MemoryDB:

- Delete the `memorydb-http` crate and `docs/api/http/`.
- The explorer and `POST /query` client are not part of this repository.
  Clients implementing embedded Relate own that surface.
- This repository’s public surfaces are the Rust crate, C ABI, language
  clients, CLI, and Bolt.

[ADR-0008](./0008-localhost-http-query-api.md) is withdrawn here, not edited in place.

## Alternatives considered

- **Keep `memorydb-http` for local explorer** — two HTTP contracts.
  Rejected; hosted HTTP belongs to clients implementing embedded Relate.
- **Promote engine HTTP to a hosted edge** — rejected; hosted HTTP
  belongs to clients implementing embedded Relate.
- **Treat Bolt as a hosted API in this change** — out of scope. Bolt
 stays the optional remote transport in this repo ([ADR-0057](./0057-embedded-c-abi-and-bolt.md)). Hosted
  access belongs to clients implementing embedded Relate.

## Consequences

- Local use is the CLI and the embed API.
- CI no longer builds or tests `memorydb-http`.
- A later return of engine HTTP would be a new ADR.

## Later references

[ADR-0232](./0232-network-surfaces-charter.md) (network surfaces charter) restored an HTTP Query API v2 endpoint as
a product surface alongside Bolt and MCP, superseding this decision's
withdrawal; [ADR-0237](./0237-query-api-tx-routes.md) defines its transaction routes. The decision text above
is the historical record and is unchanged.
