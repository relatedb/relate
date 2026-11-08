# 0545. Telemetry delivery off the exit path: local spool, next-run drain

- **Status**: Proposed
Claim: 6c5984093ab0d983ef961918dd5fc35a78024136
- **Date**: 2026-11-08
- **Deciders**: Relate maintainers

## Context

ADR-0079 §5 promised a bounded exit flush "so the CLI is never
perceptibly slowed". The first implementation joined the worker thread,
so every exit paid the worker's HTTP timeout window; a follow-up bound
replaced the join with a 250 ms done-channel wait. Measured on a release
build against the production endpoint, both miss the contract:

- A short `query` run costs ~0.28 s with telemetry on, ~0.01 s opted
  out — the budget is consumed on nearly every short run, because one
  POST costs ~220–260 ms (TLS ~70 ms + ~150–190 ms server time) and a
  query run issues two sequential POSTs (~400 ms): STARTUP is emitted
  before the statement, QUERY after, and the worker POSTs each batch it
  wakes with.
- When the budget expires the worker is killed at `process::exit`
  mid-POST: the QUERY event is dropped nearly always, STARTUP is a
  race. The wait is paid and the delivery is lost.

No in-process design can do better: the network round trip is the floor
any exit wait pays, and `process::exit` kills in-flight work, so an
async-on-exit trigger delivers nothing on cold short runs — the worker
is ~5 ms into a ≥70 ms TLS handshake when the process dies. The config
directory also never resolves on Windows (resolution reads only
`$XDG_CONFIG_HOME`, then `$HOME`; stock Windows sets neither), so
telemetry there runs stateless — a fresh per-process install ID, a
first-run notice that never persists — and a spool would have no home.

## Decision

Exits write locally; the next enabled run delivers.

1. **Exit = one local append.** `flush` writes the run's pending
   envelopes to the run's own spool file — one JSONL file per process
   under the config directory, one local write, no thread handoff, no
   network. The spool is bounded and drops past the bound, mirroring the
   in-memory overflow semantics; no fsync at exit, telemetry stays
   best-effort.
2. **Next-run drain.** An enabled run spawns one detached drain thread
   at its first recorded event — a run that records nothing (the `off`
   command above all) never starts a delivery — which claims spool
   files, renaming each to a unique claim
   name, sweeping claim files left by crashed or concurrent drains too,
   and POSTs them in bounded batches while the run's own work proceeds,
   taking at most a bounded set of files per sweep and dropping the
   excess past a bounded set of pending files. The sweep takes
   regular files only: a planted symbolic link is never claimed, read,
   or followed — including one swapped in after the sweep's filter, by
   a post-rename re-check. A claim is deleted only after a confirmed
   response; undelivered claims stay for the next sweep, and a claim
   whose content cannot be read is attempt-once — removed on the spot,
   like a corrupt line, so undeliverable content never starves a
   sweep. Delivery is
   at-least-once, effectively-once via the per-event `$insert_id`
   (ADR-0079 §2) the sink already dedups on.
3. **No in-run delivery.** The bounded channel and its worker thread
   disappear; a run's events sit in a bounded in-memory backlog and
   spool at exit. The 2 s HTTP timeout governs the drain POSTs. A
   session's events report one run late — immaterial for aggregate
   metrics, since event `time` is stamped at emit.
4. **The flush budget and `RELATE_TELEMETRY_FLUSH_MS` are removed** —
   nothing on the exit path needs a network bound.
5. **Opt-out.** Disabled runs spool and drain nothing. `telemetry off`
   is one ordered transition — disarm the run's pipeline, persist the
   key, clear any existing spool — so the run that turns telemetry off
   neither delivers, records, nor leaves a spool file of its own:
   turning telemetry off means no further transmissions, pending ones
   included.
6. **Windows config directory.** Resolution gains `%APPDATA%\relate`,
   then `%USERPROFILE%\.config\relate`, after the existing XDG/HOME
   order. Strictly additive — `None` becomes a path on Windows — giving
   Windows a persistent install ID, a working first-run notice, and
   extension-registry discovery (ADR-0429 §6).
7. **Portability rules.** Spool mechanics use std-only primitives:
   claims by exclusive create and atomic rename, short-lived opens only
   (Windows denies rename of files another process holds open), no
   advisory file locks, no platform-conditional code.
8. **Docs and guardrail, same change.** The CLI spec's usage-metrics
   section and privacy listing name the spool and its bounds; the env
   table loses the removed knob. A cold-start-to-first-query benchmark
   lands with the change and runs per-OS.

## Alternatives considered

- **Keep the bounded exit grace** — rejected: measured worst of both
  worlds; the wait is paid almost always, the second event still drops.
- **Keep the in-run POST worker, spooling before each POST** — rejected:
  an exit must either wait out the worker's in-flight POST (the delay,
  again) or race it and duplicate the delivery bookkeeping; drain-only
  is simpler and measures the same usage.
- **Fire-and-forget (zero budget, detached worker)** — rejected: the
  process kills the worker mid-handshake on every cold short run;
  short-run telemetry silently stops existing.
- **Runtime threshold (deliver only long runs)** — rejected: honest but
  blind; short runs are the majority class the metrics exist to count.
- **Detached helper child finishing the POST after exit** — rejected:
  no `fork` on Windows, container PID 1 exit kills the child, and a
  hidden flag surface fights the CLI's documentation rules.
- **Advisory file locks for single-drainer claims** — rejected: not in
  std, divergent on Windows; at-least-once plus `$insert_id` dedup is
  simpler and portable.

## Consequences

- Short-run exit cost approaches the opted-out floor plus a sub-ms
  append; acceptance is ≤10 ms over the floor, measured per-OS.
- Delivery becomes eventually-once: the next enabled run delivers, and
  a final tail (the last run on a retired machine) is lost — acceptable
  for aggregate metrics, and event `time` is stamped at emit, so late
  delivery does not skew time-based reports.
- A new bounded on-disk artifact appears under the config directory,
  mode 0600 like the telemetry state, disclosed in the privacy listing
  and cleared by `telemetry off`.
- Windows changes beyond telemetry: install IDs persist and the
  extension registry finds its directory — strictly additive, recorded
  here because the spool requires it.
- `RELATE_TELEMETRY_FLUSH_MS` disappears with no migration hint, per
  the repository's precedent for removed spellings.
