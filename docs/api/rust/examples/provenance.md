# Provenance-function example

Run with `cargo run -p relate --example provenance`.

The example opts in by setting `RELATE_PREVIEW=value-provenance` before
`Database::open` (ADR-0610, through ADR-0371's registry). The
`provenance(element, key)` and `history(element, key [, maxNum])`
functions are
name-gated behind that opt-in (ADR-0497's admission shape): without it
a call is the ordinary unknown-function error (42N48) with no
`SHOW FUNCTIONS` row — the names do not exist, so no error may
advertise them.

With the opt-in, the example walks one property's life through the
functions' ADR-0610 contract:

- `provenance(n, key)` returns the **newest retained event** of the
  property's current value as the `MAP {commitTs, lsn, metadata, value}`
  of the commit that installed it — `value` is `null` for a removal
  event, and the call answers `NULL` when no retained event exists.
- `history(n, key)` returns **every retained event** of the property,
  ascending `commitTs`, as a `LIST<MAP>` of the same event maps — the
  retained events only, so the oldest need not be the birth.
- `history(n, key, maxNum)` — the bounded form — returns the **last
  `maxNum` retained events**, still ascending: an omitted or `null`
  `maxNum` is the whole list, `0` the empty list, and the bound rides
  the walk (a sliding window over the collected events), so the tail
  answers without materializing the whole list; a negative or
  non-integer `maxNum` is the ordinary argument-type error.
- A `SET` that leaves a sibling property untouched never re-dates the
  sibling: ADR-0013 versions whole rows, so per-property events are
  derived by diffing consecutive retained versions — the sibling's event
  stays at the commit that wrote it.
- The event's `lsn` and `metadata` join the committing transaction's
  retained record, so the metadata channels (`CALL tx.setMetaData`, the
  embedded `set_tx_metadata`, a Bolt `BEGIN`'s `tx_metadata` extra) ride
  the answer.
- A null element or key answers `null` (Cypher's null propagation), and
  a key no version of the element ever carried has no event.

The same opt-in is the **retention** configuration (ADR-0610): the GC
horizon drop exempts every version and the checkpoint persists the
retained version chains as `chains/` objects beside the snapshot image.
The example runs on a durable store, checkpoints, and reopens: the
reopened store answers the same `history()` list with the real `xmin`
of every version — the event maps name each version's own commit, not
the restored row's birth. An open without the opt-in collapses retained
history at its next checkpoint, so retention is per-open, never a
durable store property.

The same retained records are the `db.transactions` / `history()` audit
surface's input (`Database::transaction_records()`, joined to a
bookmark's `relate:<n>` by `n` = `commit_ts`).
