---
type: adr
area: surface
name: 0223-wire-entity-kind-tags
status: accepted
tags:
  - http
  - values
title: Wire JSON entity values carry an explicit `kind` tag
REFERENCES:
  - "[[adr/0088-explicit-arrow-value-provenance]]"
  - "[[adr/0280-inf-nan-variable-semantics]]"
---

# ADR-0223: Wire JSON entity values carry an explicit `kind` tag

## Context

The canonical wire encoding (`cypherlite-model/src/wire.rs`) promised to
"never emit `__type` tags": a node arrives as `{"id","labels","properties"}`
and a relationship as
`{"id","type","start_node","end_node","properties"}`, and the reader
decides which it is holding by sniffing the key set. That works until a
client wants a generic dispatch: the node shape is a strict subset of
nothing, but the *heuristic* ("has `labels`? has `start_node`?") is
exactly the kind of shape-sniffing the wire format exists to avoid. Every
graph-rendering or re-serializing client (the JavaScript client's graph
extraction, notebook visualizations, downstream tooling) re-implements the
same two predicates, and each copy is a drift point (LABS-289).

The promise was written when the only consumer was the CLI's JSON mode,
which renders entities as `Display` strings anyway. Now that the wire JSON
feeds the C FFI, WASM, and the language clients on top of them, the tag is
the cheaper contract: one documented field beats two inferred predicates.

## Decision

Entity wire objects gain a **`kind` discriminator** key:

- Node: `{"kind":"node","id","labels","properties"}`
- Relationship:
  `{"kind":"relationship","id","type","start_node","end_node","properties"}`

`kind` values are the lowercase `CypherValue` variant names, matching the
snake_case convention of the stats counters. The tag lives *inside* the
existing wire objects — no envelope, no renaming of the existing fields,
so a reader that already pattern-matches the old shapes keeps working if
it ignores unknown keys (JSON's usual rule).

Paths gain **nothing**: the alternating `[node, rel, …, node]` array is
already unambiguous positionally, and its elements now carry their own
`kind` tags, so a path reader can validate as it walks. Scalars,
temporals, lists, maps, points, geometries, and vectors are unchanged —
points/geometries/vectors already self-describe (`srid`/`crs`,
GeoJSON `type`, `{"type","dim","values"}`), and tagging them would be a
second breaking change with no dispatch problem to solve: no client
confuses a point with a node. If a future format revision wants one
uniform discriminator on every non-scalar object, that is a new ADR.

The tag strings are exported from the model as
`cypherlite_model::wire::WIRE_KIND_NODE` / `WIRE_KIND_RELATIONSHIP` so
encoders and decoders share one vocabulary instead of re-typing
literals.

This is a **breaking wire change**: it supersedes the "never emits type
tags" promise in the wire module docs. Acceptable pre-1.0/alpha under the
same precedent as [ADR-0280](./0280-inf-nan-variable-semantics.md) (non-finite floats) and the stats-counters
unification; recorded in the CHANGELOG. Clients should dispatch on `kind`
when present; key-presence sniffing is deprecated and no longer
documented.

The persistence encoding (`serde_value`, `__type`-tagged) and the Arrow
typed-JSON provenance format ([ADR-0088](./0088-explicit-arrow-value-provenance.md)) are separate formats with their
own tags and are unaffected.

## Alternatives considered

- **`__type` tag (persistence-style).** Rejected: reuses the persistence
  discriminator for a different format with different values, inviting
  readers to feed wire JSON into the persistence decoder; the wire format
  deliberately never collides with user map keys by using a plain word
  only on engine-built objects whose other keys are fixed.
- **Wrap entities in an envelope (`{"node": {…}}`).** Rejected: changes
  every nesting level and breaks every existing reader outright, for no
  gain over one added field.
- **Keep shape-sniffing, document the two predicates.** Rejected: the
  predicates are the bug — N clients, N copies, and no way to evolve the
  shapes (e.g. adding an `element_id` field) without risking every
  sniffing copy.
- **Tag paths too (`{"kind":"path","nodes":…,"relationships":…}`).**
  Rejected: the alternating array is already self-describing and changing
  it would be a second, unnecessary break; the elements' tags make the
  array validatable.

## Consequences

- `value_to_wire_json` emits the `kind` key on node and relationship
  objects; the golden fixtures (`wire::golden`) pin it, so every surface
  routed through the shared encoder (C FFI, CLI envelope, WASM via
  `cypherlite::execute_wire`) picks the tag up with no surface-side code
  change.
- The wire module docs drop the "never emits `__type` tags" sentence in
  favour of the documented `kind` contract; `docs/api/c/spec.md`'s entity
  table names the tag.
- Readers that strict-match the old key sets must be updated in the same
  change (golden fixtures); readers that ignore unknown keys are
  unaffected.
- The gram recording decoder (`cypherlite-formatter`) dispatches on
  `kind` through one shared predicate; its key-count fallback exists only
  to read recordings written before this change. **Removal condition:**
  the fallback is not a compatibility contract — recordings are
  rendered-consumed artifacts, not a stored format — and it may be
  deleted in any later release without a new ADR.
- Future entity metadata (e.g. `element_id`) can be added as another key
  without re-opening the discrimination question.
