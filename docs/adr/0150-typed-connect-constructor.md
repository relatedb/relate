---
type: adr
area: surface
date: 2026-09-29
deciders: CypherLite maintainers
name: 0150-typed-connect-constructor
status: accepted
tags:
  - namespaces
title: Typed connect constructor in the Rust API
REFERENCES:
  - "[[adr/0117-namespace-selection-apis-and-bolt]]"
  - "[[adr/0146-unknown-namespace-selection-is-graph-not-found]]"
  - "[[adr/0147-typed-namespace-values]]"
---

# 0150. Typed connect constructor in the Rust API

## Context

[ADR-0117](./0117-namespace-selection-apis-and-bolt.md) §2 introduced namespace selection by name:
`Database::connect_namespace(&str)` parses the session namespace at
runtime and `Database::connect()` delegated to it with `"neo4j"`. Callers
that already hold a typed `Namespace` ([ADR-0147](./0147-typed-namespace-values.md)) had to flatten it to a
string and re-parse — `connect()` itself did this for `Namespace::Neo4j`,
and the CLI (`main.rs`) and Python `reset()` repeated the same
typed→string→typed round trip. A stellar review of PR #127 flagged the
pattern (std-4d: runtime introspection of a statically-known shape).

## Decision

Add `Database::connect_typed(namespace: Namespace)` as the canonical
constructor: no name parsing, so no `DatabaseNotFound` is possible.
`Database::connect()` delegates with `Namespace::Neo4j`;
`Database::connect_namespace(&str)` parses the name ([ADR-0117](./0117-namespace-selection-apis-and-bolt.md) §2
semantics and the [ADR-0146](./0146-unknown-namespace-selection-is-graph-not-found.md) error text are unchanged) and delegates to
`connect_typed`. The string-based FFI and client APIs ([ADR-0117](./0117-namespace-selection-apis-and-bolt.md) §3–§4)
are unchanged.

## Alternatives considered

- **Do nothing** — rejected: every typed caller pays a runtime parse of a
  statically-known value, and the stringly round trip invites drift
  between `Namespace::as_str` and the parser.
- **Replace the stringly Rust API with the typed one** — rejected: a
  breaking change for `connect_namespace(&str)` callers, and parsing at
  the boundary is exactly right when the input is genuinely a string
  (CLI flags, FFI, Python).
- **One generic `connect<N: TryInto<Namespace>>`** — rejected: implicit
  conversion hides the boundary; the explicit pair keeps "have a name"
  and "have a namespace" visibly distinct at call sites.

## Consequences

- `docs/api/rust/` (spec, description, namespace example) documents
  `connect_typed`; the method carries a doctest and the runnable example
  exercises it in CI.
- The CLI connects via `connect_typed`; Python `reset()` reopens with
  the typed namespace instead of stringifying it.
- `Connection::set_namespace(&str)` stays stringly — its callers (FFI,
 Python) hold names per [ADR-0117](./0117-namespace-selection-apis-and-bolt.md) §3–§4. A typed variant can follow this
  pattern if a typed caller appears.
