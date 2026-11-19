---
type: adr
area: surface
name: 0220-tabular-abi-single-entry-points
status: accepted
tags:
  - ffi
title: Tabular C ABI — one canonical entry point per operation
REFERENCES:
  - "[[adr/0054-one-c-abi-env-python]]"
  - "[[adr/0117-namespace-selection-apis-and-bolt]]"
  - "[[adr/0502-typed-c-abi-v2]]"
---

# ADR-0220: Tabular C ABI — one canonical entry point per operation

## Context

The tabular / multi-connection C ABI ([ADR-0054](./0054-one-c-abi-env-python.md), `cypherlite-ffi`'s
`tabular.rs`) grew two paired forms for each of its two core operations:

- `cypherlite_connect(db, &conn)` and
  `cypherlite_connect_namespace(db, name, &conn)`
- `cypherlite_query(conn, cypher, &result)` and
  `cypherlite_query_params(conn, cypher, json_params, &result)`

Each pair is one operation with an optional trailing argument that gained
its own entry point when the argument was added (namespaces in [ADR-0117](./0117-namespace-selection-apis-and-bolt.md),
parameters earlier). That leaves four exported symbols for two operations,
and every language client (JS/koffi, Java/JNA, Go/cgo) binds both forms and
hand-picks between them — duplication in every client for a distinction a
NULL pointer already expresses. `cypherlite_query_params` already accepted
`NULL` for `json_params`; `cypherlite_connect_namespace` rejected a NULL
`name`, so there was no single connect entry point a client could always
call. LABS-288 tracks the collapse.

## Decision

One canonical entry point per operation, with NULL meaning "absent":

- `cypherlite_connect_namespace(db, name_or_null, &conn)` is **the**
  connect entry point. A NULL `name` means the default `neo4j` namespace
  (exactly what `cypherlite_connect` did). A non-NULL `name` behaves as
  before.
- `cypherlite_query_params(conn, cypher, json_or_null, &result)` is **the**
  query entry point. A NULL (or empty) `json_params` means no parameters —
  unchanged behavior.

`cypherlite_connect` and `cypherlite_query` remain exported as thin,
documented aliases that forward to the canonical form with NULL, so
existing C-ABI consumers keep compiling and linking. They are deprecated in
docs and may be removed in a future major version; the header comment and
`docs/api/c/spec.md` mark them as aliases.

Language clients bind only the canonical forms: the JavaScript client drops
its `cypherlite_connect` / `cypherlite_query` bindings and passes `null`
for the absent argument (koffi marshals JS `null` to a NULL `str` pointer);
the Go client calls `cypherlite_query_params` with a NULL json pointer when
it has no parameters; the Java client drops the paired JNA declarations and
passes `null` (JNA marshals a null `String` as NULL).

This is an ABI addition plus a contract widening (NULL `name` becomes
legal), not a removal: no previously valid call changes meaning, and no
exported symbol is dropped.

## Alternatives considered

- **Remove the paired functions outright** — rejected: a hard ABI break for
  every existing C consumer (and older client binaries) with no migration
  window; the alias cost is four lines of forwarding each.
- **Keep both pairs as equals and just document a preference** — rejected:
  clients keep paying the double binding and the branch; the point of the
  change is that NULL already encodes the optionality.
- **Keep `cypherlite_connect` canonical and add a separate
  `cypherlite_connect2` with a flags/namespace struct** — rejected: a new
  name for the same operation is worse than widening the existing
  namespace-taking form, and a struct is overkill for one optional string.
- **Make clients keep binding the aliases** — rejected: that preserves the
  duplication the collapse exists to remove; aliases exist for third-party
  C consumers, not for our own clients.

## Consequences

- Two canonical symbols per operation; the alias pair is frozen (no new
  aliases for future optional arguments — extend the canonical form or add
  a NULL-able parameter instead).
- `clients/javascript`, `clients/go`, `clients/java` each lose one binding
  and one branch; behavior is unchanged.
- The header regains no symbols and loses none; only doc comments move.
  Downstream C code compiled against older headers links unchanged.
- Removing the aliases later is a new ADR plus a major-version bump.

## Later references

- [ADR-0502](./0502-typed-c-abi-v2.md): the typed C ABI v2 removes the `cypherlite_query` /
  `cypherlite_connect` aliases. Its `cypherlite_abi_version()` = 2 (the
  JSON-era ABI is version 1 by definition) is the major-version bump the
  Consequences above require, delivered pre-public.
