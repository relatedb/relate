---
type: adr
area: engine
date: 2026-09-28
deciders: CypherLite maintainers
name: 0149-ce-kind-tokens-from-typed-error-model
status: accepted
tags:
  - conformance
  - errors
title: CE kind tokens come from the typed error model
REFERENCES:
  - "[[adr/0003-tck-named-error-kinds]]"
  - "[[adr/0103-engine-surfaces-testing-layout]]"
  - "[[adr/0116-fixed-namespaces-community-edition-semantics]]"
  - "[[adr/0117-namespace-selection-apis-and-bolt]]"
---

# 0149. CE kind tokens come from the typed error model

## Context

The CE error-text formatters in `cypherlite-core/src/ce_messages.rs`
([ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md) §9, [ADR-0117](./0117-namespace-selection-apis-and-bolt.md) §6) embed a `"<Kind>:"` token in every message, and
that token is a load-bearing wire contract: the bindings' name scan
(`cypherlite::binding_error_metadata`) recovers the kind from it. The
tokens were restated as string literals in each formatter while the typed
model — `cypherlite_error::ErrorKind` / `TckErrorClass`, whose `as_str()`
is the canonical string form of the same names ([ADR-0003](./0003-tck-named-error-kinds.md)) — already
existed. Two hand-written copies of one token list drift. Flagged in
review of the fixed-namespaces work.

## Decision

`cypherlite-core` depends on `cypherlite-error`, and `ce_messages` takes
each kind token from the typed model:
`cypherlite_error::ErrorKind::<Kind>.as_str()` for the kind-level tokens
(`UnsupportedAdministrationCommand`, `DatabaseNotFound`,
`WriteOnReadOnlyAccessDatabase`, `TransactionError`) and
`cypherlite_error::TckErrorClass::SemanticError.as_str()` for the
class-level `SemanticError` token, which has no `ErrorKind` variant. The
rendered messages are byte-identical (pinned by the existing `ce_messages`
tests and `testdata/ce-errors/neo4j-ce.json`). The new edge is legal under
the [ADR-0103](./0103-engine-surfaces-testing-layout.md) layering rules (R1: `cypherlite/*` may depend on
`cypherlite/*`) and creates no cycle: `cypherlite-error` depends on
`cypherlite-parser`, which depends on `cypherlite-model` only.

## Alternatives considered

- **Module-level `const`s in `ce_messages` with a cross-crate test**
  asserting each token equals the corresponding `ErrorKind::as_str()`.
  Rejected: the token text would still be written twice (const literal and
  variant name), with a test detecting drift after the fact instead of one
  definition generating both.
- **Move `ce_messages` into `cypherlite-error`.** Rejected: the formatters
  produce `cypherlite_core::Error`, so this would require the inverse edge
  (`cypherlite-error` → `cypherlite-core`), making the low-level typed
  error channel depend on the much larger core crate — a worse direction
  than the chosen one.
- **Keep the literals.** Rejected: that is the duplication being removed.

## Consequences

- `cypherlite-core` gains one workspace dependency (`cypherlite-error`);
  its transitive closure gains `cypherlite-parser` (already pulled in by
  every planner/executor build).
- Renaming an `ErrorKind` variant now renames the CE wire token with it.
 Per [ADR-0003](./0003-tck-named-error-kinds.md) a kind rename is already a breaking change, so the coupling
  makes an existing invariant explicit rather than adding a new one.
- `cargo xtask check-boundaries` covers the new edge (R1).
