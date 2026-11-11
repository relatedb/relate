# 0045. Optional CDC slots hold WAL recycle; apply-only replicas

- **Status**: Superseded by [ADR-0574](0574-withdraw-cdc-replication-slots.md)
- **Date**: 2026-09-09
- **Deciders**: MemoryDB maintainers

## Context

WAL is a crash-recovery buffer (ADR-0020). Checkpoint recycle deletes
closed `wal/NNNNNNNN.log` files with `max_lsn <= checkpoint_lsn`. A
client implementing embedded Relate that publishes those records to a
stream loses history forever if it lags the checkpointer.

That client embeds this crate. The engine must not know the client's
stream broker. It must keep today’s recycle when nobody is consuming WAL.

A replica that applies a CDC stream must not accept customer Cypher
writes. The only mutation is applying physical redo.

## Decision

- **Slots are optional.** Zero slots ⇒ `recycle(checkpoint_lsn)` is
  unchanged (ADR-0020).
- A slot is `{name, registered_lsn, acked_lsn}`. Register at current
  `wal.lsn`. Recycle cut is `min(checkpoint_lsn, min(acked_lsn))`.
  Parquet still advances; only file deletion waits on acks.
- Persist `<data>/cdc/slots.json` (atomic rename). `:memory:` keeps
  slots in process. Load on `Database::open`.
- Public API: `register_cdc_slot`, `ack_cdc_slot`, `drop_cdc_slot`,
  `cdc_slots`, `cdc_records_after`, `set_apply_only` / `apply_only`,
  `apply_replicated`.
- `cdc_records_after` copies records with `lsn >` cursor under a brief
  WAL lock. It does not open the WAL for append.
- **Apply-only** is enforced in `try_wal_commit`. The embedding client
  only sets the flag. `apply_replicated` appends to this WAL (new LSN),
  fsyncs, and `apply_redo` with the source `commit_ts` and ids. Reject
  `apply_replicated` when any source slot exists.
- Persist `apply_only` in the same JSON so a restart cannot accept a
  Cypher write before the replica task starts.

## Alternatives considered

- **Sidecar file tail without a slot** — recycle deletes unread
  segments. Rejected; the stream would have permanent holes.
- **Publish to the stream on every commit** — couples write latency
  and availability to the broker. Rejected (ADR-0019 already keeps
  fsync off the network).
- **Fence writes only in the embedding client** — a missed check or a
  future Bolt path would accept writes. Rejected; the fence is
  `try_wal_commit`.

## Consequences

- A lagging slot grows disk until ack or drop.
- A client implementing embedded Relate can implement source and sink
  without changing group commit.
- Existing checkpoint tests stay valid when no slot is registered.

## Later references

Clients implementing embedded Relate consume this API.

ADR-0574 withdraws the slots, the poll, the apply-only flag, and
`cdc/slots.json`. Recycle is `checkpoint_lsn` again. `apply_replicated`
stays.
