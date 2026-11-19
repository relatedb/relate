---
type: adr
area: surface
claim: 7fa7affeb959113166e990fd145b78415909b1bc
name: 0506-ffi-numeric-entity-ids
status: accepted
tags:
  - ffi
title: The C ABI v2 gains the legacy numeric entity ids
REFERENCES:
  - "[[adr/0502-typed-c-abi-v2]]"
---

# ADR-0506: The C ABI v2 gains the legacy numeric entity ids

## Context

[ADR-0502](./0502-typed-c-abi-v2.md) typed the C ABI's value path: the v2 walker exposes every entity
identity as the element-id string (`cypherlite_value_element_id`,
`_start_element_id`, `_end_element_id`, §6), and the numeric ids the engine
carries in `CypherValue::Node { id }` / `CypherValue::Relationship { id,
start_id, end_id }` do not cross the boundary at all.

The LABS-447 driver-provider spikes (the stock Neo4j Java, Python and
JavaScript drivers run in-process against the engine) need those numbers.
The stock Neo4j drivers decide node and relationship **equality** by the
deprecated numeric `id` — `Node.equals` compares the id — so a provider
that cannot read it cannot hydrate two handles to the same node as the
same object, and the analysis doc's measured hacks included synthesising
or re-spelling ids out of the element-id string (`"4:cypherlite:n0"` →
`0`), which [ADR-0502](./0502-typed-c-abi-v2.md) exists to remove. Cypher `id` returns exactly the
engine's number (`bi_id` returns `Value::Int(x.id)`), and
`CypherValue`'s id fields are plain `i64` — no range question, no
`Option`.

## Decision

**The v2 walker gains three scalar accessors returning the engine's own
numbers, additively — no version bump:**

- `cypherlite_status cypherlite_value_id(cypherlite_value v, int64_t *out)`
  — NODE or RELATIONSHIP: the numeric id, exactly the INTEGER Cypher
  `id()` returns for the same entity;
- `cypherlite_status cypherlite_value_start_id(cypherlite_value v, int64_t *out)`
  and `_end_id` — RELATIONSHIP only: `id(startNode(r))` /
  `id(endNode(r))`.

They mirror `cypherlite_value_int` exactly: one `read_scalar` shape, the
same refusal contract (wrong kind, NULL handle or NULL out-param →
`CYPHERLITE_ERROR` with `*out` untouched), no second refusal path and no
new state on the handle — the numbers are read straight off the value.
An entity value always carries an id, so there is no in-kind refusal.
The endpoint accessors take a NODE as the wrong kind, the same way
`_start_element_id` does.

Pre-public, the ABI is additive-only after the freeze [ADR-0502](./0502-typed-c-abi-v2.md) §8 sets —
three new symbols without a semantic change to any existing one are
exactly that case; `cypherlite_abi_version()` stays `2`.

## Alternatives considered

- **Parse the number out of the element-id string in the client (the
  spikes' measured hack).** Rejected: it re-spells an identity the engine
  already computed, couples every client to the `"4:cypherlite:n0"`
 format [ADR-0502](./0502-typed-c-abi-v2.md) §6 deliberately did not specify as a client contract,
  and breaks the day the spelling changes.
- **Expose the ids as entity map entries or a kind component alongside
  properties.** Rejected: a map key is user-collidable surface and
  conflates engine identity with data; the walker's kind-component
  accessors are the one path for entity structure.
- **Wait for the stock drivers to move to element ids and ship nothing.**
  Rejected: the drivers decide equality by numeric id today, and the
  provider spikes are the ABI's near-term consumer; deferring re-exports
  the synthesis hack into every provider.
- **Fold the accessors into [ADR-0502](./0502-typed-c-abi-v2.md)'s §6 and bump nothing.** Rejected:
 [ADR-0502](./0502-typed-c-abi-v2.md) is Accepted and immutable; a later addition needs its own ADR
  plus a Later references note, which is this file.

## Consequences

- `surfaces/cypherlite-ffi` exports the three symbols; the header
  (`include/cypherlite.h`), `docs/api/c/spec.md` and the `walker.c`
  example document them
  in the same change. FFI tests assert the ids equal `id(n)` / `id(r)` /
  `id(startNode(r))` / `id(endNode(r))` in the same row, at the top level
  and nested inside a LIST and a PATH, and the wrong-kind refusals.
- Driver providers read identity in one place and stop synthesising ids;
  user-facing code keeps the element ids (the numeric `id()` is
  deprecated in Cypher and the accessors' docs say so).
- The ids are per-store engine identities: they are not stable across a
  store rebuild from external data and are not comparable across stores —
  the same caveats Cypher `id()` carries, inherited rather than specified
  anew.
- No new state, allocation or refusal path on the value handle: the
  accessors are three more `read_scalar` calls over fields the engine
  already materialises.
