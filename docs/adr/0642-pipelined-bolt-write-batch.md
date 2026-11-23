---
type: adr
area: engine
claim: 385cde07c24e572f8ff25aafcbb32d2256067fcd
date: 2026-11-21
deciders: Relate maintainers
name: 0642-pipelined-bolt-write-batch
status: proposed
tags:
  - write-path
  - bolt
title: One commit per pipelined Bolt write batch
REFERENCES:
  - "[[adr/0019-group-commit-shared-fsync]]"
  - "[[adr/0209-batched-global-bulk-writes]]"
  - "[[adr/0210-call-in-transactions-batching]]"
  - "[[adr/0310-bolt-bookmarks]]"
  - "[[adr/0317-bolt-reset-ignores-queued-work]]"
---

# 0642. One commit per pipelined Bolt write batch

- **Related**: [ADR-0019](./0019-group-commit-shared-fsync.md) (group commit),
  [ADR-0209](./0209-batched-global-bulk-writes.md) (batched global bulk writes),
  [ADR-0210](./0210-call-in-transactions-batching.md) (per-batch commit sink),
  [ADR-0317](./0317-bolt-reset-ignores-queued-work.md) (the framed message batch)

## Context

Driver imports pipeline RUN/PULL pairs: one socket write carries many
statements, and the driver reads the responses afterwards. Every auto-commit
RUN is its own WAL record and its own durability barrier, so a 429-statement
import pays 429 barriers and 429 group-commit admissions even though nobody
observes a statement boundary between them. ADR-0209 batches the *rows* of one
global bulk statement; ADR-0210 commits *chunks* of one `CALL { … } IN
TRANSACTIONS`; neither coalesces commits across statements. The framed message
batch (ADR-0317) is the natural unit: exactly what the client sent without
waiting for an answer.

## Decision

A framed message batch that carries two or more RUNs arms a **pipelined write
batch** on the connection. Its eligible auto-commit statements execute in one
shared write overlay and commit **once**, at the batch's end, through the
normal group-commit path (ADR-0019) — one WAL record and one barrier for the
whole run instead of one per statement.

1. **Eligibility is plan-based and conservative** (`plan_is_create_only`): the
   statement's only mutating clause is `CREATE`, it carries no `CALL`, no
   `CALL { … }` subquery, no `WHEN`/`ELSE`, no `FOREACH` and no schema DDL, it
   produces no rows (no `RETURN`), and it is a single part with no `UNION` and
   no `NEXT`. A statement that cannot join ends the batch first — its
   predecessor statements commit, so it reads and writes the state it expects
   — and the batch stays closed for the rest of that framed batch.
2. **The batch is the transaction machinery in a second mode.** It pins one
   snapshot and holds one `WriteScratch`, so each statement sees the previous
   ones' writes, like the auto-commit statements it replaces. It is not a user
   transaction: `in_transaction` stays false, so no RUN carries a `qid`, no
   transaction breach fires, and the per-request namespace selection is
   unchanged. `BEGIN` commits the batch before the explicit transaction
   starts, and an explicit transaction never nests one.
3. **Failure keeps auto-commit durability.** A batched statement that fails is
   rolled back to its statement-entry scratch checkpoint: its own partial
   writes are discarded, and the batch's earlier statements still commit. A
   batch commit failure (storage, capacity, conflict) fails the whole batch and
   closes the connection, since a FAILURE with no request to answer would
   desynchronize the client.
4. **A lone RUN keeps the old path.** A framed batch with one RUN — an
   interactive driver's RUN, wait, PULL — arms nothing and commits per
   statement, so its ending summary keeps its bookmark (ADR-0310). A batched
   statement commits nothing at its own boundary, so its ending summary
   carries no bookmark and the batch's own commit is not reported on the wire.

## Alternatives considered

- **Commit before each acknowledgement so every batched statement keeps its
  bookmark.** That is the per-statement commit being removed. Rejected.
- **Defer the commit across framed batches until the client waits.** Makes
  durability depend on arrival timing and needs socket polling below the
  framing layer. Rejected.
- **Batch every write flavor, not only `CREATE`.** The shared overlay keeps
  `SET`/`MERGE`/`DELETE` sequentially correct too, but widening the gate
  widens the review surface of a change whose point is the import path.
  Widening stays an additive later decision. Rejected for now.
- **Rewrite the pipeline into one explicit transaction at the session.** The
  client would get a transaction it never asked for: one ROLLBACK discards the
  whole pipeline, and its statements lose auto-commit durability. Rejected.

## Consequences

- A pipelined `CREATE`-only import pays one durability barrier per framed
  batch. Measured over Bolt against the previous binary, the identity load's
  statement shape (429 UNWIND-CREATE statements of 200 rows, one pipeline):
  679-780 ms before, 250-295 ms after; a 20 000 single-row `CREATE` stream:
  4.4-16.2 s before, 0.67-0.71 s after.
- A batch's statements commit together: a write conflict or a storage failure
  at its commit fails all of them, not one, and other connections observe the
  writes only at that commit.
- A batched statement has no bookmark of its own (ADR-0310); a pipelined
  import that needs causal bookmarks must break the pipeline.
- Under `full` durability a batched statement's `SUCCESS` no longer implies
  durability: its summary is written as the statement runs, while the batch's
  one barrier happens at the framed batch's end. A crash inside that window
  loses writes the client was already told succeeded — the window a client
  that pipelines without waiting for acknowledgements accepts by construction.
  A lone `RUN` (decision 4) keeps its per-statement barrier and its old
  guarantee.
- Memory: the batch's redo accumulates in one overlay, bounded by the existing
  64 MiB per-commit payload cap — the statement that would cross it is refused
  and the batch commits what it has.
