---
type: adr
area: engine
claim: dca876d599524b2fd9223effc273298228ab1a7c
date: 2026-11-16
deciders: MemoryDB maintainers
name: 0610-value-provenance
status: proposed
tags:
  - checkpoint
  - functions
  - mvcc
  - provenance
  - transactions
  - wal
title: Per-property value provenance from retained MVCC history
REFERENCES:
  - "[[adr/0013-tuple-mvcc]]"
  - "[[adr/0014-single-wal-write-path]]"
  - "[[adr/0023-incremental-version-gc]]"
  - "[[adr/0086-bounded-persistence-input]]"
  - "[[adr/0371-preview-feature-gating]]"
  - "[[adr/0378-ontology-procedures-preview-gate]]"
  - "[[adr/0497-catalogue-corpus-alignment]]"
  - "[[adr/0529-inline-single-version-rows]]"
  - "[[adr/0593-system-ontologies-visibility-preview-gate]]"
---

# ADR-0610: Per-property value provenance from retained MVCC history

## Context

ADR-0013 versions whole rows and ADR-0014 persists one record per commit,
but ADR-0023's GC and every checkpoint's collapse destroy a property
value's origin at the next commit, and transaction metadata has natural
input channels and nowhere to go (`tx.setMetaData` is a compat no-op,
ADR-0497 §6; the record carries none). Users ask about values: who set
this price and why.

## Decision

One opt-in preview feature, `value-provenance` (ADR-0371's registry),
gates the whole machinery — record format, retention, checkpoint shape and
the three query names together.

**Transaction records.** `CommitRecord` gains optional fields, omitted while
the feature is off, so old records and old readers decode unchanged:
`metadata` — the transaction's metadata map, omitted when empty;
`changed_keys` — the commit's distinct property-key changes, each
`{kind, id, key}`, derived once at commit from the redo and pre-state
(creates: initial keys; set/remove: the named key; delete: every key at
the snapshot); and `type` — the family type.

**Per-property granularity.** No per-property storage: ADR-0013 versions
whole rows, so per-property events are derived by diffing consecutive
retained versions — a key whose value differs between versions *i-1* and
*i*, or is absent on one side, has an event at *i*'s `xmin` carrying the
installed value (`null` means removed), joined to that commit's record.

**Metadata channels.** Four inputs, one per-transaction map merged in call
order (later keys win), serialized into the record's `metadata` at commit,
cleared after — an ADR-0086 metadata object, bound-checked at the channel:
`CALL tx.setMetaData(map)` (merges while on, the void no-op off); a Bolt
`BEGIN`'s `tx_metadata` extra (seeds at open while on, ignored off); the
embedded `Connection::set_tx_metadata(map)` (fails the authored preview
error without the opt-in, never silently dropping); and `@txMetadata(...)`
— the one **reserved** clause-annotation name (parsed only under
`cypher-annotations`; off, `@` stays the unexpected-character error). Its
parameters spell one map — positional map literal or all-named — with an
optional string `type` key naming the record type (absent or
`"provenance"`: the home type; another name: the canonical typed
validation). Captured before the annotation discard, merged in clause
order, seeded after the admission gates so a refused statement seeds
nothing; `value-provenance` off drops it — no error, no record. `EXPLAIN`
never seeds; no other name is captured.

**Typed records.** Every record names its family: the shipped channels
hard-set `type: "provenance"`; the embedded API's typed twin,
`set_tx_metadata_of_type(type, map)`, seeds another type — non-empty
kebab-case, `"provenance"` reserved. Retention retains every typed
record; changed keys derive only for provenance commits; the provenance
queries join only provenance records; a record or part without the field
decodes as provenance's; a future type brings its own ADR and gate.

**Retention.** While the feature is on for an open, ADR-0023's horizon drop
exempts every version — tombstoned chains included, so deleted elements
stay auditable through `db.transactions` — and the checkpoint persists
what it used to collapse: the retained version chains and records, a new
lsn-windowed snapshot family under the existing orphan sweep. An open
without the opt-in collapses retained history at its next checkpoint.

**Query contracts.**

- `provenance(element :: NODE | RELATIONSHIP, key :: STRING) :: MAP` — the
  `{commitTs, lsn, metadata, value}` event map; `NULL` when none exists.
- `history(element, key [, maxNum]) :: LIST<MAP>` — the same event shape,
  ascending `commitTs`, retained events only. Omitted or `null` `maxNum`:
  every retained event; a non-negative integer: the last `maxNum` (0 =
  empty), a sliding window on the walk, never materialized; any other
  third argument, the ordinary argument-type error.
- `CALL db.transactions(since = 0 :: INTEGER)` — a READ procedure:
  retained records ascending by `lsn`, `since` the inclusive `commitTs`
  lower bound (0 = all), yielding `lsn`, `commitTs`, `type`, `metadata`,
  `changedKeys` (each `{elementId, key}`, deleted included; null for
  another type — never derived), streamed under a row cap.

**The gate, the catalogues, and zero cost while off.** A fourth exception
class to ADR-0371's syntax-only scope policy — after a procedure family
(ADR-0378), name admission (ADR-0497) and namespace visibility (ADR-0593):
a whole machinery that is one opt-in, because retention changes what a
store persists and how memory grows. The function names hide — the
ordinary `UnknownFunction` (42N48), no `SHOW FUNCTIONS` row — the
procedure gates at dispatch with the authored error and no
`SHOW PROCEDURES` row, and none is in a corpus row (ADR-0497 §2 rule
(b)). With the feature unset the catalogues, write path, GC and
checkpoint are byte-for-byte today's; a never-opted-in store is
indistinguishable from one without the feature.

## Alternatives considered

- **Per-property version chains.** Rejected: a second chain per (element, key) duplicates ADR-0013/ADR-0529.
- **A per-property event log in the WAL.** Rejected: the redo ops are the event log.
- **Metadata persisted unconditionally.** Rejected: it changes record bytes
  for never-opted-in stores; zero cost while off is the gate's point.
- **A durable or windowed retention knob.** Rejected: the per-open flag
  is reversible (the durable form waits for the gate lift), and growth
  while opted in is the recorded cost — `maxNum` bounds the answer, not
  retention.
- **Per-principal privileges on the audit surface.** Rejected: no
  privilege model exists for any query; the opt-in is the control.
- **`db.transactions` filtered only by YIELD/WHERE.** Rejected: the row
  count scales with retained history; `since` is the bounding filter.

## Consequences

- `tx.getMetaData()` returns the live map while on, the placeholder row
  while off; memory grows with retained versions and records while opted
  in — the user's explicit act, guarded by the memory-shape suite.
- The opt-in **is** the access decision: the engine has no per-principal
  privileges for any query, so an opted-in store exposes overwritten and
  deleted values to every querying principal. Metadata keys (`by`
  included) are caller-asserted: the record vouches for the commit
  identity (`commitTs`, `lsn`), never the claim's truth.
- Erasure is coarse by design: turning the opt-in off collapses retained
  history at the next checkpoint; per-value erasure and windowed
  retention are the recorded rejections below, awaiting a requirement.
- The three query names are new public surface; ADR-0013, ADR-0023 and
  ADR-0371 gain Later references notes. A merged change (the future
  branch write engine) keeps its original provenance and appends its own
  merged-by record — the audit surface shows origins and merges both.
