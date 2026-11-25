---
type: adr
area: engine
claim: fed29baa16bf362c3e7ab0e9b1fdc17e0d2dacc3
date: 2026-11-25
deciders: Relate maintainers
name: 0682-engine-batch-consumer
status: proposed
tags:
  - executor
  - http
  - bolt
title: the final RETURN hands its batches to the surface as they are produced
REFERENCES:
  - "[[adr/0386-http-chunked-streaming]]"
  - "[[adr/0555-operators-stream-rows-never-accumulate]]"
  - "[[adr/0660-clause-consumer-batch-stream-contract]]"
  - "[[adr/0675-http-rows-encode-lazily]]"
---

# the final RETURN hands its batches to the surface as they are produced

No new vocabulary is introduced: the engine's own `RowStream` / `RowBatch` /
`terminal` names are used.

## Context

[ADR-0660](./0660-clause-consumer-batch-stream-contract.md): every clause is
a transformer over a **pull** stream (`RowStream::next_batch`, `stop`),
bounded at 1024 rows per `RowBatch`. Its `Return` arm is the `terminal`,
and today the terminal lands every batch into the part's `Vec<Row>`;
`Connection::query` returns that as a fully built `QueryResult`.
HTTP and Bolt therefore hold the whole answer before sending the first byte.

The landing site is the `Clause::Return` arm of `run_clauses_mode`
(`exec/dispatch.rs`): `project_clause` returns the projection as a
`Vec<Row>`, `return_result` moves the cells into `QueryResult.rows`, and
the part result is read back by the statement tail, `UNION` folding
(`union.rs`) and the `NEXT` seeding. `QueryResult` is complete before
anything returns to `relate::Connection`, so no row is visible to a surface
until the statement ends.

## Decision (proposed)

1. **Same model, same names.** The engine stays pull. The only change is
   what the `terminal` does with each `RowBatch` it pulls: instead of
   appending to a `Vec<Row>`, it passes the batch to a **batch consumer**
   supplied by the caller. No cursor type, no second stream concept. The
   hook sits in the `Return` arm and applies only when that arm is the last
   clause, there is no enclosing scope, and no `NEXT` stage or `UNION` part
   follows it; every other `Return` keeps landing into the part's rows.
2. **Back-pressure is the pull itself.** The terminal pulls the next batch
   only after the consumer returns from the previous one. A consumer that
   blocks on a slow socket therefore stops the whole walk, with no queue
   in between and nothing buffered beyond the one batch in hand. The
   consumer can also answer *stop*, which is `RowStream::stop`: the walk
   ends, the statement is cancelled.
   - The consumer does **not** run on the engine worker. A statement runs
     on the connection's parked worker thread (ADR-0259) while the calling
     thread blocks and runs only the heartbeat tick (Bolt's keep-alive
     writes a NOOP chunk to the socket on that tick); the caller thread
     alone owns the stream. A consumer writing from the worker would race
     the tick's writes on the same stream. The worker therefore hands each
     owned `RowBatch` to the caller thread through a one-slot rendezvous
     added to the worker's job slot, and blocks until the caller has taken
     it and asked for the next. The batch moves, it is not copied; the
     pull-after-return rule above is unchanged, and the caller thread runs
     the consumer and the heartbeat in one loop.
3. **Public entry.** `Connection::query_streaming(cypher, params, token,
   on_batch)`: `on_batch` gets the result columns once, then each
   `RowBatch`; the call returns the statement's summary (stats, plan,
   notifications, timings) after the last batch. `Connection::query` is
   the same call with a consumer that collects, so it behaves as before.
4. **What streams.** The first slice ships the first form only: a single
   stage and part ending in a map-each-row `RETURN`, with no `PROFILE` and
   no `USE`. Every other shape below lands and replays through the
   consumer, byte-identical to `query`; the forms described for them are
   the follow-up slices this decision commits to, not what the first slice
   does.
   - A plain `RETURN` after any `pass`/`page`/`expand` chain: streams.
   - `UNION ALL`: arm one streams first, then arm two, and so on; no
     buffering. Arm one's columns define the output. Column names are
     validated statically (parser, front end, planner) before any row, but
     the executor also tolerates arms that project the same names in a
     different order (`align_union_columns`), so arm two and later are
     permuted onto arm one's column order per batch. A `RETURN *` arm learns
     its columns only at its first batch, so the columns are sent with the
     first batch, never earlier.
   - `UNION` (distinct): each arm streams, filtered against the
     first-seen set; only the set is retained, never the rows.
   - Breakers (aggregation, unlimited `ORDER BY`, `collect`, percentiles,
     join build side, star aggregation): they hold their input until they
     can answer, as declared. What the consumer saves is the second copy,
     not the breaker's own retention:
     - Unlimited sort: the sort is not in place (it builds a key table and
       a slot table over the held `Vec<Row>`, charged by the sort hold), so
       the landed rows are retained once and the consumer emits
       `BATCH_ROWS` slices from the front of the sorted rows, saving only
       the `QueryResult` copy. Bounded top-k emits from its window state.
     - Group table: aggregation today pushes every projected group into a
       second `Vec<Row>`. Each group is projected independently, so without
       `ORDER BY` it emits a batch every `BATCH_ROWS` groups straight from
       the table; with `ORDER BY` the rows are still collected and sorted.
     - `UNION` folding retains each part's rows today; the streaming forms
       above replace that, and only the distinct set stays.
   - `EXPLAIN`: produces no rows, nothing to stream. `PROFILE`: rows
     stream, the plan is in the summary at the end (follow-up: the first
     slice lands a `PROFILE` run).
   - Not streamed in the first slice (reads only): see decision 6.
5. **Statements that need all rows before the first can go out.**
   Every `retain` arm of ADR-0660 (aggregation, unlimited `ORDER BY`,
   star aggregation, `Conditional`, `CALL … IN TRANSACTIONS`); a join build
   side; `collect` and percentile operands; `Delete` (holds its driving
   input whole, ADR-0676); `NEXT` for every stage but the last (the later
   stage is seeded from the earlier one's complete answer). Everything
   after such a point still streams.
6. **Writes later, not never.** A write streams rows before the statement
   has committed; if the commit then fails, the client has rows from a
   statement that did not happen. A later ADR decides the semantics
   (rows after commit, or a commit marker in the footer). Until then a
   statement that writes keeps today's behaviour.
7. **Stalled consumer.** A read releases the publication read guard after
   admission but keeps its snapshot pin, so a client that stops reading
   delays follow-reload publication (the swap waits for unpinned readers)
   for as long as it stalls. A write holds the read guard across the whole
   statement, so a streaming write would also block reload and checkpoint
   writers; that is a further reason decision 6 keeps writes unstreamed.
   The cancel token is polled only cooperatively inside the executor, so a
   consumer blocked in a socket write is not interrupted by the query
   timeout: the consumer must use a write timeout, check the token between
   batches and surface a cancelled error. HTTP ends the statement on its
   socket write timeout, Bolt on its idle timeout. A worker panic stays a
   typed error returned to the caller.
8. **Errors mid-stream.** An executor error after the first batch ends the
   statement at the pull that hit it (ADR-0660 error ordering). HTTP has
   already sent `200` and the `data` member; it aborts the stream and
   writes the Query API v2 in-band `errors` member as part of the footer —
   the tail members already written after `data` (`counters`,
   `notifications`, `bookmarks`, plan) — then closes the connection. Bolt
   sends FAILURE at the PULL that hit it.
9. **Delivery in two PRs.** HTTP first, then Bolt (`PULL n` slices
   batches; `DISCARD` stops the stream). MCP results follow with a smaller
   cap on its own.

## Consequences

- Peak memory for a streaming statement without a breaker does not grow with
  the row count, and a test pins it (2M rows under a small pool); a breaker
  retains its own state once, with no second copy.
- Existing callers (CLI, Python, FFI, WASM, and anything using `query`)
  see no change.
- Summary members (stats, timings, plan) arrive after the rows, matching
  the v2 envelope's tail and Bolt's ending summary.
- Public engine API addition: `docs/api/rust` spec, description, example.

## Open questions

None open. The write-statement semantics are deferred to the later ADR
named in decision 6.
