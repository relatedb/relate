# ADR-0574: Withdraw CDC replication slots

Status: Accepted
Claim: eec14baaca3517b15863fde7a2de1ddfd0d4e7ea

## Context

ADR-0045 added optional CDC slots so an embedding client could hold WAL
recycle until a consumer acked an LSN, poll records with
`cdc_records_after`, and fence a replica with `set_apply_only`. The slot
file was `cdc/slots.json`.

The first release does not ship change-data-capture. Slots, the poll, and
the apply-only fence are product surface this release will not include.
WAL follow (ADR-0137) already tails a native directory without pinning
recycle, and `apply_replicated` is the physical replica apply path used
outside slots.

## Decision

Remove CDC slots from the engine.

- Delete `register_cdc_slot`, `ack_cdc_slot`, `drop_cdc_slot`, `cdc_slots`,
  `cdc_records_after`, `set_apply_only`, `apply_only`, and `CdcSlot`.
- Stop reading and writing `cdc/slots.json`. An existing file is ignored.
  Open does not migrate it.
- Checkpoint recycle is `checkpoint_lsn` (ADR-0020). A consumer cannot hold
  closed WAL segments past the checkpoint.
- Keep `apply_replicated`. It appends one physical redo record and applies
  it. It is not a slot.
- Keep WAL follow (ADR-0137). It does not register a slot and does not pin
  recycle.
- `Wal::records_after` stays a storage-level bounded read. It is not a
  `Database` method, and nothing in the engine waits on its cursor before
  recycle.

Cypher catalogue names that happen to contain `cdc` are a separate
concern (ADR-0557). This decision does not add them back.

## Alternatives considered

- **Cargo-feature the API and leave the code in tree.** Rejected: the
  first release should not carry a feature it will not support.
- **Keep `cdc_records_after` without slots.** Rejected: recycle would
  delete unread segments, which is the hole ADR-0045 refused to ship
  without a slot.
- **Keep the apply-only flag without slots.** Rejected: the flag existed
  to fence a CDC replica, and it was persisted in the slot file. Follow
  and read-only open already refuse customer writes.

## Consequences

- Callers of the removed methods must stop calling them. There is no
  compatibility shim.
- A store that still has `cdc/slots.json` opens, and the next checkpoint
  recycles closed segments the old slot was holding.
- Group commit, checkpoint, and follow are unchanged apart from the
  recycle cut no longer consulting a slot.
