---
type: adr
area: engine
claim: 529ad013266683d36ed5d1d40c5d85745e33f5d0
date: 2026-11-23
deciders: Relate maintainers
name: 0670-nested-eval-budget-one-million
status: proposed
tags:
  - budget
  - bulk-load
  - executor
title: The nested-evaluation budget is 1 000 000 passes per statement
REFERENCES:
  - "[[adr/0382-local-callables]]"
  - "[[adr/0395-nested-evaluation-budget]]"
  - "[[adr/0669-in-transactions-batch-pass]]"
---

# 0670: The nested-evaluation budget is 1 000 000 passes per statement

## Context

[ADR-0395](./0395-nested-evaluation-budget.md) fixed the per-statement
nested-evaluation budget at 100 000 passes — sized against
exponentially compounding nested bodies, and deliberately without a
knob: the recorded remedy for legitimate work needing more was a new
ADR. That work arrived. A bulk load of a million rows through a
`CALL (row) { … }` body — one body pass per imported row — is a
million *legitimate* passes, linear, not compounding;
[ADR-0669](./0669-in-transactions-batch-pass.md) removes the per-row
invocation for `IN TRANSACTIONS` bodies, but every other nested CALL
shape (and each nested construct inside a batched body) still ticks per
row, and 100 000 refused exactly the bulk-load statements the
WAL-payload remedy names.

## Decision

`NESTED_EVAL_BUDGET` rises from 100 000 to **1 000 000** passes per
top-level statement. It stays a fixed constant with no knob.

[ADR-0395](./0395-nested-evaluation-budget.md)'s original design bound
the runtime pass axis and the compile-side `MAX_CLONED_NODES`
([ADR-0382](./0382-local-callables.md)) at the same number. The raise
splits them deliberately: a pass is bounded clause-engine work a bulk
load legitimately spends a million of, while a cloned plan node is
memory, and the compile axis keeps its 100 000 — the two axes no longer
share a number, each carrying its own cost model.

## Consequences

- Million-row nested-CALL loads run; the bombs that cross the budget
  resize with it (each still aborts at the 1 000 001st pass, before its
  fan completes — the read-form doubling fan now meets the ADR-0463
  expression cap first, its retained row product being the larger guard
  at that scale; the budget's crossing is shown by a flat streaming
  form).
- The security property is unchanged in kind: the budget still bounds
  nested-engine passes per statement by a constant, compounding shapes
  still cross it, and the typed
  `NestedEvaluationBudgetExceeded` error (single-sourced from the
  constant) still names the number.
- The accepted trade, stated plainly: a client that can submit a
  statement can now burn ten times more nested clause-engine work on one
  statement before the guard trips, on every surface alike (Bolt, HTTP,
  embedded). No wall-clock or row companion cap is added — the pass
  budget remains the one per-statement work bound, at its new magnitude,
  by the maintainer's decision (2026-11-23).
