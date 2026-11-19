---
type: adr
area: infra
date: 2026-10-11
deciders: CypherLite maintainers
name: 0204-nightly-pull-retry-and-diagnostics
status: proposed
tags:
  - ci
  - conformance
title: "Nightly differential: bounded pull retry and pre-cleanup reference diagnostics"
REFERENCES:
  - "[[adr/0144-graph-examples-differential-suite]]"
  - "[[adr/0176-graph-examples-report-only-nightly]]"
---

# 0204. Nightly differential: bounded pull retry and pre-cleanup reference diagnostics

- **Related**: [ADR-0144](./0144-graph-examples-differential-suite.md) (graph-examples differential suite), [ADR-0176](./0176-graph-examples-report-only-nightly.md)
  (report-only nightly and infrastructure-failure kinds)

## Context

The 2026-10-11 nightly differential (run 38108996036) failed on
`fraud-detection` with:

```
cypherlite pull fraud-detection failed (exit status: 1):
Error: reading response body from http://localhost:32768/db/fraud-detection/query/v2
failed mid-statement: Peer disconnected; the server closed the response stream
```

Fifteen of sixteen datasets pulled fine on the same container (heap
already 4G), so the disconnect was transient, not a resource ceiling. The
failure surfaced two robustness gaps:

1. **No retry.** `pull_fresh_copy` runs `cypherlite pull` exactly once.
   A transient mid-statement disconnect — Neo4j's Query API v2 closing a
   response whose client stalls, per the error's own hint — fails the
 whole dataset as an [ADR-0176](./0176-graph-examples-report-only-nightly.md) infrastructure failure even though a
   fresh re-run would very likely succeed.
2. **No reference logs on a graceful-but-failing exit.** The suite's
   `DatasetTest`/`CleanupGuard` `Drop`s remove the reference container
   when the infrastructure-failure panic unwinds, so the nightly
   workflow's `Capture reference diagnostics` step — which runs after the
   test step — finds no container and ships nothing to diagnose with.
   (It only finds a container when the run was killed mid-flight:
   SIGKILL, runner timeout.)

## Decision

1. **One bounded retry of the whole pull, on the transient
   stream-disconnect signature only.** In
   `testing/cypherlite-graph-examples/src/suite.rs`, `pull_fresh_copy`
   routes the attempt through a `with_one_transient_retry` helper. The
   transient predicate (`is_transient_failure` in
   `testing/cypherlite-graph-examples/src/pull.rs`, next to `pull_once`)
   anchors on the single stable substring of cypherlite-load's
   mid-statement read error, `failed mid-statement` (see
   `mid_statement_read_error` in
   `surfaces/cypherlite-load/src/response.rs`); the io-error tail
   (`Peer disconnected`, `Connection reset by peer`, …) varies by
   platform and is not matched on its own. The retry re-runs the
   destination reset (remove + recreate) plus the pull from scratch, so
   it never merges into a half-pulled graph. Auth errors, syntax errors,
   and the pull timebox kill never carry that substring and are never
   retried. A retry that also fails returns the second error marked
   `failed again after one retry`, and the retry is logged to stderr for
   the job log.
2. **The harness captures reference container logs into the report dir
   before cleanup.** A new `docker::capture_reference_logs(report_dir)`
   writes `docker logs` for every container matching the
   `cypherlite-graph-examples-reference` name prefix to
   `<report_dir>/reference-<id>.log`, best effort. The infrastructure
   failure paths in `tests/differential.rs` (the `verdict()` panic and
   the boot panic) call it before panicking. The files land next to
   `report.txt`, inside the workflow's existing
   `reference-*.log` upload glob — no workflow change. No credential
   stripping is needed: `docker logs` carries only the process's
   stdout/stderr, never the container's `Config.Env`.
3. **No memory bump.** Evidence first: the same 4G container served the
   other fifteen pulls in the failing run, so there is no signal that
   heap was the cause. The diagnostics capture from (2) is exactly what
   provides evidence if this recurs.

## Alternatives considered

- **Retry all pull failures.** Rejected: auth and syntax errors are
  deterministic; retrying them doubles the failure latency for zero
  chance of a different outcome, and a timebox-kill retry would double a
  genuinely too-slow pull's cost.
- **Bump the reference container's heap.** Rejected: no evidence memory
  was involved — the same container served fifteen other pulls in the
  same run — and an unearned bump hides the real cause the next time.
- **Heartbeat / keep-alive reads in the load surface.** Rejected for now:
  a real product change to `cypherlite-load`'s streaming reader is a much
  larger surface than the nightly needs, and the bounded retry covers the
  observed transient class. If the diagnostics show stalls are frequent,
  revisit as a cypherlite-load change with its own ADR.
- **Workflow-side capture only.** Rejected: that step already exists and
  is what failed to produce diagnostics — by the time it runs, the
  suite's `Drop` cleanup has removed the container. Capture must happen
  inside the harness, before the panic unwinds.

## Consequences

- A transient stream disconnect costs one extra pull of one dataset
 instead of failing the whole dataset (and the run) as an [ADR-0176](./0176-graph-examples-report-only-nightly.md)
  infrastructure failure; permanent failures are untouched.
- Every future infrastructure-failure nightly ships the reference
  container's logs in the uploaded artifact, whether the run panicked
  gracefully or was killed.
- `cypherlite-load` is unchanged; if retries ever flap regularly, the
  captured logs are the evidence for a load-surface fix (see the
  heartbeat alternative above).
