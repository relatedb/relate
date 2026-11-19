---
type: adr
area: cli
name: 0241-query-timeout-flag-and-exit-code-7
status: accepted
tags:
  - commands
title: "`--timeout` query flag and exit code 7"
REFERENCES:
  - "[[adr/0111-engine-bounded-execution]]"
  - "[[adr/0194-cli-error-envelope-and-exit-codes]]"
  - "[[adr/0228-ontology-list-property-types-closed]]"
---

# ADR-0241: `--timeout` query flag and exit code 7

## Context

(Numbered [ADR-0228](./0228-ontology-list-property-types-closed.md) when written; renumbered to avoid a collision
with the ontology ADR [ADR-0228](./0228-ontology-list-property-types-closed.md) that landed on main first.)

LABS-234 (agent-CLI audit, `docs/agent-cli-audit.md` finding 7; checklist
8.1). A runaway statement had no bound short of killing the process, and
`--timeout` was an unknown flag. The engine already ships the enforcement
primitive: [ADR-0111](./0111-engine-bounded-execution.md)'s cooperative `CancelToken`, checked at row-production
points and inside the executor's long loops, surfaced as
`Connection::query_with_params_cancellable`. What was missing was the CLI
knob and a documented exit code. [ADR-0194](./0194-cli-error-envelope-and-exit-codes.md) fixed the query-run exit-code map
with code 7 reserved (`rate_limited`); the shared cheatsheet assigns 7 to
timeout.

## Decision

1. **`--timeout DURATION` bounds each statement's wall-clock time.** The
   grammar is a bare number of seconds or a number with an `ms`/`s`/`m`
   suffix; fractions are allowed (`1.5s`). `0` (any unit) disables the
   bound, matching `--max-rows 0`. The flag parses in every bundle and is a
   query-run flag only — lifecycle commands never run it. It applies to
   every statement of a session (one-shot, piped stdin, and each
   interactive line), not to the whole run.
2. **Enforcement is the engine's cancellation, not a client-side guess.**
   The CLI spawns a watchdog thread per timed statement that cancels an
 [ADR-0111](./0111-engine-bounded-execution.md) `CancelToken` after the duration; the query runs through
   `query_with_params_cancellable` and aborts at the next cancel point. The
   watchdog retires the instant the statement finishes, so it never
   outlives what it watches. Cancellation is cooperative: a pathological
   plan that never reaches a cancel point can overshoot, as with any
   cooperative scheme.
3. **A timeout exits 7** and reports `Error: query timed out after
   <duration>` on stderr. In the JSON-family modes (`json`, `jsonl`,
   `envelope`) the stdout error envelope carries `code: "timeout"`,
   `exit_code: 7`, `kind: "execution"`, `retryable: false` — the same shape
 as every other [ADR-0194](./0194-cli-error-envelope-and-exit-codes.md) failure. This fills reserved row 7 of the
 [ADR-0194](./0194-cli-error-envelope-and-exit-codes.md) table as `timeout` (superseding its `rate_limited`
   placeholder).
4. **Docs, help text, and tests land in the same change.**

## Alternatives considered

- **Client-side only: kill the worker thread / abandon the connection** —
  rejected: the engine is in-memory and a detached worker keeps burning
 CPU with no cancel points; [ADR-0111](./0111-engine-bounded-execution.md) exists precisely so a caller can stop
  a query cleanly, and the connection stays usable after a cancellation.
- **A wall-clock cap on the whole process (`timeout` command style)** —
  rejected: a session runs many statements; bounding the run conflates a
  slow statement with a long session and gives the REPL no usable meaning.
- **New exit code (e.g. 9) to leave 7 reserved** — rejected: the shared
  exit-code table is the contract agents code against; the cheatsheet
 already publishes 7 for timeout, and [ADR-0194](./0194-cli-error-envelope-and-exit-codes.md)'s reservation was a
  placeholder, not a commitment.
- **Engine-enforced deadline parameter on the query API** — rejected for
  now: the cooperative token plus a CLI watchdog delivers the behavior with
  no engine change. If another surface needs deadlines, a
  `query_with_deadline` convenience can wrap the same token.

## Consequences

- `cypherlite query --timeout 10s '…'` is bounded per statement; agents can
  rely on exit 7 plus `code: "timeout"`.
- Exit-code row 7 now reads `timeout`; [ADR-0194](./0194-cli-error-envelope-and-exit-codes.md) carries a *Later
  references* note to this ADR.
- Every timed statement costs one watchdog thread; it exits with the
  statement, so there is no accumulation.
- A cancelled write inside an explicit transaction fails that transaction
  (the engine's existing `fail_txn` path), so no partial write becomes
  durable.
