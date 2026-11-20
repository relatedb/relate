---
type: adr
area: surface
claim: fefd0681db7ad9d182e3168b2ff23639eb02e9eb
date: 2026-11-20
deciders: the maintainers
name: 0634-wire-object-key-order
status: proposed
tags:
  - output
  - values
title: Wire JSON objects serialize in construction order; row keys follow the column order
REFERENCES:
  - "[[adr/0195-result-envelope-mode]]"
  - "[[adr/0223-wire-entity-kind-tags]]"
  - "[[adr/0363-query-notifications]]"
  - "[[adr/0447-extension-single-wire]]"
---

# 0634. Wire JSON objects serialize in construction order

## Context

The canonical wire module (`relate-model`'s `wire`) builds every JSON
object by inserting keys in a documented order — a row's keys in the
statement's column order, a node's `kind` discriminator before its `id`
([ADR-0223](./0223-wire-entity-kind-tags.md)), the response envelope's
`columns` before its `rows` ([ADR-0195](./0195-result-envelope-mode.md)).
Serialization kept none of it: without `serde_json`'s `preserve_order`
feature the crate's `Map` is a `BTreeMap`, so every object on every JSON
surface serialized with alphabetically sorted keys. Row objects reached
envelope consumers keyed alphabetically — a consumer that builds a table
from the row objects alone, without consulting `columns`, shows the
wrong column order. The documented key order of every wire object never
shipped, and two readers had coupled to the accidental alphabetical
order: the HTTP stream's `fields`-before-`values` prefix, and the
scaffolded bash extension kit's tool-result extraction
([ADR-0447](./0447-extension-single-wire.md)).

## Decision

**The workspace enables `serde_json`'s `preserve_order` feature. Every
wire JSON object serializes in the encoder's construction order, and a
row object's keys follow the statement's column order.**

1. `columns` remains the authoritative column order. JSON objects are
   formally unordered and consumers keep matching by key name; the
   serialized byte order now carries the documented order instead of
   contradicting it.
2. The golden fixtures pin the serialized order at the string level — a
   mixed-order row's keys serialize exactly as `columns` lists them — so
   a return to sorted keys fails a test, not a consumer.
3. The recording renderer keeps its canonical text: map literals and
   entity properties render in code point order, now as an explicit sort
   at the decode boundary rather than an inherited `BTreeMap` iteration,
   so the recorded corpus is unchanged by this decision.
4. Order-coupled readers move to the pinned construction order: the bash
   kit's tool-result extraction anchors on the content item's last
   member, and the HTTP stream's `data` prefix holds by construction
   (its insertion was already `fields` then `values`).

## Alternatives considered

- **An order-preserving structure at the wire edge only** — rejected:
  cargo feature unification applies `preserve_order` to the whole build;
  scoping it away means a parallel ordered JSON type through the wire
  module — a second encoding path for one contract to drift.
- **Document `columns` as the only authoritative order, change nothing**
  — rejected: the encoder's own documentation already claims the
  construction order; declaring the accident canonical rewrites every
  documented object shape to alphabetical and leaves the reported
  consumer breakage in place.
- **Keep sorted keys; sort the encoder's construction to match** —
  rejected: reorders every builder to preserve an accident, and abandons
  the intended reading order (`kind` first, `columns` first).

## Consequences

Row objects, value objects, and every envelope on the JSON surfaces (the
CLI `json`/`jsonl`/`envelope` modes, the MCP tools, the C ABI envelope,
the WASM `execute` envelope) serialize in their documented order, and the
MCP tool result and the HTTP notification entries now match the Neo4j
member order their specs document. Installed
bash-kit extensions carry the old order-coupled extraction and must be
regenerated after upgrading — a breaking wire change, recorded in the
changelog, acceptable pre-1.0 on the ADR-0223 precedent. Parsed JSON
retains source order in every `serde_json::Map` in the tree; nothing
reads iteration order for correctness (readers match by key; the
recording layer sorts explicitly). `indexmap` was already a workspace
dependency, so the feature adds no package to the lock.
