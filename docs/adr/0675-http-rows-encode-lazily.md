---
type: adr
area: surface
claim: 980da9eef47e1d9046609a4bb83963be9ecd762f
name: 0675-http-rows-encode-lazily
status: proposed
tags:
  - http
title: HTTP Query API rows encode lazily from the engine's values
REFERENCES:
  - "[[adr/0386-http-chunked-streaming]]"
  - "[[adr/0555-operators-stream-rows-never-accumulate]]"
---

# ADR-0675: HTTP Query API rows encode lazily from the engine's values

## Context

[ADR-0386](./0386-http-chunked-streaming.md) bounded the *serialization* copy of a Query API reply and left row
materialization to the engine (its Decision 6). The success envelope still
converted every row into a `serde_json::Value` tree before the writer
started, and that tree is the largest copy in the path: a `Value` per scalar
plus an array per row costs about 20x the response size. One query returning
every relationship of a 400k-relationship graph is enough to exhaust a 2 GB
host, while Bolt, which keeps only the engine's compact values and encodes a
page at a time, serves the same result.

## Decision

1. **The envelope's `data` member keeps the engine's rows.** `fields` and the
   rows are held as the `QueryResult`'s own values and encoded one value at a
   time inside the body's `Serialize` implementation, so the transient JSON a
   row needs lives for that row alone. The wire bytes are unchanged: `fields`
   first, then `values`, in both media types.
2. **Everything else stays a JSON tree.** Whole-object replies and the
   envelope's tail members are small and keep the `serde_json::Value` form.
3. **Engine row streaming is out of scope.** The rows are still materialized
   by the engine before the reply seam (ADR-0386 Decision 6); pushing rows
   from the final projection into the response, for HTTP and Bolt alike, is a
   separate decision with its own ADR.

## Consequences

- A reply's peak memory drops from the JSON tree plus the engine rows to the
  engine rows alone, the same footprint Bolt already has.
- The counting pass and the chunked writer serialize through the same
  `Serialize` implementation, so the measured length is still the written one.
- A row's encoding cost is paid at write time, so a client that stops reading
  stops the encoding.

## Later references

[ADR-0386](./0386-http-chunked-streaming.md) Decision 6 is narrowed by this ADR: rows are no longer
rebuilt as a JSON tree before the reply seam.
