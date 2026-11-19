---
type: adr
area: surface
claim: 0cdf2287379bd9b4715fd84e98cefa8d1eb8a6d2
name: 0367-graph-value-output-contract
status: accepted
tags:
  - output
  - values
title: graph values in output — the [ADR-0223](./0223-wire-entity-kind-tags.md) wire form in JSON, id-free Display in text
REFERENCES:
  - "[[adr/0223-wire-entity-kind-tags]]"
---

# 0367: graph values in output — the ADR-0223 wire form in JSON, id-free Display in text

## Context

LABS-107's demo-scenario findings: `--mode json` rendered nodes,
relationships and paths as their Display strings with internal ids
(`<(:P {n: "x"}) [id=0]-[:R] [id=0]-(:P {n: "y"}) [id=1]>`), losing graph
identity for every JSON consumer (demo tooling, agents, alias-insensitive
comparisons). The Display text appended ` [id=N]` to nodes and
relationships and wrapped paths in `<...>` — both diverging from the
Neo4j precedent, captured live against a running neo4j:5.26 reference:
cypher-shell (table and plain) and the Browser text mode render
`(:Person {name: "Alice"})-[:KNOWS {since: 2020}]->(:Person {name: "Bob"})`
— labels, properties and rel types only, never internal ids, never a
wrapper. Neo4j's own JSON (the Query API and drivers) returns structured
node/relationship/path objects with element ids.

The wire encoder already owns the structured forms ([ADR-0223](./0223-wire-entity-kind-tags.md)): the
kind-discriminated objects every client consumes. The formatter's JSON
family (`json`, `jsonl`, `envelope`) deliberately fell back to Display
strings for the graph-value set — a fallback inherited from before the
wire forms existed.

## Decision

1. **The JSON family routes graph values through the wire encoder.**
   Nodes emit `{kind, id, labels, properties}`, relationships
   `{kind, id, type, start_node, end_node, properties}`, paths the
   alternating `[node, rel, ...]` array; spatial values emit their
   structured wire forms (`{crs, srid, x/y/z}`, `{type, coordinates}` for
   geometry), vectors `{type, values, dim}`. The display-string fallback
   in `cypher_value_to_json` is deleted — uniformly, including Point
   (the former local exception; geometry going structured while Point
   stayed a string was incoherent).

2. **The Display text drops internal ids and the path wrapper.** Nodes
   render `(:Label {props})`, relationships `[:TYPE {props}]`, paths the
   bare node-rel-node chain — the captured Neo4j precedent. Engine ids
   stay reachable through `elementId()` / `id()` when a query asks.

## Alternatives considered

- **Keep the display strings in JSON, add a structured opt-in flag** —
  rejected: two spellings of one value invites consumer drift, and the
  wire forms already exist as the one documented encoding.
- **Keep Point's display-string exception for output compatibility** —
  rejected: pinning incoherence to avoid one diff; the JSON family is
  new enough (no released consumer depends on the string form) and the
  wire form is the documented contract.
- **Render ids in the text mode, configurable** — rejected: no captured
  precedent shows ids in text mode; a flag per rendering question
  multiplies surface area for a reachability `elementId()` already
  covers.

## Consequences

- `--mode json/jsonl/envelope` output for graph values is a breaking
  change vs the pre-change display strings; the pins
  (formatter, CLI params) carry the new shapes; the agent-facing skill
  bundle references that document the JSON shapes update in the same
  change.
- The Display change ripples through every text consumer (table, cypher,
  CSV, recordings, the examples differential's diff renderer) — all
  pinned outputs verified in the same change (the full workspace suite).
- Temporal values keep their Display strings in the wire encoder's JSON
  (the one documented intentional Display arm, ISO-8601) — unchanged.
