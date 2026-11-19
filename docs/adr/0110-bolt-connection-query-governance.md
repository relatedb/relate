---
type: adr
area: surface
date: 2026-09-26
deciders: CypherLite maintainers
name: 0110-bolt-connection-query-governance
status: proposed
tags:
  - bolt
title: Bolt connection and query governance
REFERENCES:
  - "[[adr/0057-embedded-c-abi-and-bolt]]"
  - "[[adr/0082-bound-bolt-decoded-value-count]]"
  - "[[adr/0111-engine-bounded-execution]]"
  - "[[adr/0141-bolt-single-handshake-deadline]]"
  - "[[adr/0259-query-execution-stack-provisioning]]"
  - "[[adr/0311-bolt-tx-open-results-by-qid]]"
  - "[[adr/0319-bolt-execution-failures-at-pull]]"
  - "[[adr/0357-plan-summary-placement-and-neo4j-plan-args]]"
---

# 0110. Bolt connection and query governance

## Context

`cypherlite-bolt` is the optional remote transport ([ADR-0057](./0057-embedded-c-abi-and-bolt.md)). Today its accept
loop spawns one task per accepted socket with no cap — see
`surfaces/cypherlite-bolt/src/main.rs:294-302` — and `listener.accept().await?`
at `main.rs:295` turns an accept error such as `EMFILE` into process exit.
Remote clients can therefore exhaust or wedge the server. Linear LABS-112,
tracked as OPLANE_REQ-00262969 (High, recorded NOT_IMPLEMENTED, reviewed
`d5f61bd`), covers the concrete failure modes:

- **No connection cap** — one task per connection, unbounded.
- **Accept error kills the server** — `listener.accept().await?` propagates and
  exits the process.
- **No idle/read timeouts** — handshake reads (`protocol.rs:21-27`) and chunk
  reads (`protocol.rs:209-231`) block forever, so a stalled partial message
  pins a task indefinitely.
- **Queries block tokio workers** — `query_with_params`/`begin`/`commit`/
  `rollback` are called synchronously from async code (`protocol.rs:85,153,167,
  181`), and the engine waits on the join of a 64 MiB query thread
 (`cypherlite/cypherlite/src/connection.rs:61-83`, [ADR-0259](./0259-query-execution-stack-provisioning.md)).
- **Unbounded result memory** — `PULL` ignores `n` and materializes the whole
  `QueryResult` before streaming (`protocol.rs:119-129`).

Existing controls are transport-local: a 16 MiB message cap and PackStream
depth 128 / 262,144-value caps ([ADR-0082](./0082-bound-bolt-decoded-value-count.md)). [ADR-0082](./0082-bound-bolt-decoded-value-count.md) deliberately deferred
outbound result and `PULL` governance to this server-side stream. The engine's
snapshot-pin stall (256 fixed pin slots; `pin_now()` spins with `yield_now`
forever once all are held, `cypherlite/cypherlite-executor/src/graph/mod.rs:435
-455`) is delegated to [ADR-0111](./0111-engine-bounded-execution.md).

The decisions below were settled by the maintainer before this ADR was written;
this record states them rather than reopening them.

## Decision

Govern connections and query execution with **safe-by-default limits**, not
opt-in controls.

### Connections

- **Connection semaphore, default 128 permits.** Each accepted connection holds
  one permit for its lifetime.
- **Accept-then-close on overflow.** The server still `accept()`s, then
  `try_acquire`s a permit; on failure it logs and immediately closes the
  connection. Over-cap clients are rejected, not queued.
- **Accept errors never exit the process.** `accept()` errors are logged and
  retried with backoff.

### Timeouts

- **Handshake timeout, default 5s** — the magic/version exchange must complete
  within it or the connection closes.
- **Idle per-read timeout, default 300s**, `0` disables it — a stalled partial
  message closes instead of pinning a task.
- **Query timeout, default 0 (off)** — cancellation is driven through the
 engine's `CancelToken` ([ADR-0111](./0111-engine-bounded-execution.md)).

### Query offload

- Engine calls are offloaded via **`tokio::task::spawn_blocking`** so they do
  not block tokio worker threads. `Connection` is `!Sync`; it is moved in and
  out of `spawn_blocking` per engine call.

### Result cap

- **Default 256 MiB of logical bytes.** The cap counts `Value::logical_bytes()`
  over result rows.
- **Over cap = Bolt `FAILURE`** naming the limit; the result is discarded.

### PULL

- At most **`n` RECORDs per PULL**, read from the buffered result.
- `SUCCESS` carries `has_more: true` while rows remain; `n = -1` drains.
- `DISCARD` or a new `RUN` discards pending rows.
- **True executor streaming stays deferred** to the executor stream per
 [ADR-0082](./0082-bound-bolt-decoded-value-count.md).

## Alternatives considered

- **Accept-backlog backpressure** (acquire a permit before `accept()`).
  Rejected: a client can then wait in the TCP backlog indefinitely, and the
  security fix must *reject* beyond-cap clients, not queue them.
- **Per-connection actor thread** owning the `Connection`. Rejected: more
  machinery than needed; `Connection` is `!Sync` (it holds a `RefCell`), and
  moving it in and out of `tokio::task::spawn_blocking` per engine call is
  simpler and already provides the offload.
- **A fixed connection cap with no per-connection lifetime permit.**
  Rejected: a cap that does not return permits on close cannot bound concurrent
  live connections.
- **Per-message-only read timeouts** (reset the clock per chunk/record).
  Rejected: a slow-drip client can keep a connection alive forever by sending
  one byte before each deadline; an idle per-read timeout closes the stall.
- **Result cap off by default (opt-in).** Rejected: safe-by-default is the
  chosen posture; an unbounded outbound result is exactly the memory exhaustion
  this node exists to stop.

## Consequences

- The accept loop gains a `Semaphore` and a backoff retry; `accept()` errors no
  longer terminate the server.
- `cypherlite-bolt` gains per-connection handshake and idle timeouts plus a
 query-cancellation path wired to the engine's `CancelToken` ([ADR-0111](./0111-engine-bounded-execution.md)).
- Engine calls run on `tokio::task::spawn_blocking`; the `Connection` moves in
  and out per call.
- `PULL` becomes genuinely `n`-bounded with `has_more` metadata; true streaming
 remains deferred to the executor stream per [ADR-0082](./0082-bound-bolt-decoded-value-count.md).
- Bolt documentation and regression tests must state and enforce the 128
  connection cap, accept-then-close, the 5s/300s timeouts, the 256 MiB result
  cap, and the `PULL` batching contract.

## Later references

- [ADR-0141](./0141-bolt-single-handshake-deadline.md) refines the handshake timeout: instead of a fresh
  `--handshake-timeout-secs` budget per negotiation phase, one deadline is
  computed at accept time and spans every phase (first byte, TLS handshake,
  post-TLS re-sniff, WebSocket upgrade read, Bolt magic/version exchange),
  capping worst-case pre-service occupancy at one timeout instead of 5×.
- [ADR-0311](./0311-bolt-tx-open-results-by-qid.md) reverses "`DISCARD` or a new `RUN` discards pending rows" for
  explicit transactions: every RUN in a transaction keeps its own open
  result, addressed by `qid`, and the result byte cap bounds the sum of
  all open results on the connection. Auto-commit keeps one result.
- [ADR-0319](./0319-bolt-execution-failures-at-pull.md) keeps the buffered design but moves the report point of a
  failure raised while executing a planned statement to the first PULL or
  DISCARD.
- [ADR-0357](./0357-plan-summary-placement-and-neo4j-plan-args.md) adds the retained RUN statement text to the result cap's
  accounting: an open result reserves the statement's byte length beside
  the rows' `Value::logical_bytes()`, in both the buffered and the
  execution-failed arm.
