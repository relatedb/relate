---
type: adr
area: engine
claim: f9e6f0db4609ba4845bfa29f42a5b865af3bd432
date: 2026-11-22
deciders: Relate maintainers
name: 0657-executor-memo-budget-open-setting
status: accepted
tags:
  - executor
  - memory
  - perf
title: One open-time setting for the executor memo budget
REFERENCES:
  - "[[adr/0463-expression-allocation-budget]]"
  - "[[adr/0640-executor-expression-memo-tiers]]"
---

# 0657: One open-time setting for the executor memo budget

The maintainers ruled on the four questions raised in the proposal; the
rulings are folded into the decision below.

## Context

[ADR-0640](./0640-executor-expression-memo-tiers.md) bounds the memo tiers
with two fixed numbers: one 16 MiB live budget shared by every memo of a
statement, and a per-table ceiling of 8 MiB in each memo's policy. Both are
compiled in. The other executor limits are open-time settings, resolved
once at open from an `OpenOptions` field, then an environment variable,
then a default, with a CLI flag for the query session and `serve`, and a bad
value failing the open ([ADR-0463](./0463-expression-allocation-budget.md)
is the closest sibling). The memo ceilings are the odd ones out.

The constants matter to two users. A host with a tight memory ceiling wants
to give the memo less, or none. A maintainer measuring the memo wants a
control run with it off, and a way to vary the size, without a rebuild.
Measured so far: the distinct-datetime group holds about 8.6 MB live at the
8 MiB table ceiling and runs about 20% faster than its control; the
chained-`WITH` group holds about 12 MB across three tables.

## Decision

1. **One setting, the shared memo budget, in whole megabytes.** An
   open-time setting carries the statement's shared live budget as a whole
   number of MiB (`0`, `1`, `8`, `32`, …); no byte values and no unit
   suffixes. Default 16, today's value, to be confirmed by the sweep below.
   It resolves like the sibling limits: explicit `OpenOptions` value, else
   environment variable, else default.
2. **The per-table ceiling is derived, not a second knob.** It is half the
   budget, today's ratio, so one table cannot starve the others. No
   independent setting exists. The ceiling lives today in both
   `MemoPolicy::MODERATE` and the construct memo, so the derivation touches
   both.
3. **Zero disables the budgeted tables only.** No table is allocated and
   every pure check runs as it would without a memo. The mechanisms that
   cost no budgeted memory (the compiled-regex cache, the `IN`-list
   membership index, the node-label memo's bitmap tier) stay on. There is no
   per-statement kill switch for them: a misbehaving one is fixed and
   rolled forward.
4. **Loud on bad input.** A negative or unparsable value fails the open
   naming the setting, before the store lease. There is no silent fallback to
   the default.
5. **Speed and memory only, never results.** Only purity-gated expressions
   are memoised ([ADR-0640](./0640-executor-expression-memo-tiers.md)), and
   a refusal already falls back to the function, so any budget, including
   zero, returns the same rows. The clock functions follow the same rule:
   the statement-pinned ones may be memoised, while `.realtime`, `rand()`
   and `randomUUID()` are never memoised, so no budget touches them. A differential test across budgets pins it.
6. **The default is validated, not guessed.** The implementation PR carries
   a measured 2, 4, 8 and 16 MiB sweep of the distinct-datetime group
   (time and peak live bytes, against its control) as an acceptance item,
   and the default is the measured knee or stays 16 MiB with the numbers
   recorded.
7. **Implementation is a separate single PR.** It ships the Rust
   `OpenOptions::expression_memo_budget` field and the
   `RELATE_EXPRESSION_MEMO_BUDGET` environment variable, with spec,
   description and a runnable example, and the CLI environment-variable
   table. There is no CLI flag: the setting is advanced, and an undocumented
   flag would break the rule that every flag appears in `--help`. A flag can
   follow if a need appears. The Python and C open options follow in a later
   change.

## Alternatives considered

- **Keep the constants.** Rejected: no way to bound the memo for a small host
  or to run an off control without a rebuild.
- **Separate knobs for the budget and the per-table ceiling.** Rejected: a
  second number to keep consistent with the first, for a ratio nobody has
  asked to change.
- **A per-statement setting.** Rejected for now: memory limits are decided
  once per store, and a per-statement knob adds a surface to every client.
- **Clamp a bad value to the default.** Rejected: a silent fallback hides a
  misconfigured limit.

## Consequences

- Hosts can bound or remove memo memory; benchmarks get a clean control.
- One more open-time setting, with the docs surfaces of the other limits.
- The default stays pinned to a measurement rather than to a constant.
