# 0029. Withdraw the localhost HTTP sidecar

- **Status**: Accepted
- **Date**: 2026-08-31
- **Deciders**: MemoryDB maintainers

## Context

ADR-0008 shipped `memorydb-http` as a localhost-only JSON query API for
the explorer. This repository does not own a hosted HTTP plane; clients
implementing embedded Relate own any hosted HTTP query surface. Keeping
a second HTTP stack in this repo would split the client contract and
re-litigate ADR-0057’s “HTTP is not the public remote protocol.”

Bolt remains the optional remote transport in this repo (ADR-0057).

## Decision

Remove the HTTP product surface from MemoryDB:

- Delete the `memorydb-http` crate and `docs/api/http/`.
- The explorer and `POST /query` client are not part of this repository.
  Clients implementing embedded Relate own that surface.
- This repository’s public surfaces are the Rust crate, C ABI, language
  clients, CLI, and Bolt.

ADR-0008 is withdrawn here, not edited in place.

## Alternatives considered

- **Keep `memorydb-http` for local explorer** — two HTTP contracts.
  Rejected; hosted HTTP belongs to clients implementing embedded Relate.
- **Promote engine HTTP to a hosted edge** — rejected; hosted HTTP
  belongs to clients implementing embedded Relate.
- **Treat Bolt as a hosted API in this change** — out of scope. Bolt
  stays the optional remote transport in this repo (ADR-0057). Hosted
  access belongs to clients implementing embedded Relate.

## Consequences

- Local use is the CLI and the embed API.
- CI no longer builds or tests `memorydb-http`.
- A later return of engine HTTP would be a new ADR.
