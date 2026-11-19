---
type: adr
area: engine
date: 2026-09-19
deciders: CypherLite maintainers
name: 0078-typed-redo-in-process
status: accepted
tags:
  - values
  - wal
title: Redo ops carry typed values; JSON only at the WAL boundary
REFERENCES:
  - "[[adr/0059-wal-redo-typed-payloads]]"
SUPERSEDES:
  - "[[adr/0059-wal-redo-typed-payloads]]"
---

# 0078. Redo ops carry typed values; JSON only at the WAL boundary

## Context

[ADR-0059](./0059-wal-redo-typed-payloads.md) made WAL redo payloads typed models and forbade re-parsing Cypher
text on replay, but left one text-shaped payload in place: the property maps
on `CreateNode`/`CreateRel` and the value on `SetNodeProp`/`SetRelProp` were
`serde_json::Value`. The write path encoded typed `Value`s into that JSON
(`props_json`/`value_json`), the executor's commit apply then decoded the
same JSON back (`props_from_json`/`value_from_json`), and
`estimate_redo_delta` decoded it a third time on every accepted commit when a
memory limit is set. That is a JSON round-trip on the per-item write path.

Profiling the 1M-node + 1M-rel bulk `CREATE` put the redo JSON round-trip at
~14% of samples (encode inside the executor, decode in `apply_redo`). `Value`
has no serde derive; the tagged JSON encoding (NaN/Infinity/temporal/point)
is built manually in `redo.rs`.

## Decision

`RedoOp` property/value fields are **typed** in-process:
`CreateNode`/`CreateRel` carry `BTreeMap<String, Value>`,
`SetNodeProp`/`SetRelProp` carry `Value`. JSON is produced **once**, at the
WAL write (`Wal::append_record`), and decoded **once**, at replay
(`read_seg_limited`). The executor applies the typed values directly.

Byte compatibility is preserved by construction: each typed field serializes
via a `#[serde(with = ...)]` codec (`props_serde` / `value_serde`) whose
`serialize` delegates to the unchanged `props_json`/`value_json` encoders and
whose `deserialize` goes through the unchanged `props_from_json`/
`value_from_json` decoders. The on-disk shape is the same tagged JSON, so no
WAL format version bump and no migration.

`CatalogIndexDetail.options` stays `serde_json::Value` (cold DDL path).

## Guarantees pinned by tests

- **Byte-identical serialization** (`byte-compat-golden`): literal pinned
  bytes for the tagged NaN/Infinity/-Infinity/-0.0/temporal/point encodings,
  plus new-vs-old equality over a representative value set. The old shape is
  reproduced by test-local mirror DTOs; the pinned literals guard drift.
- **Old WAL replays on new code** (`replay-compat`): a hand-authored
  old-shape JSON `WalRecord` decodes through `read_seg_limited` and applies
  to the same graph state.

## Alternatives considered

- **simd-json for the WAL write.** Rejected. The serializer's self time is
  1.34% of the bulk profile; the real encode cost (~9%) is the intermediate
  `serde_json::Value` tree that `props_json` builds and drops, which a faster
  serializer does not remove. See the follow-up below.
- **Skip the WAL write for in-process apply.** Rejected. Durability is the
  contract; the WAL must record the ops regardless (user constraint).

## Consequences

- The per-item executor write path no longer encodes or decodes property-map
  JSON. `estimate_redo_delta` no longer decodes (and its lenient-decode
  divergence from `apply_redo`'s strict decode is gone — typed values cannot
  be malformed).
- `apply_redo`'s strict `?` decode is now the only decode, and it runs only
  on replay.
- Measured (bulk 1M+1M): 10.77s → 10.61s wall (−1.5%); `apply_redo`
  inclusive 47% → 45%. The modest wall-time delta is because the encode now
  dominates the WAL write — the intermediate `serde_json::Value` tree in
  `props_json`.

## Later references

- Measurements and the profile attribution:
  [`docs/performance-notes.md`](../performance-notes.md) §12.
- **Follow-up, landed (2026-09-19, commit 06dbe3d):** `props_serde`/
  `value_serde` now serialize the tagged bytes directly from the typed map
  (`TaggedValueRef`/`ListRef`/`MapRef`/`PointRef`), with no intermediate
  `serde_json::Value` tree. Emitted bytes unchanged (the byte-pinned and
  legacy-mirror tests pass unmodified); the ~890 ms tree build+drop in the
  bulk WAL encode is gone (`to_vec` 9.32% → 3.8%, `to_json_tagged` 4.4% →
  0.1%). See [`docs/performance-notes.md`](../performance-notes.md) §13.
- Supersedes the property/value half of [ADR-0059](./0059-wal-redo-typed-payloads.md)'s "typed payloads" goal for
  the in-process path; the WAL replay path keeps the JSON contract.
