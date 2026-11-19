---
type: adr
area: surface
date: 2026-09-24
deciders: CypherLite maintainers
name: 0099-fallible-memory-usage-and-pull-lookup
status: proposed
tags:
  - memory
  - wal
title: Fallible `memory_usage` and `pull_lookup` public APIs
REFERENCES:
  - "[[adr/0094-preflight-wal-acceptance]]"
---

# 0099. Fallible `memory_usage` and `pull_lookup` public APIs

## Context

[ADR-0094](./0094-preflight-wal-acceptance.md) makes a fenced/poisoned handle observable: once a physical WAL
outcome is uncertain, subsequent reads and writes must fail admission with a
typed `Error::Storage("uncertain WAL outcome…")` until the database is closed
and reopened. Every admission path routes through `Shared::read_guard`, which
returns that typed error.

Two public methods on the embedded Rust API bypassed that contract by
swallowing the guard error:

- `Database::memory_usage()` matched on `read_guard()` with
  `let Ok(_read) = … else { return (0, None) };`. Its own documentation states
  that `None` means "unlimited", so a poisoned handle was reported as an
  empty, unlimited graph — indistinguishable from a healthy empty database and
  directly contradicting fail-closed behavior.
- `Connection::pull_lookup()` used `read_guard().ok()?`, so a poisoned handle
  returned `None`, the same value as "no pull mapping for this element". A
  caller could not tell a fenced database from a genuine miss.

Both are public API; changing their signatures is a decision that must be
recorded before implementation (AGENTS.md §1). The fix tightens the error
contract without changing the healthy-path values.

## Decision

Make both methods fallible, propagating the typed poisoned/unhealthy error
from `read_guard()`:

1. `Database::memory_usage(&self) -> Result<(u64, Option<u64>)>`. Acquire
   `read_guard()?`, then return the live logical bytes and the configured cap
   exactly as before (`None` limit means unlimited). A poisoned/fenced handle
   returns `Error::Storage` instead of `(0, None)`.
2. `Connection::pull_lookup(&self, source: &str, kind: PullKind,
   remote_id: &str) -> Result<Option<i64>>`. Acquire `read_guard()?`, then run
   the existing lookup logic unchanged; `Ok(None)` keeps meaning "no live
   mapping", while a poisoned handle is `Err`.

All in-tree callers are updated in the same change: binaries and loadtests
propagate with `?` (or map into their own error type), tests and examples use
`.unwrap()`/`?`. The public docs under `docs/api/rust/` record the new
signatures and error contract.

## Alternatives considered

- **Keep `memory_usage` infallible and return `(0, None)` on poison.**
  Rejected: `(0, None)` is documented as an empty, unlimited graph, so a
  fenced handle becomes indistinguishable from a healthy one — the opposite of
  fail-closed.
- **Keep `pull_lookup` infallible and return `None` on poison.** Rejected:
  "database poisoned" and "no pull mapping for this element" collapse into the
  same value, hiding an unusable handle from the caller.
- **Panic on a poisoned handle.** Rejected: no public API in this crate may
  panic on a recoverable, typed condition; a poison is exactly the case a
  caller must handle by closing and reopening.
- **Expose a separate `is_poisoned()` predicate and keep both methods
  infallible.** Rejected: it forces every caller to remember a second call and
  creates a check-then-use race against a concurrent poison; the guard's own
  error already carries the right information atomically.

## Consequences

- `memory_usage` and `pull_lookup` are breaking public-API signature changes;
  Rust callers must handle the `Result`. This is intentional and lands with
  the doc and caller updates.
- Healthy behavior is unchanged: the same bytes/limit and the same mapping
  result. Only the poisoned/fenced path differs, now surfacing the same typed
 error as the rest of the read-admission surface ([ADR-0094](./0094-preflight-wal-acceptance.md)).
- Tests gain a direct way to assert that a fenced handle fails these reads
  rather than silently reporting an empty graph.
