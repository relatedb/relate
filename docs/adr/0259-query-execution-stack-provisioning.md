---
type: adr
area: engine
date: 2026-09-22
deciders: CypherLite maintainers
name: 0259-query-execution-stack-provisioning
status: proposed
tags:
  - executor
title: "Query-execution stack provisioning: big-stack worker thread at the engine layer"
REFERENCES:
  - "[[adr/0042-graph-memory-limit]]"
  - "[[adr/0257-query-input-limits]]"
  - "[[adr/0504-bolt-connection-thread-fast-path]]"
---

# 0259. Query-execution stack provisioning: big-stack worker thread at the engine layer

- **Renumbered**: from 0080 (shared-number cleanup)

## Context

[ADR-0257](./0257-query-input-limits.md) capped query nesting depth at 500 (`MAX_NESTING_DEPTH`) and
calibrated that cap on 8 MiB stacks, where release-build probing verified
the guard fires 3.3×–48× before stack exhaustion across all 10 grammar
classes. A follow-up security review (LABS-69/70/81) verified what happens
on *small* stacks in release builds: on a 2 MiB stack, parse+lower
SIGABRTs at ~310–320 nested parentheses end-to-end, ~480 nested
`FOREACH`, and ~400 `CALL {}` inside a pattern predicate — all **below**
the 500 cap, so no [ADR-0257](./0257-query-input-limits.md) guard fires and the process aborts before the
typed error can be produced.

This is not hypothetical:

- **Bolt serves pre-auth `RUN` on 2 MiB workers.**
  `crates/cypherlite-bolt/src/main.rs` uses `#[tokio::main]` with no
  `thread_stack_size` override, so connections (and their queries) run on
  tokio's documented 2 MiB default worker stacks. An unauthenticated
  client that completes the handshake can abort the server process
  remotely with a single deeply nested query.
- **Any C-ABI embedder on a small-stack thread is equally exposed.**
  Rust `std::thread` defaults, FFI consumer runtimes, and Python
 embedding threads routinely run with ≤2 MiB stacks. As [ADR-0257](./0257-query-input-limits.md) noted,
  through the C ABI an abort kills the host process and cannot be
  surfaced as a recoverable error.

The root cause is that the depth cap is a *proxy* for stack consumption,
and the conversion factor (bytes of stack per nesting level) is a
property of the **caller's thread**, not of the query. A single global
number can only be correct for one stack size. [ADR-0257](./0257-query-input-limits.md) correctly picked
the number for big stacks; the missing piece is controlling the stack the
recursive work actually runs on. [ADR-0042](./0042-graph-memory-limit.md) does not engage (this is
pre-commit, pre-allocation recursion), and the [ADR-0257](./0257-query-input-limits.md) caps stay
exactly as they are — they bound *work*, this ADR bounds *where the work
runs*.

The test suite already demonstrates the pattern: `with_big_stack`
(`crates/cypherlite-tck/src/runner.rs`) and the [ADR-0257](./0257-query-input-limits.md) limit tests in
`crates/cypherlite-front/src/lower.rs` run recursive suites on an
explicit 64 MiB worker thread.

## Decision

**All query work that recurses proportionally to input nesting —
parse, front-end lowering/validation, and execution — runs on one
big-stack worker thread owned by the engine `Connection`**, inside the
`cypherlite` crate's single query choke point,
`Connection::query_with_params` (`cypherlite/cypherlite/src/connection.rs`;
`Connection::query` and `Connection::execute` funnel into it). The
worker is spawned on the first query and joined when the connection is
dropped. One choke point covers every host — the Rust API, the C ABI
(`cypherlite-ffi`), the Python bindings, the CLI, and
`cypherlite-bolt` — transparently, the same way [ADR-0257](./0257-query-input-limits.md)'s single
parse choke point covers every entry point with one check. Bolt, the
CLI, and the FFI bindings already hold an engine `Connection` from
`Database::connect` and drop it when the session ends.

- **Worker stack size: 64 MiB (67 108 864 bytes)**, set via
  `std::thread::Builder::stack_size`. This matches the size the test
 suite and the [ADR-0257](./0257-query-input-limits.md) limit tests already use, and is 8× the 8 MiB
  stack on which the depth-500 cap was probe-verified to fire with
  3.3×–48× margin on release frames. The guard therefore fires long
  before exhaustion with very large margin, in debug and release builds,
  on every supported platform.
- **Effective depth cap: unchanged at 500** ([ADR-0257](./0257-query-input-limits.md)). The cap now has
  a fixed, known stack budget to be correct against, because the engine
  — not the host — controls the stack.
- **One worker per connection, for the connection's lifetime after the
  first query.** The thread is created on the first statement, not in
  `Database::connect`, so a session that is accepted and then sits idle
  without running a statement does not reserve a stack. Later statements
  on that connection publish a job to the parked worker and wait. The
  stack reservation is virtual; pages are committed only as the recursion
  touches them — with the 500 cap, worst-case touched stack is bounded
  at roughly what 500 levels consumed in the 8 MiB probe, ≲2.5 MiB.
  `Connection` stays on the caller. It is `!Sync`, and queries on one
  connection are serialized, so the parked thread matches that. Dropping
  the connection shuts the worker down and joins it.
- **Spawn failure is a typed error.** If the OS refuses the thread (or
  the address-space reservation), `Connection::query_with_params`
  returns a typed engine error naming the resource failure and leaves
  the worker unset; it never falls back to running the query on the
  caller's stack. The next query may try to spawn again.
- **A worker panic is a typed error, never a host abort.** A panic
  inside the worker is caught in the worker loop and returned as a typed
  internal error; it never unwinds into or aborts the host, and the same
  thread stays parked for the next statement. The workspace sets no
  `panic = "abort"`, so unwind is available in every profile. (For
  `cypherlite-bolt`, the wait is a blocking wait on a tokio worker
  thread — sound today because queries already run synchronously there.)
- **Caller stack requirements disappear.** The calling thread only
  publishes a job and waits — constant, shallow frames. The first query
  also performs the spawn. Hosts calling from threads with <2 MiB stacks
  (including tokio's 2 MiB workers and small embedded threads) are safe
  without any action on their part.
- **Public specs may now claim**: for any query text within the
 documented input limits (≤4 MiB, nesting depth ≤500, [ADR-0257](./0257-query-input-limits.md)), the
  engine returns a typed error rather than aborting, **regardless of the
  calling thread's stack size**, under the sole assumption that the
  process can spawn one 64 MiB-stack thread per connection that has
  executed a query. The "typed error, never abort" contract is no longer
  conditioned on the host providing a big stack.

## Alternatives considered

- **Dedicated big-stack thread in `cypherlite-bolt` only.** Rejected.
  It fixes the remote pre-auth kill but leaves every other host — the C
  ABI, the Python bindings, and Rust API callers on small-stack threads
  — exposed to exactly the same abort from the same query text. The
  vulnerability is a property of the engine's recursion, not of the Bolt
  transport; the fix belongs at the engine layer where one choke point
  covers all hosts.
- **Lower `MAX_NESTING_DEPTH` to a value proven safe on 2 MiB release
  (measurements suggest ≤150).** Rejected. It is user-visible: it
  rejects legitimately deep queries in the 150–500 band that today work,
  and the TCK corpus itself drives deep recursion (e.g. the
  `Literals7`/`Literals8` features recurse once per nesting level), so a
  150 cap risks failing currently-passing TCK scenarios — the skip list
  only shrinks. Worse, it is whack-a-mole: the safe constant is a
  function of frame sizes, which vary by platform, compiler version, and
  build profile, and it still abandons hosts with *smaller* than 2 MiB
  stacks (512 KiB embedded threads abort at proportionally shallower
  depth). Calibrating a proxy against an uncontrolled variable cannot be
  made sound; controlling the stack can.
- **Bolt-only tokio `thread_stack_size` bump.** Rejected. A one-line
  change, but it only re-sizes the Bolt host and leaves FFI, Python, and
  Rust API callers on small threads exactly as exposed as today. It also
  permanently commits 8+ MiB of reservation to *every* tokio worker —
  including ones that never run a deep query — instead of paying the
  reservation per query.
- **Combination: engine-layer big-stack execution *and* a Bolt stack
  bump, keeping 500.** Rejected. Once the engine runs the recursive work
  on its own 64 MiB worker, the Bolt worker stack no longer carries any
  input-proportional recursion — protocol handling is iterative and
  shallow — so the bump buys nothing. Two mechanisms guarding one hazard
  is cost and confusion without defense in depth; the Bolt
  message-chunk accumulation concern is tracked separately (LABS-76).
- **Persistent per-`Database` worker thread or worker pool.** Rejected.
  A pool's address space does not follow the number of open connections:
  it stays reserved after sessions close. Bolt already caps connections
  and expires idle ones, and every surface already owns an engine
  `Connection`, so one worker per connection scales down when sessions
  end. A panicking statement is caught and the same worker continues;
  the connection is still the failure domain. Moving the session into
  the worker was also rejected: `Connection` holds the caller's
  transaction state, and a persistent thread cannot own that borrow
  between calls.
- **Make the parser and walkers iterative so no deep stack is needed.**
 Rejected (again; see [ADR-0257](./0257-query-input-limits.md)). A large rewrite that this decision
  does not preclude; if it ever lands, the worker thread remains correct
  and simply always has headroom.

## Consequences

- The remote unauthenticated process-kill against `cypherlite-bolt`, and
  the equivalent host-process kill through the C ABI and Python
 bindings, are closed: deeply nested input now reaches the [ADR-0257](./0257-query-input-limits.md)
  typed error on every host, because the recursion runs on a
  64 MiB engine-owned stack with 3.3×–48×+ verified margin.
- The public specs (`docs/api/rust/`, `docs/api/c/`, `docs/api/cli/`,
  `docs/api/python/`, `docs/api/bolt/`) may state the stronger contract:
  oversized or over-nested queries produce typed errors, never aborts,
  for any caller stack size, assuming the process can spawn one 64 MiB
  worker thread per connection that has executed a query. Spec wording
  lands in the same change as the implementation.
- Each connection that has run a query carries one 64 MiB virtual stack
  reservation until it is dropped. On 64-bit hosts this is address space,
  not RSS; pages are committed only as touched, and the 500 cap bounds
  touched stack at ≲2.5 MiB per worker. Bolt concurrency of N sessions
  that have each run a query therefore costs at most N×64 MiB of
  *address space* and N×(touched depth) of real memory. Sessions that
  never query, and sessions that have closed, do not.
- `Connection::query_with_params` (and the `Connection::query` /
  `Connection::execute` funnels) gains one new failure mode —
  worker-spawn failure — surfaced as a typed error; there is no silent
  fallback to the caller's stack. A worker panic likewise surfaces as a
  typed internal error, and the same worker stays parked for the next
  statement. The thread is joined when the connection is dropped.
- The [ADR-0257](./0257-query-input-limits.md) constants (4 MiB, 500) are unchanged; this ADR is the
  provisioning that makes 500 a sound number everywhere instead of only
 on big stacks. Implementing nodes must run the [ADR-0257](./0257-query-input-limits.md) limit and
  limit+1 tests through the engine query path (`Connection::query_with_params`,
  not only the parser / front-end directly) on small-stack caller
  threads, in debug and release CI lanes, proving the caller's stack is
  never the one that recurses.

## Later references

[ADR-0504](./0504-bolt-connection-thread-fast-path.md) narrows the worker-for-every-statement rule for one class of
connection: a caller that asserts its calling thread carries this ADR's
own 64 MiB provision (`Connection::assume_worker_stack_on_caller_thread`)
gets bounded-work statements inline on that thread, escalating to the
worker when the plan could outlive a keep-alive interval. Every guarantee
here holds on both paths — the stack budget is identical (the assertion
is the same number this ADR chose), a panic is contained to the same
typed internal error, and the depth cap is unchanged. Unasserted hosts
are byte-for-byte this ADR's behavior.
