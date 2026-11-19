---
type: adr
area: surface
claim: cc753eebd904f943add9c45a2f85968a66f38f75
name: 0334-heartbeat-boundary-invariants
status: proposed
tags:
  - bolt
title: Heartbeat invariants are enforced at the boundary (non-zero interval, tick-panic safety)
REFERENCES:
  - "[[adr/0259-query-execution-stack-provisioning]]"
  - "[[adr/0312-bolt-hello-recv-timeout-hint-and-noop-keepalive]]"
---

# ADR-0334: Heartbeat invariants are enforced at the boundary (non-zero interval, tick-panic safety)

## Context

[ADR-0312](./0312-bolt-hello-recv-timeout-hint-and-noop-keepalive.md) added `cypherlite::Heartbeat` — a callback the waiting caller runs
on its own thread once per elapsed interval while a statement or COMMIT
executes on the connection's query worker ([ADR-0259](./0259-query-execution-stack-provisioning.md)) — with public fields
and a prose contract: "Time between ticks. Must be non-zero." Nothing
enforced the contract. A zero interval makes `JobSlot::run_with_heartbeat`
tick, relock and tick again without ever waiting: a hot loop that, through
the Bolt server, writes a NOOP chunk per spin for the whole statement.

The same wait loop is the first place a caller's code runs *between* the
publish and the completion signal of the erased job. The job is a
non-`'static` closure published to the worker as a raw pointer into the
waiting frame (`ErasedJob`, [ADR-0259](./0259-query-execution-stack-provisioning.md)). Before [ADR-0312](./0312-bolt-hello-recv-timeout-hint-and-noop-keepalive.md), no unwinding point
existed inside that window — the worker catches a panicking job itself —
but the new tick call is one: a panicking tick unwinds the waiting frame
before `finished` is set, dropping the frame's locals while the worker may
still dereference the erased pointer. That is undefined behavior, not just
a lost result (rust review HIGH and stellar boundary findings on the
bolt-compat-tests branch, F-1-87 and F-1-91).

Two more review findings sit in the same wait: the loop tracked "a
heartbeat is armed" in two `Option`s that must agree (silently degrading
to an untimed wait if they ever disagreed), and
`Connection::commit_with_heartbeat` took an `Option<Heartbeat>` no caller
ever passed `None` to.

A later stellar round on the same branch found the shape of the fix
(LABS-323): the tick-panic handling had grown inside
`run_with_heartbeat`'s single concurrency-critical frame, mixing publish,
both wait modes, the stash/clear/resume dance and worker-panic reporting;
and the timed loop kept re-arming its timer after the tick died — a timer
with no consumer for the rest of the job. The wait now lives in
`JobSlot::wait`, and a stashed panic drops the timer and finishes the
wait untimed.

A later rust-review round on the same PR found the timed loop's own
deadline arithmetic unguarded: `Instant::now() + interval` panics when
`Instant` cannot represent the sum, and `Heartbeat::every` rejects only
zero, so a caller-supplied oversized interval unwinds the very frame
whose unwind safety this ADR exists to guarantee.

## Decision

1. **The interval contract is enforced by construction.**
   `Heartbeat::every(interval, tick) -> Result<Self>` is the only
   constructor; the fields are crate-private. A zero `interval` returns
   `Error::Semantic("heartbeat interval must be non-zero")` — a typed
   error, not a panic, so a surface with a configured interval (the Bolt
   server's `ConnLimits.keep_alive_interval`) can refuse the request
   instead of aborting a connection thread.
2. **A tick panic cannot unwind the waiting frame while the worker runs.**
   `JobSlot::wait_ticked` — the ticked half of `JobSlot::wait` — wraps
   each tick in `std::panic::catch_unwind(AssertUnwindSafe(...))`, stashes
   the payload, stops ticking, and finishes the wait untimed — the timer
   is dropped with the tick, so no wakeup survives a dead tick. Only
   after the worker sets `finished` does `run_with_heartbeat`
   `resume_unwind` the payload. If the job also panicked in that window,
   the caller's tick panic takes precedence and the worker's stashed
   payload is cleared, so it cannot surface as a later call's error.
3. **The wait keeps one discriminator.** `JobSlot::wait` branches once on
   the single `Option<Heartbeat>`: `None` is the untimed `Condvar::wait`,
   `Some` hands the guard to `JobSlot::wait_ticked` for the timed loop
   over a plain `Instant` deadline. No mirrored
   `Option` pair is re-checked on every wake. A stashed tick panic breaks
   out of the timed loop into the same untimed wait for the rest of the
   job, so the mode can only change once, timed → untimed, and only for a
   tick that can never fire again: it panicked, or its deadline is
   unrepresentable.
4. **The dead nullable mode is removed.**
   `Connection::commit_with_heartbeat` takes `Heartbeat<'_>` non-`Option`;
   `commit()` remains the no-heartbeat path. The Bolt COMMIT arm and the
   docs use the constructor.
5. **An unrepresentable deadline can neither fire nor panic.** The timed
   loop computes each deadline with `Instant::checked_add`; when it
   returns `None` the tick is unreachable — every later `now` overflows
   too — so the wait drops the timer and finishes untimed, the same
   shape as the dead-tick path. The addition is fallible, so an oversized
   interval can never unwind the waiting frame.

## Alternatives considered

- **Keep the prose contract and enforce nothing.** Rejected: a zero
  interval is a hot tick loop (a NOOP flood per statement); the invariant
  was already stated three times and enforced zero times.
- **`expect`/`assert!` on zero instead of a typed error.** Rejected: the
  interval is caller/config-supplied; a server must be able to fail the
  request (`InvalidInput`), not panic a connection thread.
- **A non-zero newtype (`NonZeroDuration`-style) for the interval.**
  Rejected: a new public type and conversion at every call site for one
  field; the checked constructor keeps `Duration` in the signature and is
  the smallest enforced boundary.
- **Let the tick panic unwind immediately ([ADR-0312](./0312-bolt-hello-recv-timeout-hint-and-noop-keepalive.md)'s behavior).**
  Rejected: the worker can still be inside the erased job whose pointer
  targets the unwinding frame — undefined behavior, the reason this ADR
  exists.
- **Convert the tick panic to a typed internal error instead of
  resuming.** Rejected: the tick is the caller's own code, so a panic in
  it is the caller's panic; Rust callbacks propagate it. The safety fix
  is *when* it propagates (after the worker finishes), not its type.
- **Keep ticking after a tick panic.** Rejected: the same tick would
  panic again every interval; waiting out the job is the only safe
  progress.
- **Keep the nullable `commit_with_heartbeat(None)` mode.** Rejected:
  dead branch — no caller passes `None`, and `commit()` already is that
  path; the `Option` forced a re-wrap and a tuple guard for nothing.
- **Keep `Instant + Duration` and let an oversized interval panic.**
  Rejected: that unwind is exactly the UB this ADR exists to prevent —
  the waiting frame must never unwind while the worker may call the
  erased job.
- **Reject an unrepresentable interval in `Heartbeat::every`.** Rejected:
  representability depends on the current instant, so the constructor
  would have to sample a clock, and an interval accepted now can overflow
  later as `now` advances.
- **Saturate the unrepresentable deadline to `Instant::MAX`.** Rejected:
  a wait until the end of time is the untimed wait with an extra timer;
  the `None` branch is that behavior directly.

## Consequences

- A zero heartbeat interval fails at construction with
  `SemanticError: heartbeat interval must be non-zero`; the Bolt
  keep-alive path turns it into an invalid-input IO error before any
  engine call, so a zero configured interval closes the connection
  instead of flooding NOOPs.
- A panicking tick no longer unwinds the caller's frame while the worker
  runs: the job always finishes first, then the panic resumes on the
  calling thread; the slot stays usable for the next job with no stale
  panic payload. The dead tick leaves no timer behind: after the panic
  the wait is untimed, so the caller is not woken every interval by a
  tick that can never run again.
- An interval `Instant` cannot represent from now (only zero is rejected)
  no longer panics the waiting frame: the tick can never fire, so the
  wait is untimed and still ends when the job finishes.
- `Heartbeat`'s fields are private; every construction goes through
  `Heartbeat::every` (the engine, the Bolt server, the docs examples and
  the tests).
- `Connection::commit_with_heartbeat` takes a `Heartbeat`; [ADR-0312](./0312-bolt-hello-recv-timeout-hint-and-noop-keepalive.md)
  item 4 is amended in place (still `Proposed`) to the constructor and
  the non-optional COMMIT signature.
- Pinned by the `query_worker.rs` unit tests (panicking tick waits for
  the job, the dead tick is not re-armed across a long job, the slot
  stays usable, an unrepresentable deadline neither panics nor ticks,
  zero interval rejected), the staged-run tests, and the Bolt keep-alive
  tests.
