---
type: adr
area: surface
claim: 65e5c45abad0b085510ff4712ab578b818c95069
name: 0504-bolt-connection-thread-fast-path
status: proposed
tags:
  - bolt
  - perf
title: Bolt serve's connection-thread fast path for small statements
REFERENCES:
  - "[[adr/0028-user-visible-transactions]]"
  - "[[adr/0233-bolt-thread-per-connection]]"
  - "[[adr/0238-shared-accept-loop]]"
  - "[[adr/0257-query-input-limits]]"
  - "[[adr/0259-query-execution-stack-provisioning]]"
  - "[[adr/0312-bolt-hello-recv-timeout-hint-and-noop-keepalive]]"
  - "[[adr/0334-heartbeat-boundary-invariants]]"
  - "[[adr/0463-expression-allocation-budget]]"
---

# ADR-0504: Bolt serve's connection-thread fast path for small statements

## Context

Every Bolt `RUN` pays the [ADR-0259](./0259-query-execution-stack-provisioning.md) query-worker handoff even
when the statement is trivial. Sampled on this machine (macOS, release
build, `sample <pid>` under a single-client point-read loop), the
connection thread's statement path is `serve_batch → run_statement →
query_staged → run_cancellable → JobSlot::run_with_heartbeat →
JobSlot::wait`, and 93% of the statement-path samples sit in
`pthread_cond_wait`: the connection thread wakes the 64 MiB worker
([ADR-0259](./0259-query-execution-stack-provisioning.md)), parks on a condvar, the worker runs the ~16 µs statement,
signals back, the connection thread wakes and writes the reply. Per
statement that is 2 context switches and 2 condvar syscalls on top of the
engine work. Measured (release, loopback, single client, 20 000
iterations, p50): a point read over Bolt costs 81.5 µs end-to-end
(RUN+PULL) and 51.6 µs on the RUN leg, against 15.6 µs for the identical
statement in-process — the delta is this handoff plus Bolt decode/encode
and two loopback round trips.

The 64 MiB worker exists so deep-recursion statements cannot overflow the
calling thread's stack ([ADR-0259](./0259-query-execution-stack-provisioning.md)) — but the Bolt surface's connection
threads already carry that exact budget: `serve_capped` spawns every
connection thread with an explicit 64 MiB stack, deliberately "matched to
the engine's 64 MiB query-worker stack ([ADR-0259](./0259-query-execution-stack-provisioning.md))" (`CONNECTION_STACK_BYTES`,
[ADR-0233](./0233-bolt-thread-per-connection.md)/[ADR-0238](./0238-shared-accept-loop.md)). For this host the handoff buys no stack safety the
connection thread does not already have. What the worker's *wait shape*
still buys is [ADR-0312](./0312-bolt-hello-recv-timeout-hint-and-noop-keepalive.md)'s NOOP keep-alive: NOOPs tick on the connection
thread while the statement runs on the worker, and a driver honoring the
advertised `connection.recv_timeout_seconds: 120` hint drops a silent
connection — so a statement that could outlive the interval must keep
that shape.

## Decision

Statements run directly on the calling thread of a connection whose
caller has asserted the worker's own stack budget, escalating to the
query worker when the statement could outlive a keep-alive interval.
The escalation boundary is runtime boundedness, not stack depth — stack
safety holds by identity (same stack budget, same [ADR-0257](./0257-query-input-limits.md) depth-500
margin), so no stack-shape classifier is needed.

1. **Engine seam (public Rust API).**
   `Connection::assume_worker_stack_on_caller_thread()` records a
   one-way, per-connection assertion: every later call into this
   connection happens on a thread whose stack carries the engine's
 query-worker provision (64 MiB, [ADR-0259](./0259-query-execution-stack-provisioning.md)) for the connection's
   remaining lifetime. The engine cannot check a thread's stack size —
   the caller owns the guarantee. On wasm32 there is no worker, so the
   assertion changes nothing there. Default is unset: every statement
   runs on the worker, exactly as before, for every host that does not
   assert.

2. **The dispatch split.** On an asserted connection, the one admission
   sequence (`admit_statement`: health gate, failed-transaction gate,
   parse, lowering, planning, the compile-time gates — the recursion
 [ADR-0259](./0259-query-execution-stack-provisioning.md) provisions for) runs on the calling thread, which is safe
   because the asserted stack is the worker's own budget. The body after
   admission (`run_admitted`: the EXPLAIN early return, post-EXPLAIN
   gates, token reservation, dispatch, PROFILE epilogue) is one shared
   function — entered inline when the plan is bounded work, or published
   to the worker with the admission already in hand when it escalates,
   so no path re-admits a statement and no gate can differ between
   paths.

3. **The bounded-work predicate** is one free function over the
   `LogicalPlan`, `cypherlite_planner::plan_is_bounded_work`, riding the
   planner's canonical walks (`plan_any_clause`, the new expression half
   `walk_plan_exprs`, the per-part `walk_plan_parts`), the planner's
   published per-clause mutation table (`clause_mutates_store`), and the
   IR's own shape predicates (`PathPattern::needs_gpm`,
   `Expr::is_scope_bearing`, the IR's pattern-anchor enumeration) — the
   shape of the planner's other classifiers, not a parallel classifier.
   The fast path is **reads-only**: every mutating clause escalates —
   the native write arms and the catalog passthrough's write commands
   (constraint and graph-type DDL, `ALTER DATABASE`, the named index
   forms) alike, one answer from the mutation table — for two reasons:
   an auto-commit write must never commit after its client gave up
 waiting, and inline work cannot tick [ADR-0312](./0312-bolt-hello-recv-timeout-hint-and-noop-keepalive.md)'s NOOPs to keep that
   client alive — while a write's WAL/commit latency dwarfs the handoff
   it thereby keeps. A read plan escalates when any position (full
   descent: UNION arms, `NEXT` stages, subquery bodies, branch bodies,
   `FOREACH` bodies, pattern payloads) holds: `UNWIND`, a procedure
   `CALL`, `CALL {}`, `WHEN`/`ELSE`, `FOREACH`, `LOAD CSV`, a
   variable-length or quantified relationship, a GPM pattern (path
   mode, selector, inline element predicates), a vector `SEARCH`
   subclause, or a scope-bearing expression (pattern predicates,
   `EXISTS`/`COUNT`/`COLLECT` subqueries, `shortestPath`) — or when any
   single pipeline's (one UNION arm / `NEXT` part's) pattern-join graph
   is disconnected: the **cartesian class**, two groups of MATCH
   patterns that share no pattern-bound variable, whose row product is
   quadratic in the store — the one read shape whose runtime can break
   the keep-alive interval's arithmetic. Consecutive MATCH clauses in
   one part product exactly like multiple patterns in a single MATCH,
   so the graph is assessed per part across all of its MATCH clauses; a
   `WHERE`-only join still products first, so only pattern-bound
   variables connect components. The residue — point and joined reads,
   catalog reads (the SHOW family), plain projections — is bounded
 work: buffered results are admission-capped ([ADR-0463](./0463-expression-allocation-budget.md) byte caps, the
   token budget), and a linear scan of the largest supported store is
   bounded by the store's own size limit, orders of magnitude under the
   60 s keep-alive interval. The predicate errs toward escalation:
   anything it misses degrades to today's worker behavior, never to an
   unsafe inline run.


4. **Both [ADR-0259](./0259-query-execution-stack-provisioning.md) guarantees hold on the inline path.** The whole
   inline statement — admission (parse, lowering, planning) and the
   post-admission body — runs under `catch_unwind`, mapped to the same
   typed internal-error class the worker loop reports ("query panicked
   on the caller's thread: …"): the worker loop guards the same span, so
   a panic never unwinds the session's connection thread and the
   connection serves the next statement. The escalate arm stays outside
   the guard — its wait is the unasserted path's wait, tick panics
 included ([ADR-0334](./0334-heartbeat-boundary-invariants.md)), and a worker-side panic is already contained by
   the worker loop. Cancellation is unchanged (the `CancelToken` is
   checked by the executor's entry points and row-production loops
   wherever the statement runs). A heartbeat passed alongside an inline
 statement is never due — there is no wait — which is [ADR-0312](./0312-bolt-hello-recv-timeout-hint-and-noop-keepalive.md)'s own
   first-tick rule for any statement that finishes inside the interval;
   an escalating statement keeps the worker wait and its ticks exactly.

5. **`commit_with_heartbeat` becomes one shape for every connection.**
   `commit` is split into its precondition-and-drain half
   (`prepare_commit`) and its flush half (`flush_commit`); the heartbeat
   form prepares on the connection thread and routes the flush on the
   prepared work — `None` for the no-op drain, the drained redo
   otherwise — the commit's own decision, made once at construction. The preconditions run first, so a failed transaction
 errors ([ADR-0028](./0028-user-visible-transactions.md)) before any worker is spawned for a doomed flush; an
   empty drain is the no-op, flushed inline (a no-op cannot outlive any
 interval, so no wait shape is lost — this refines [ADR-0312](./0312-bolt-hello-recv-timeout-hint-and-noop-keepalive.md) decision
   4's "runs on the existing worker when the connection has one" for
   no-op commits); a redo drain flushes on the connection's worker —
   the running one, or one spawned for the flush — keeping the
   heartbeat's wait shape. A worker-spawn failure after the drain never
   drops the prepared work: the flush completes inline instead, the
   commit's data safety taking precedence over the one lost wait shape —
 the flush is not the input-proportional recursion [ADR-0259](./0259-query-execution-stack-provisioning.md) provisions
   the worker against, and the inline flush is the pre-fast-path
   no-worker behavior.

6. **The Bolt surface asserts where the stack fact lives.**
   `serve_listener_with_cap` — whose shared accept loop spawns every
   connection thread with `CONNECTION_STACK_BYTES`, *derived from* the
   engine's exported `QUERY_WORKER_STACK_BYTES` (the one canonical
   number, not a cross-crate copy) — sets the new
   `ConnConfig::caller_thread_carries_worker_stack` fact — derived from
   that same number (`CONNECTION_STACK_BYTES == QUERY_WORKER_STACK_BYTES`,
   a constant expression, so a future divergence flips the assertion off
   instead of lying about it) — and `handle_conn` applies the engine
   assertion when it holds. Embedders
   driving `handle_conn` on their own threads keep the default (`false`)
   and the worker path. The Bolt test fixtures' one-connection server
   threads now mirror the production spawn (the same derived stack and
   the same fact), so the surface suites exercise the fast path.

## Alternatives considered

- **Keep the handoff but make it cheap (pre-posted job slot,
  spin-then-park wait).** Rejected: it optimizes the switch cost rather
  than the cause — a handoff that exists only to change stacks, which
  this host's connection threads make unnecessary. A caller-side spin
  can elide at most the return-side park; the wake-side futex and
  context switch remain, and between statements (network gaps of
  milliseconds) any spin expires and both sides park again, so the
  steady-state saving is a fraction of the handoff. Under CPU contention
  — the normal state of a busy host — spin-then-park degrades to
  today's full cost while burning cores the server does not have, and a
  128-connection server gains up to 128 hot-spinning threads. All the
  worker machinery stays, plus new spin state to reason about.
- **Per-statement stack-shape classification without the caller-stack
  assertion (escalate on deep pattern shapes, or on first
  recursion-depth fault).** Rejected: for hosts on small stacks no
  static classifier can prove shallow-safety — parse itself recurses
  proportionally to nesting before any plan exists, so the statement
  must already be on a big stack to be classified, and an
 under-classified statement aborts the host process ([ADR-0259](./0259-query-execution-stack-provisioning.md)'s own
  finding: on 2 MiB release builds the depth guard fires below the cap).
  A lexical pre-scan is a second half-parser that must track every
  grammar class that nests. Escalate-on-fault is unreachable: a stack
  fault is SIGSEGV, not a recoverable error. The assertion design makes
  stack classification unnecessary — safety holds by identity — and
  moves the classification question to runtime boundedness, where a
  miss costs a wasted worker handoff instead of a process.
- **All-inline for Bolt, amending [ADR-0312](./0312-bolt-hello-recv-timeout-hint-and-noop-keepalive.md) to drop the NOOP keep-alive.**
  Rejected: it breaks the documented contract for statements past 120 s
  — `UNWIND` over a huge range and the commit of a large explicit
  transaction are real shapes on a real store, and drivers honoring the
  advertised hint drop a silent connection. The wait shape is cheap to
  keep precisely for the statements that can outlive it, because they
  are exactly the ones whose runtime dwarfs the handoff.
- **A keep-alive writer thread per connection (a second writer on the
 socket), enabling all-inline.** Rejected: [ADR-0312](./0312-bolt-hello-recv-timeout-hint-and-noop-keepalive.md) already rejected
  this shape — a lock around every write and a thread per connection
  even for fast requests; resurrecting a rejected alternative to buy
  what escalation already preserves is cost without a gain.
- **Assert at open (`OpenOptions`) rather than per connection.**
  Rejected: the fact is per-connection-thread — one store serves many
  connections on many threads, and an open-level flag would assert on
  behalf of every future connection, including ones on small threads.
  The assertion belongs to the connection whose calls it covers.
- **Make parse and execution iterative so no big stack is needed
  anywhere (then no fast-path boundary exists).** Rejected for this
 decision, as [ADR-0259](./0259-query-execution-stack-provisioning.md) rejected it: a large rewrite, out of scope. If
  it ever lands, this fast path stays correct and the predicate simply
  escalates nothing for stack reasons.

## Consequences

- Measured on this machine (release, loopback, single client, 20 000
  iterations, p50; before = `ca84dd93`): a point read over Bolt costs
  81.5 µs → 66.1 µs end-to-end (−19 %) and 51.6 µs → 43.4 µs on the
  RUN leg; `RETURN $i` costs 66.2 µs → 51.2 µs. Bolt 5.4 and 4.4 are
  identical. The in-process floor for the same point read is 15.6 µs —
  the remaining over-Bolt delta is two loopback round trips plus codec,
  transport rather than server queuing. A `sample` of the served
  process shows the statement path's `pthread_cond_wait` share under
  `JobSlot::wait` going from 93 % of statement-path samples to zero:
  the path now reads `query_staged → admit_statement → run_admitted →
  plan_is_bounded_work` on the connection thread. (The PULL leg of the
  numbers rides the shared accept loop's `TCP_NODELAY`, which landed
  beside this work as LABS-911/#718 — a two-write reply no longer waits
  on the peer's delayed ACK — and reaches this branch through its
  base.)
- [ADR-0259](./0259-query-execution-stack-provisioning.md)'s guarantees are unchanged wherever they applied: the
  worker path is byte-for-byte today's for unasserted hosts (and for
  every write, on any host), and the
  inline path holds the same stack budget (identity), the same
  panic containment (typed internal error, session survives), the
 same cancellation semantics, and the same [ADR-0257](./0257-query-input-limits.md) depth cap — the
  at-cap and beyond-cap proofs of `tests/stack.rs` run inline on a
  64 MiB caller as new tests.
- A host that asserts from a thread without the budget reintroduces
 the pre-[ADR-0259](./0259-query-execution-stack-provisioning.md) exposure for its own process. The method's name
  carries the precondition and the spec table says the caller owns the
  guarantee; the engine does not check, because it cannot.
- Escalated statements on an asserted connection admit once and ship
  the admission to the worker, paying one extra handoff against the
  worker path of today — negligible against the data-proportional work
  that triggered the escalation. The opt-in commit-timing window
  (`timing_on`'s `t_parse`) includes the handoff on that path;
  LABS-832's always-on PARSE/PLAN stamps are measured inside admission
  and are unchanged.
- `cypherlite-planner` gains `plan_is_bounded_work` and the expression half
  of the canonical plan walk (`walk_plan_exprs`, with its
  `plan_any_expr` boolean sibling, crate-private); the facade gains the
  assertion method and exports `QUERY_WORKER_STACK_BYTES` (the budget
  the assertion names — `docs/api/rust/` spec, staged and description
  updated); the Bolt surface derives `CONNECTION_STACK_BYTES` from that
  export and gains the `ConnConfig::caller_thread_carries_worker_stack`
  fact (`docs/api/bolt/` updated).
- [ADR-0233](./0233-bolt-thread-per-connection.md) (the connection thread waits on the worker) and [ADR-0259](./0259-query-execution-stack-provisioning.md)
  (every query on the worker) gain Later-references notes pointing
  here: the boundary is now per-connection, keyed on the caller's
 asserted stack and the plan's boundedness. [ADR-0312](./0312-bolt-hello-recv-timeout-hint-and-noop-keepalive.md)'s note records
  the no-op-commit refinement of its decision 4.
