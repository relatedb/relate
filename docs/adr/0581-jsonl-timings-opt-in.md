# ADR-0581: The jsonl terminal timing line is a `--timings` opt-in

Status: Proposed
Claim: 2be0785c7b0e1c2c1181658a91037f564176b3ad

## Context

ADR-0540 decision 6 put the statement's timing block on every CLI machine
mode. In `--mode jsonl` it is one keyed terminal line closing every
statement's stream — including a zero-row statement, whose whole output is
that line. But jsonl is the piped default (ADR-0107) and the mode
line-based consumers script against: for them the line is one non-row line
to skip per statement, and a zero-row statement printing a line at all
breaks the one-line-per-row contract the mode is named for. The row cap's
terminal record (ADR-0403) is the counterexample that stays: it appears
only when a cut happened, to keep the cut from reading as complete. The
timing line appears on every statement unconditionally, carrying a signal
most jsonl consumers never asked the stream for.

## Decision

The jsonl terminal timing line is opt-in through a `--timings` boolean
flag, default off. Off, a jsonl stream is pure rows — plus the plan and
rowCap terminal lines when those facts exist (ADR-0403): a zero-row
statement prints an empty stdout; an N-row statement exactly N lines. On,
the stream closes with the keyed timing line as ADR-0540 decision 6 shaped
it, after any plan and rowCap lines; a zero-row statement emits exactly
that line and nothing else.

Everything else is unchanged. The `--json` and `--mode envelope` timing
riders and the `--toon` terminal entries stay always-on; the rowCap and
plan terminal lines stay always-on; and the engine's always-on measurement
(ADR-0540 decision 2) is untouched — the flag gates one rendering, never
the clocks. The flag is double-dash only, boolean, default off, and owned
by the formatter as one run-level setting, the same shape as the
pretty-print switch: the library default stays on — a stamp passed through
the render call renders — while the CLI writes the flag's value once per
run, so no per-statement path carries the policy.

## Alternatives considered

- **Keep the line always-on** (status quo) — rejected: every line-based
  jsonl consumer skips one non-row line per statement, and a zero-row
  statement prints a line at all; the mode's own name promises rows.
- **Drop the line from jsonl entirely** — rejected: the in-band "where did
  the time go" answer was ADR-0540's point; consumers that want it would
  have no way to ask for it back.
- **A `--no-timings` opt-out** — rejected: the default is what every
  scripted consumer lands on, so purity would require every script to pass
  a flag; the opt-in direction matches how the row cap's `--max-rows 0`
  escape hatch relates to its own default.
- **Gating on agent-mode detection** — rejected: the stream's shape would
  depend on the environment a process runs in — the same script piped
  under a harness would emit different lines than in a developer shell,
  and an explicit `--mode jsonl` would still need its own rule.
- **Riding the timing block on the last row line** — rejected: keyed
  terminal lines are the established carrier (plan, rowCap) precisely
  because a bare object in the row stream is indistinguishable from a
  data row that happens to carry those columns.

## Consequences

jsonl consumers that filtered the keyed terminal line see pure rows; the
typed filter helpers keep working when the flag is on. A zero-row
statement under jsonl prints nothing, so "no rows" and "no output" are the
same there — the envelope mode remains the shape that distinguishes them.
`--timings` is documented in `--help`, the CLI spec's flag table and the
flags reference, and the agent-safety skill note names it.
