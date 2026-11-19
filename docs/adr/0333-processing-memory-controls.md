---
type: adr
area: engine
claim: 33d34f99055871c1a4106c56306ffc1e75998e7d
name: 0333-processing-memory-controls
status: accepted
tags:
  - memory
title: Processing memory controls
REFERENCES:
  - "[[adr/0042-graph-memory-limit]]"
  - "[[adr/0232-network-surfaces-charter]]"
  - "[[adr/0249-docker-unified-serve-image]]"
  - "[[adr/0259-query-execution-stack-provisioning]]"
  - "[[adr/0352-drop-processing-byte-limits]]"
  - "[[adr/0451-bound-unauthenticated-network-connections]]"
  - "[[adr/0452-workspace-limit-enables-admission]]"
---

# ADR-0333: Processing memory controls

## Context

`--memory-limit` caps Logical Graph memory ([ADR-0042](./0042-graph-memory-limit.md)). It does not cap
the memory used to run queries, hold sessions, or checkpoint. That
processing memory scales with connections, concurrent queries, retained
rows, write scratch, and checkpoint work. Without a control on those
vectors, Operating RSS has no ceiling an operator can set, and a
container memory limit is a guess that ends in an OOM kill.

`cypherlite serve` listens on Bolt, HTTP, and MCP in one process
([ADR-0232](./0232-network-surfaces-charter.md), [ADR-0249](./0249-docker-unified-serve-image.md)). Each listener currently builds its own connection
cap of 128, so three surfaces admit 384 sessions. Session cost is
process-wide: a Bolt session and an HTTP session both take a connection
thread and, after the first query, a parked query worker.

## Decision

Four controls. Each ships unlimited: omitting the flag, leaving its
environment variable unset, or passing `0` turns that control off. A
positive value enables it. `--memory-limit` stays the Logical Graph cap.
The query-worker stack size stays the fixed [ADR-0259](./0259-query-execution-stack-provisioning.md) value.

- `--max-connections` (`CYPHERLITE_MAX_CONNECTIONS`) bounds sessions in
  the process. Over cap is accept-then-close. The log names the flag,
  the value, and the surface. `cypherlite serve` creates one
  `ConnectionCap` and passes it to every listener, so Bolt, HTTP, and
  MCP share it. The previous default of 128 per listener is removed.
- `--max-active-queries` (`CYPHERLITE_MAX_ACTIVE_QUERIES`) bounds
  queries executing at once.
- `--query-memory-limit` (`CYPHERLITE_QUERY_MEMORY_LIMIT`) caps the
  estimated bytes one query retains.
- `--processing-memory-limit` (`CYPHERLITE_PROCESSING_MEMORY_LIMIT`)
  caps estimated bytes of active queries plus checkpoint work together.

When both byte limits are set, the query limit must be less than or
equal to the processing limit. A query is admitted only while every
enabled control has room.

Growth of retained rows (aggregation, projection, sort, distinct),
write-scratch entities, and an explicit checkpoint charges the budget
before the structure grows. Crossing `--query-memory-limit` is a typed
`ResourceLimit` and is not retryable. Crossing `--max-active-queries`
or `--processing-memory-limit` is the same error and is retryable. The
statement does not commit, the reservation is released, and the session
stays usable. Bolt passes the message through as `CypherLite.Error`.
An automatic checkpoint that cannot reserve waits and retries, and
skips that round rather than failing the database.

The operating ceiling, when the corresponding flags are set, is:

```text
cold Graph RSS
+ measured session cost × --max-connections
+ --processing-memory-limit
+ measured allocator margin
```

A flag that is off drops out of that sum. The ceiling is derived from
the controls and from RSS samples. It is not itself a flag.

## Alternatives considered

- **Per-surface connection caps.** Rejected. Three independent caps of
  128 hide the process session count the RSS ceiling has to use.
- **A default processing ceiling.** Rejected. A new default that refuses
  work which runs today is a behavior change. Operators opt in.
- **An operator-set worker stack size.** Rejected for now. The 64 MiB
  stack is the budget the nesting cap was calibrated against
 ([ADR-0259](./0259-query-execution-stack-provisioning.md)). Lowering it without re-proving that cap reopens
  stack-abort failures.

## Consequences

- Deployments that omitted `--max-connections` now accept more than 128
  sessions per listener until they set the flag. The load tests set the
  caps they push.
- A set `--max-connections` on `serve` is one process total, shared by
  every surface.
- Charges are conservative byte estimates. A path that grows RSS
  without a typed error is a missing charge.
- Linux load tests sample the server process RSS, repeat the workload,
  and fail on panic, on idle RSS growth, or on a limit hit that is not
  the typed error.

## Later references

[ADR-0352](./0352-drop-processing-byte-limits.md) drops `--query-memory-limit` and `--processing-memory-limit`.
Those flags, their environment variables, the byte-charge rules, and the
`--processing-memory-limit` term in the operating ceiling above no
longer apply.

[Token budgets](../design/token-budgets.md) removes
`--max-active-queries` and `CYPHERLITE_MAX_ACTIVE_QUERIES`. Token
admission is the only query-concurrency control, and it is opt-in.
`--max-connections` stays the session cap.

[ADR-0452](./0452-workspace-limit-enables-admission.md) removes the separate `--token-admission` switch. A positive
`--concurrent-logical-workspace-limit` is what turns admission on.

[ADR-0451](./0451-bound-unauthenticated-network-connections.md) restores a non-zero default for `--max-connections` (128,
process-wide) and adds the absolute request-header and body deadlines.
The flag, its environment variable, and the one-shared-cap decision above
still apply; the unlimited default and the `0`-only posture do not — `0`
stays the explicit unlimited spelling and keeps this ADR's non-loopback
warning.
