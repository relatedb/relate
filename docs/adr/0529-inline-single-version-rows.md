# ADR-0529: Inline single-version rows; spill to a chain on the first update

Status: Proposed
Claim: 54a63c6c690668d0f3afb2ef59fad79450bba7c1
Later references: ADR-0532 (the dense id store this record deferred)

## Context

Every node and relationship id owns a version list, even when it has
never been updated. A create-only load (the memory-shape lanes, bulk
imports) keeps one-element lists everywhere: a vector header, a separate
heap allocation, and an `xmax` that stays zero. On the flat memory shape
that is 32 logical bytes of 136 per node, plus one allocation per id.

An earlier sketch derived the inline row's `xmin` from a graph-wide
epoch watermark. A watermark cannot answer "was this row created after
the pin" once it has moved past the create, so it cannot serve snapshot
reads (ADR-0012, ADR-0038).

## Decision

Each id's slot in the core node and relationship maps has two shapes.

- **Inline.** The live row plus its own birth timestamp. No `xmax`, no
  version list. Visibility reads the birth timestamp as `xmin` and the
  missing `xmax` as clear, so the predicate is unchanged and a pin taken
  before the create misses the row.
- **Spilled.** Today's version chain.

**Spill.** The write that creates a second version spills the slot: the
previous row gets `xmin` = its birth timestamp and `xmax` = this write,
and the new version is appended with `xmin` = this write. Further edits
in the same write reuse that version. A delete of an inline row spills
it into a one-version chain carrying the tombstone.

**Fold.** When checkpoint GC (unchanged: collectable iff
`xmax != 0 && xmax <= horizon`) leaves exactly one live version, the
slot returns to inline with that version's `xmin`. An inline row has no
`xmax`, so GC never collects it.

**Abort.** Abort of the creating write removes the slot. Abort of a
later write pops its versions and clears the tail's `xmax`; one live
version left folds back to inline.

**Floor (ADR-0202).** An inline slot is charged one map slot, the
8-byte birth timestamp, and the live row's own size. A spilled slot is
charged as before. The estimate and the live walk share the shape
functions, so `used_bytes == accounted_bytes` still holds.

**Admission (ADR-0042).** Spilling an inline row on delete grows the
floor until GC returns it. That growth alone never refuses a batch: the
check compares the delta net of tombstone spills against the limit, and
the reservation still carries the full delta. A delete at the cap
commits, as it did before.

## Alternatives considered

- **Watermark-derived `xmin`.** Saves the 8-byte birth timestamp but
  loses snapshot correctness once the watermark moves. Rejected.
- **Charge a one-version tombstone at the inline price.** Keeps
  admission unchanged but undercounts retained structure until GC,
  which ADR-0202 charges. Rejected.
- **A third, tombstoned inline shape.** Delete would grow 8 bytes
  instead of a list, but every slot widens by 8 bytes. Rejected.
- **Replace the core map with a dense store.** Larger change with its
  own trade-offs; independent of this one. Deferred.

## Consequences

Create-only graphs drop 32 logical bytes and one allocation per id. The
map value widens from a vector header to the inline row, so part of the
RSS gain is spent on map buckets; the memory-shape lane measures the
net. Per-node pins in capacity tests and the memory-shape baseline move
with the floor. Deletes of never-updated rows cost one small allocation
until the next checkpoint.
