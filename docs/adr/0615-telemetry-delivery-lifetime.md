# 0615. Telemetry delivery survives short runs: arming start, joined exits, once-per-version drain

Status: Proposed
Claim: 2d56e270df9ea9b53733760de69cb2c16e7fdd3d

## Context

ADR-0545 moved telemetry delivery off the exit path: a run spools its
events locally, and a detached drain thread — started at the run's first
recorded event — claims earlier runs' spool files and POSTs them while
the run works. Exit never waits on the network.

Diagnosis of under-reported usage found that this shape never delivers
for most runs. `process::exit` kills the detached thread mid-POST, and a
cold POST to the sink measures ~250 ms, so a sub-second one-shot query
run — the dominant invocation shape — delivers nothing. Lifecycle
commands never deliver either, however long they run: their only event
is emitted after the command body, so the drain starts microseconds
before exit; a self-upgrade or a serve lifetime is a dead delivery
window. Undelivered claim files sorted ahead of spool files, so they
consumed every sweep slot and starved newer events, and past the spool's
file bound the retention cut shed the overflow at random. Machines
accumulated days of pending events with no confirmed delivery while the
sink accepted the same envelopes on first contact.

## Decision

Delivery keeps ADR-0545's durable layer — local spool, at-least-once,
dedup by `$insert_id`, the `telemetry off` transition untouched — with
four changes. The spool file grammar and envelope are unchanged, so a
new binary claims an old binary's files and an upgrade needs no
migration.

1. The drain starts when the pipeline arms it — at the first recorded
   event, or when a non-query exit (version, help, a lifecycle command)
   opens the delivery window before its work begins — and only when the
   spool holds files to deliver: a clean machine spawns no thread, so a
   long-lived `serve` never holds the HTTP and TLS machinery of a
   delivery it has no backlog for. A command's own runtime becomes the
   delivery window; the off transition never opens it (ADR-0545 §5).
2. Non-query exits join the drain with a small budget (1 s): the thread
   had the whole run as a head start, so the join pays the POST tail,
   not the POST; what exceeds the budget rides the next run.
   Query-session exits keep the pure local write of ADR-0545 §1 — a
   scripted one-shot never waits on the network, and its own events ride
   the next run like every run's do.
3. The first run of a new CLI version joins with an elevated,
   pending-scaled budget (1 s plus 150 ms per pending file, at most
   10 s) whatever its shape — the once-per-version drain clearing a
   machine's accumulated backlog after an upgrade. The run records its
   version in the state file whether or not delivery succeeded
   (attempt-once), so a machine that cannot deliver yet pays the
   elevated budget exactly once.
4. A join that fails or runs out stamps a 15-minute backoff window in
   the state file; exits inside the window skip the wait — a machine
   with a pending backlog and an unreachable sink must not stall every
   exit, the pre-ADR-0545 failure returning for the join.

A batch the sink refuses outright (its 4xx contract: resending
unchanged cannot help) drops its claim attempt-once, like the spool's
undeliverable content — retained, it would consume a sweep slot forever;
retryable answers (server failure, unreachable, timeout) keep the claim
for the next run.

The sweep claims oldest files first (content mtime, which the claim
rename preserves), and past the file bound the retention cut keeps the
oldest files and drops the newest overflow: the oldest pending events
are a machine's earliest undeliverable history — nothing can re-record
them, and they carry the first-run events install counting depends on —
while the newest overflow is statistically redundant with the runs that
follow it. Age ordering also dissolves the claim wall: undeliverable
claims no longer sort ahead of spool files.

## Alternatives considered

- A detached delivery child process outliving the CLI exit — rejected:
  no fork on Windows, unsigned-binary self-spawn trips antivirus
  heuristics, kill-on-close job objects and the running-image replace
  window complicate it, and a default-on telemetry feature keeping a
  process alive after the user's command exits is poor optics.
- The exit join on every run, one-shot queries included — rejected: a
  steady-state machine almost always has the previous run's file
  pending, so the highest-frequency shape would pay the POST tail on
  every invocation; the once-per-version drain covers the residual
  instead, once per version.
- Keep next-run-only delivery and raise the sweep caps — rejected: the
  next run must itself outlive its POSTs, which sub-second runs never
  do; caps do not change that.
- A synchronous network flush at exit (the pre-ADR-0545 shape) —
  rejected in ADR-0545 §1 with measurements (~280 ms on every short
  run); the revision keeps query exits at that floor and bounds the
  join to the non-query paths.

## Consequences

- Every enabled run except a sub-second query session delivers the
  backlog it finds or hands it to the next run; a queries-only machine
  delivers once per version, making its usage stream a per-release
  sample — a documented bias, against the previous silent zero.
- A run's own events still ride the next run (one-run lag, unchanged),
  and a machine's final spooled run still needs one successor.
- `state.json` gains the last-run CLI version and the backoff window's
  end; schema-additive, older binaries ignore the keys.
- The usage-metrics spec's delivery paragraph — the privacy disclosure —
  changes with the code, as does ADR-0545's later-references note.
- Past the file bound the spool now sheds its newest events, not a
  random selection.
