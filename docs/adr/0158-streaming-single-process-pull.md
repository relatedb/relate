---
type: adr
area: surface
name: 0158-streaming-single-process-pull
status: accepted
tags:
  - load
title: Streaming single-process pull, in-memory identity, refuse non-empty destinations
REFERENCES:
  - "[[adr/0080-cli-pull-command]]"
  - "[[adr/0086-bounded-persistence-input]]"
  - "[[adr/0092-manifest-bound-pull-identity]]"
  - "[[adr/0144-graph-examples-differential-suite]]"
  - "[[adr/0155-bulk-writes-batched-unwind]]"
  - "[[adr/0181-typed-query-api-wire-for-pull]]"
SUPERSEDES:
  - "[[adr/0080-cli-pull-command]]"
  - "[[adr/0144-graph-examples-differential-suite]]"
---

# ADR-0158: Streaming single-process pull, in-memory identity, refuse non-empty destinations

## Context

`cypherlite pull` ([ADR-0080](./0080-cli-pull-command.md)) loads a subgraph from a Neo4j Query API v2
source into a local database. The measured load path after the [ADR-0155](./0155-bulk-writes-batched-unwind.md)
batching work is still structurally slow on whole-database pulls:

- The HTTP client buffers one full response per statement; a whole-database
  shape query exceeds the 64 MiB response cap, so callers page manually
  (`SKIP`/`LIMIT`) and run **one full CLI process per page**. Every process
  ends with a full checkpoint (all Parquet shards plus the persisted
  pull-identity table) and an fsync — so an N-page pull rewrites the store
  O(N × total) times (measured: recommendations, 28,863 nodes + ~150k rels,
  4 pages ≈ 8.7 min in a debug build; ~70% of each page's local time is the
  end-of-load checkpoint, not materialization).
- The pull identity table (source element id → local id) is persisted
 ([ADR-0092](./0092-manifest-bound-pull-identity.md), 256 MiB load-state budget) so that a *re-pull* can reuse
  existing elements. In practice re-pulls write into a database that already
  has the data, which is at best a duplicate-creating operation and at worst
  a silently divergent copy.

The paging itself is the problem, not the page size: the sandbox
differential suite ([ADR-0144](./0144-graph-examples-differential-suite.md)) had to synthesize deterministic partitions
(`ORDER BY elementId` + `SKIP`/`LIMIT` + carried endpoints) purely to stay
under per-page caps — logic that belongs inside the loader, not in every
caller.

## Decision

1. **One pull process, streaming statements.** `cypherlite pull --from
   ENVFILE --query VALUE DATABASE` executes a *list* of statements in one
   process:
   - `VALUE` is split on `;` into statements (the engine's statement
     splitter), executed in order, all against the same connection;
   - `VALUE` may name a file (`pull.cypher`) — its content is the statement
     list;
   - `VALUE` `-` reads the statement list from stdin.
   One process = one transaction flow = **one checkpoint and one fsync at
   the end**, regardless of statement or batch count.
2. **Streaming materialization.** The loader reads the Query API v2 response
   body as a stream (chunked transfer), extracts result rows from the
 `values` array incrementally, and materializes them into [ADR-0155](./0155-bulk-writes-batched-unwind.md) UNWIND
   batches as they arrive. No full-response buffer; no manual
   `SKIP`/`LIMIT` paging in callers. A statement without `LIMIT` may return
   the whole database; the response cap no longer bounds a pull.
3. **Pull refuses non-empty destinations.** `pull` into a database that
   already has elements or stored state fails with a typed error. There is
   no `--incremental` escape yet — incremental/idempotent re-pull semantics
   (MERGE on constraint-derived keys) stay a separate future decision.
4. **The pull identity map is load-scoped memory, not persisted state.**
   The map (source element id → local id) exists only during the pull
   process, where it resolves endpoints and cross-statement references
   (nodes created by an earlier statement in the list are the `a`/`b`
   endpoints of a later statement's relationships). It is dropped when the
 process ends. Persisted pull-identity objects ([ADR-0092](./0092-manifest-bound-pull-identity.md)) are no longer
   written, read, or validated; existing ones are ignored on open.

## Alternatives considered

- **Keep manual paging, bigger pages.** Rejected: the per-page process is
  the cost multiplier (checkpoint + fsync per page), and every caller has
  to re-derive deterministic partitions; raising `PAGE_ELEMENTS` only moves
  the caps.
- **Keep the persisted identity table for future incremental pulls.**
  Rejected for now: with the refuse-non-empty rule nothing can consume it;
  keeping durable state that no feature reads is pure cost (16 → 256 MiB
  budget, validation, sweep). If incremental re-pull lands (MERGE mode,
  LABS-159), the map returns as load-scoped state anyway — the source of
  truth for merge keys is the source's unique constraints, not a stale
  local table.
- **Loader-native write API (bypass Cypher statements).** Rejected for now:
 the WAL records the same logical writes and the [ADR-0155](./0155-bulk-writes-batched-unwind.md) statements
  already measure at engine speed; a native API is a larger surface change
  without a measured need.

## Consequences

- Whole-database pulls are one process with one checkpoint: the O(pages ×
  store) rewrite disappears; the sandbox suite's five datasets pull in
  seconds (measured single-page release baseline ≈ 4.4 s per statement,
  half of it oracle-side).
- The `--query` flag table and hand-written `--help` change (multi-statement,
  file, stdin) — `docs/api/cli/spec.md` updates in the same change.
- [ADR-0092](./0092-manifest-bound-pull-identity.md)'s manifest-selected pull-identity objects are retired: existing
  stores' `pull-identity/` objects are inert legacy data — never read and
  outside storage's known-object sweep (storage carries no pull knowledge
  at all); they linger on old stores until the directory is recreated. The
 256 MiB load-state budget ([ADR-0086](./0086-bounded-persistence-input.md) amendment) has no persisted object to
  bound anymore.
- Rel-reuse anchoring ([ADR-0155](./0155-bulk-writes-batched-unwind.md)'s `elementId(r) = row.rid` reuse batch) only
  applies within a single pull's statement list; the LABS-161 engine-side
  rel seek stays relevant only if reuse returns with incremental mode.
- The streaming reader adds a small purpose-built scanner for the Query API
  v2 `values` array (serde_json parses each complete row; the array itself
  is never buffered whole). It is contained in `surfaces/cypherlite-load`.
- Hang protection for a stalled source mid-body is the caller's: ureq's
  body timeouts are whole-body budgets (a streaming pull legitimately runs
  minutes), so the loader takes only a connect timeout. The CLI runs under
  the invoking process's control; the suite kills the child past its budget.

## Later references

[ADR-0181](./0181-typed-query-api-wire-for-pull.md) fixes the pull wire format: `load_streaming` now requests the
typed Query API JSON media type and decodes each `$type`-tagged value into
an engine value, failing the pull on tags it cannot represent. The
streaming scanner, the one-process/one-checkpoint shape, and the
refuse-non-empty rule decided here are unchanged.
