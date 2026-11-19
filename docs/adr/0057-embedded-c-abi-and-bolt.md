---
type: adr
area: surface
date: 2026-08-28
deciders: MemoryDB maintainers
name: 0057-embedded-c-abi-and-bolt
status: accepted
tags:
  - bolt
  - ffi
title: Embedded C ABI is the client boundary; Bolt is optional remote transport
REFERENCES:
  - "[[adr/0003-tck-named-error-kinds]]"
  - "[[adr/0008-localhost-http-query-api]]"
  - "[[adr/0029-withdraw-localhost-http]]"
  - "[[adr/0056-opencypher-tck-language-contract]]"
---

# 0057. Embedded C ABI is the client boundary; Bolt is optional remote transport

## Context

DuckDB-style databases are in-process. Users also asked for clients in major
languages and for existing Neo4j drivers to connect. Those are two different
surfaces: a stable embed ABI, and a network protocol.

## Decision

1. The engine is an **embedded library**. The public Rust crate
   (`memorydb`) is first-class for Rust users.
2. The **C ABI** (`memorydb-ffi`, documented in `docs/api/c/`) is the
   multi-language boundary. Wave-1 bindings (Python, JavaScript, Java, Go)
   wrap that ABI and do not invent a second wire protocol.
3. A **Bolt** server (`memorydb-bolt`) is optional remote transport so
   official Neo4j drivers can `RUN` OpenCypher. Bolt is not Neo4j Cypher
 parity ([ADR-0056](./0056-opencypher-tck-language-contract.md)). Supported Bolt versions are documented in
   `docs/api/bolt/`.

Do not add a client language until the C ABI for the used features is
documented.

## Alternatives considered

- **Embedded only, no Bolt** — lighter, but existing Neo4j drivers could not
  connect. Rejected by product choice.
- **Bolt only, no C ABI** — forces a network hop for in-process users and
  makes Python/JS/Java/Go bindings heavier. Rejected; embed is the default.
- **HTTP/OpenAPI instead of Bolt** — easier to document, but does not reuse
  Neo4j drivers. Rejected; Bolt covers remote. HTTP would be a new ADR.
- **Per-language native bindings to Rust (no C ABI)** — PyO3/JNI/cgo each
  bind a shifting Rust surface. Rejected as the *published* boundary; C ABI
  is the stable contract even if some bindings call Rust internally during
  bring-up.

## Consequences

- FFI must be ABI-stable, opaque handles, explicit free.
- Bolt CI uses official Neo4j drivers as contract tests.
- Driver users get OpenCypher only; Neo4j-specific queries fail.
- A second remote protocol (HTTP, Arrow Flight) requires a new ADR.

## Later references

[ADR-0029](./0029-withdraw-localhost-http.md) removes the localhost HTTP sidecar ([ADR-0008](./0008-localhost-http-query-api.md)). This repo does
not ship an HTTP query API. Hosted HTTP belongs to clients implementing
embedded Relate, not a second engine protocol.

Renumbered from [ADR-0003](./0003-tck-named-error-kinds.md) to [ADR-0057](./0057-embedded-c-abi-and-bolt.md): the CypherLite/MemoryDB merge
imported the MemoryDB ADR series numbered from 0001 while this repo already
had ADRs 0001–0003. The main-era three were the minority (50+ files use the
imported numbering), so they moved to 0055/0056/0057 to keep every number
unique. This ADR was formerly `0003-embedded-c-abi-and-bolt.md`.
