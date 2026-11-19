---
type: adr
area: ecosystem
date: 2026-08-28
deciders: MemoryDB maintainers
name: 0055-apache-2-license
status: accepted
tags:
  - licensing
title: MemoryDB is licensed under Apache License 2.0
REFERENCES:
  - "[[adr/0001-cip-eval-ratcheted-gherkin-eval]]"
---

# 0055. MemoryDB is licensed under Apache License 2.0

## Context

The project needs a license before any code is published. Contributors and
downstream embedders (including companies that ship MemoryDB inside products)
need a clear grant of copyright and patents.

## Decision

MemoryDB is licensed under the Apache License, Version 2.0. The license text
lives in [`LICENSE`](../../LICENSE). All original source and documentation in
this repository is covered by that license.

## Alternatives considered

- **MIT** — simpler text. Rejected because it has no
  explicit patent grant; Apache-2.0 is the usual choice for databases that
  expect commercial embedding.
- **GPL / AGPL** — copyleft would block proprietary embedding, which is the
  DuckDB-style use case. Rejected as incompatible with “embed anywhere.”
- **BSL / dual license** — would complicate early contribution and TCK
  consumption (openCypher TCK is Apache-2.0). Rejected for a greenfield
  open-source engine.

## Consequences

- Downstream users may embed MemoryDB in proprietary products.
- Contributions are Apache-2.0 unless a separate CLA is introduced later
  (that would be a new ADR).
- Vendored openCypher TCK files remain Apache-2.0, matching this license.

## Later references

- Renumbered from [ADR-0001](./0001-cip-eval-ratcheted-gherkin-eval.md) to [ADR-0055](./0055-apache-2-license.md): the CypherLite/MemoryDB merge
  imported the MemoryDB ADR series numbered from 0001 while this repo already
  had ADRs 0001–0003. The main-era three were the minority (50+ files use the
  imported numbering), so they moved to 0055/0056/0057 to keep every number
  unique. This ADR was formerly `0001-apache-2-license.md`.
