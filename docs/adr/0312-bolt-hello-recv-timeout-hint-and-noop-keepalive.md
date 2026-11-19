---
type: adr
area: surface
claim: 963997d9e26bfdc2758395227b4fe673909b0ea7
name: 0312-bolt-hello-recv-timeout-hint-and-noop-keepalive
status: proposed
tags:
  - bolt
title: Bolt HELLO advertises connection.recv_timeout_seconds and the server sends NOOP keep-alives during long requests
REFERENCES:
  - "[[adr/0110-bolt-connection-query-governance]]"
  - "[[adr/0233-bolt-thread-per-connection]]"
  - "[[adr/0259-query-execution-stack-provisioning]]"
  - "[[adr/0293-configurable-bolt-hello-identity]]"
  - "[[adr/0319-bolt-execution-failures-at-pull]]"
  - "[[adr/0334-heartbeat-boundary-invariants]]"
  - "[[adr/0504-bolt-connection-thread-fast-path]]"
---

# ADR-0312: Bolt HELLO advertises connection.recv_timeout_seconds and the server sends NOOP keep-alives during long requests

## Context

[ADR-0293](./0293-configurable-bolt-hello-identity.md) fixed the HELLO SUCCESS metadata to `server` and `connection_id`
and left `hints` out: "the server has no receive-timeout hint to
advertise". Latest Neo4j (LABS-323 finding F-07) answers HELLO with
`hints: {"connection.recv_timeout_seconds": 120}`, its default of a 60 s
keep-alive interval times two probes.

The hint tells a driver to treat a connection that stays silent for that
long as dead. A server that sends it must keep a long request's
connection alive: Bolt 4.1+ defines the NOOP chunk (`00 00`, an empty
chunk between messages) for that. Without NOOPs, a query that runs past
120 s would be dropped by the driver.

Every RUN executes on the engine connection's 64 MiB query worker
([ADR-0259](./0259-query-execution-stack-provisioning.md)), while the Bolt connection thread waits for it on a condition
variable ([ADR-0233](./0233-bolt-thread-per-connection.md)). That wait is the natural place to write a NOOP.

## Decision

1. **HELLO SUCCESS** is `{server, connection_id, hints:
 {"connection.recv_timeout_seconds": 120}}`. This reverses [ADR-0293](./0293-configurable-bolt-hello-identity.md)'s
   hints-omitted bullet. `server` and `connection_id` are unchanged.
2. **Keep-alive.** While a request's engine work (RUN execution, or
   COMMIT) has gone one keep-alive interval without its reply, the server
   writes a NOOP chunk `00 00` and repeats it every interval until the
   reply is written. NOOPs go only between messages, never inside a
   chunked message: the reply is not started until the engine returns.
3. **One constant pair.** The interval is 60 s and the hint is twice the
   interval, in whole seconds rounded up. `ConnLimits.keep_alive_interval`
   carries the interval (default 60 s) so tests can inject a short one; it
   is a library knob, not a CLI flag. The hint is always derived from it.
4. **Engine change (public Rust API).** `cypherlite::Heartbeat` is a
   callback the waiting caller runs on its own thread once per elapsed
   interval while a statement executes on the connection's query worker.
   It is built with `Heartbeat::every(interval, tick)`, which rejects a
 zero interval ([ADR-0334](./0334-heartbeat-boundary-invariants.md); amended in place while this ADR is still
 `Proposed`). `Connection::query_staged` ([ADR-0319](./0319-bolt-execution-failures-at-pull.md)) takes an optional
   heartbeat; `Connection::commit_with_heartbeat` takes one (a commit
   without a heartbeat is `commit`). The worker wait becomes a timed wait
   only when a heartbeat is passed, so a request that finishes inside the
   interval pays no extra thread spawn and no timer. A COMMIT with a
   heartbeat runs on the existing worker when the connection has one; a
   connection that never ran a query has no worker, and its transaction
   holds no writes, so its COMMIT runs inline.
5. A NOOP write that fails (the client went away) stops further ticks;
   the request still completes, and the failed write closes the
 connection afterwards. The [ADR-0110](./0110-bolt-connection-query-governance.md) idle timeout is unchanged.

## Alternatives considered

- **Keep omitting `hints`.** Drivers then use their own defaults and never
  time out a quiet connection. Rejected: the user decision for LABS-323
  is to match Neo4j, and the hint lets drivers detect dead servers.
- **A keep-alive thread per connection that writes NOOPs on a timer.**
  Rejected: it needs a second writer on the socket (a lock around every
  write) and a thread or timer per connection even for fast requests.
- **Run the engine call on a fresh thread per request and poll it.**
 Rejected: the [ADR-0259](./0259-query-execution-stack-provisioning.md) worker already exists; a timed wait on it costs
  nothing when the statement finishes first.
- **A `--keep-alive-interval-secs` flag.** Rejected for now: no operator
  need is known, and the hint must stay twice the interval, so the pair is
  one constant. Tests use the library field.

## Consequences

- Drivers that honour the hint (Python 5.x+, JS, neo4rs) detect a dead
  server after 120 s of silence and keep long queries alive through the
  NOOPs.
- Clients must skip server NOOP chunks between messages, as Bolt 4.1+
  requires. The in-tree test reader (`protocol::read_message`) skips them.
- The Rust API gains `Heartbeat` (constructed via `Heartbeat::every`,
 [ADR-0334](./0334-heartbeat-boundary-invariants.md)), `Connection::query_staged`'s heartbeat parameter and
  `Connection::commit_with_heartbeat`
  (`docs/api/rust/spec.md`).
- [ADR-0293](./0293-configurable-bolt-hello-identity.md) gains a Later references note pointing here.
- Pinned by `tests/compat/scripts/protocol/hello-recv-timeout-hint.bolt`
  and the NOOP-cadence tests in `protocol_hello_tests.rs`.

## Later references

[ADR-0504](./0504-bolt-connection-thread-fast-path.md) keeps decision 4's wait shape wherever it matters — a statement
or COMMIT that can outlive the interval still runs on the worker with
the heartbeat ticking — and refines it where it does not: on a
caller-stack-asserted connection, bounded-work statements run inline
(no wait exists, so the first-tick rule means no tick is ever due), and
`commit_with_heartbeat` prepares on the connection thread and routes the
flush on the drained redo, so a no-op commit (empty drain) flushes
inline on every connection. The NOOP cadence for long requests is
unchanged.
