---
type: adr
area: engine
date: 2026-09-26
deciders: CypherLite maintainers
name: 0111-engine-bounded-execution
status: proposed
tags:
  - executor
  - memory
title: "Engine bounded execution: cooperative cancellation and bounded snapshot pins"
REFERENCES:
  - "[[adr/0028-user-visible-transactions]]"
  - "[[adr/0038-lock-free-snapshot-pins]]"
  - "[[adr/0082-bound-bolt-decoded-value-count]]"
  - "[[adr/0259-query-execution-stack-provisioning]]"
---

# 0111. Engine bounded execution: cooperative cancellation and bounded snapshot pins

## Context

LABS-112, tracked as OPLANE_REQ-00262969, asks the Bolt surface to enforce a query
timeout and to be robust when clients misbehave. Two engine-side gaps make that
impossible to honor at the transport layer alone.

**Snapshot pins are unbounded.** `cypherlite-executor` keeps 256 fixed slots
(`PIN_SLOTS`) of `AtomicU64` in `cypherlite/cypherlite-executor/src/graph/mod.rs`.
`pin_now()` (around lines 435-455) CAS-claims a slot; when every slot is
occupied it spins `std::thread::yield_now()` and retries **forever**. An open
explicit transaction ([ADR-0028](./0028-user-visible-transactions.md)) holds a pin until commit, rollback, or
disconnect. With no authentication and no idle timeout, 256 BEGIN-then-idle
clients can occupy every slot and wedge every read and every commit permanently.
The engine has no way to ask a spin to stop. [ADR-0038](./0038-lock-free-snapshot-pins.md) is the Accepted decision
that created the slot array and explicitly chose "spin + yield and retry" over a
mutex.

**Queries cannot be cancelled.** `Connection` runs all query work on its
64 MiB worker thread ([ADR-0259](./0259-query-execution-stack-provisioning.md)), and the caller blocks until that worker
finishes the job. There is no query timeout or cancellation anywhere in the
engine. When the Bolt surface's query timeout fires it has no mechanism to stop
the running query; the caller stays blocked until the query finishes on its own.
A well-behaved but slow query (large scan, long expansion) holds a connection
and a worker indefinitely.

The engine materializes a full `QueryResult` (`rows: Vec<Vec<Value>>`); true
streaming is out of scope ([ADR-0082](./0082-bound-bolt-decoded-value-count.md) defers it to the executor stream). The
decision below is the two-part **bounded execution** contract that later Bolt
resource-governance work builds on.

## Decision

### Cooperative cancellation

Add a public `cypherlite::CancelToken`: an `Arc`-backed `AtomicBool` with
`cancel(&self)`, `is_cancelled(&self) -> bool`, and `Clone`. Cloning shares the
same flag, so a transport can hold a handle while the worker checks it.

Add a cancellable query entry point on `Connection`:

```rust
pub fn query_with_params_cancellable(
    &mut self,
    cypher: &str,
    params: BTreeMap<String, Value>,
    token: CancelToken,
) -> Result<QueryResult>;
```

The existing `query` and `query_with_params` keep their signatures and delegate
to the cancellable path with a token that is never cancelled. The executor
checks the token at row-production points and in long loops. A cancelled query
returns a typed engine error whose message contains **"query cancelled"**.

### Bounded pin acquisition

`pin_now()` attempts the slot array for a bounded number of yields — roughly
microseconds to low milliseconds — then returns a typed error instead of
spinning forever. The error message contains **"snapshot pin"**. This is the
[ADR-0038](./0038-lock-free-snapshot-pins.md) follow-up: the slot protocol and the lock-free hot path are unchanged,
but a full table is surfaced as a typed error rather than an infinite stall.

## Alternatives considered

- **Keep the unbounded spin.** Rejected: with 256 pins held, every read and
  commit wedges permanently. That is a liveness bug, not just a slowdown, and no
  timeout or disconnect can recover it.
- **Mutex fallback for pin acquisition when the slots are full.** Rejected per
 [ADR-0038](./0038-lock-free-snapshot-pins.md)'s lock-free rationale: the mutex was removed from the hot path
  deliberately, and a full table is better surfaced as a typed error than as
  hidden contention on a fallback lock.
- **Deadline-only API without a token.** Rejected: a deadline cannot express
  disconnect or abort cancellation and forces clock checks into the executor. A
  token covers both timeout and teardown with one mechanism.
- **Blocking acquire with a condition variable.** Rejected: it reintroduces a
  blocking primitive on the query path and still cannot bound the wait; a
  bounded retry that fails fast is simpler and matches the token model.

## Consequences

- `cypherlite` gains `CancelToken` as public API; `docs/api/rust/` must gain its
  spec, description, and an example, and the cancelled-error message is part of
  the contract.
- `query` and `query_with_params` are unchanged for existing callers and run
  with a never-cancelled token.
- `pin_now()` no longer blocks forever; callers get a typed "snapshot pin" error
  and can retry or fail the statement.
- The Bolt surface can implement its query timeout and disconnect handling by
  cancelling the token, which is the reason this decision precedes that work.
- [ADR-0038](./0038-lock-free-snapshot-pins.md)'s slot array and lock-free protocol are preserved; only the
  all-slots-occupied behavior changes, from spin to bounded wait plus error.
