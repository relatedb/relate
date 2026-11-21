---
type: adr
area: engine
claim: 8111b09a42a4aa5843be93bf8e7d3497dd1c31c0
date: 2026-11-21
deciders: Relate maintainers
name: 0653-executor-dispatch-resolve-once-and-row-invariant-hoist
status: proposed
tags:
  - executor
  - perf
title: Executor function-dispatch resolve-once and row-invariant sub-expression hoist
REFERENCES:
  - "[[adr/0245-query-latency-baseline-environment-and-gate]]"
  - "[[adr/0463-expression-allocation-budget]]"
  - "[[adr/0555-operators-stream-rows-never-accumulate]]"
  - "[[adr/0640-executor-expression-memo-tiers]]"
---

# 0653: Executor function-dispatch resolve-once and row-invariant sub-expression hoist

## Context

[ADR-0640](./0640-executor-expression-memo-tiers.md) memoises pure checks
keyed by their input and defers two cheaper optimisations that remove work
paid per row, which no memo can.

**Dispatch is paid per row.** Every evaluation of a call node lower-cases
the name, binary-searches the builtin table and checks the arity contract
before evaluating arguments; all three are functions of the plan alone.

**Row-invariant sub-expressions are re-evaluated per row.** A sub-tree with
no variable reference (a literal, a parameter, a call over them such as
`date('2024-05-05').year`) yields the same value on every row of a
statement unless it samples something per call. The evaluator has no notion
of this and walks the tree for each row; the predicates needed to decide it
(free variables, the unstable-node predicate, the registry's determinism
flag) exist for narrower purposes.

### What the clock functions actually do

Every statement entry installs one clock pin for the statement. Verified in
the executor and core clock code:

- **Statement-pinned** (every evaluation in a statement returns the same
  instant): `timestamp()`; the no-argument constructors `date()`,
  `datetime()`, `time()`, `localtime()`, `localdatetime()` (and their
  `zoned_*` and `local_*` spellings); the `<kind>.statement(...)` and
  `<kind>.transaction(...)` accessors; and the value-less
  `<kind>.truncate(unit)` forms, which truncate the pinned instant.
- **Sampled per call** (the live wall clock at each evaluation): the
  `<kind>.realtime(...)` accessors, `rand()` and `randomUUID()`.

Today the registry flags the pinned functions nondeterministic anyway: a
conservative answer for its existing consumers, wrong as the hoist's gate.

## Decision

### A. Resolve a function call once per plan

1. A call's resolution (lower-cased name, builtin row, arity verdict) is
   computed **once per lowered plan expression**, stored with the plan node
   and produced by the resolver the dispatcher already shares; the per-row
   path reads the slot.
2. **Errors keep their timing and text.** Unknown-function, arity and
   aggregate-outside-aggregation rejections are raised today at first
   evaluation, and never if no row reaches the call. The slot stores the
   rejection and raises it then, identical in kind and message.
3. Argument evaluation order and the exactly-once argument contract are
   unchanged; the registry stays the single source.

### B. Hoist row-invariant sub-expressions

1. A sub-expression is **row-invariant** when it references no row variable,
   contains no per-call-sampling call and no scope-bearing form, and reads
   no graph state that a write clause of the same statement can change (an
   own-write view disqualifies, as in ADR-0640). Parameters are fixed for the
   statement and qualify.
2. **Why only row-variable-free sub-trees qualify.** A value that depends on a
   row variable differs per row, so serving one cached value to every row
   would be wrong. A literal- or parameter-only sub-tree has one input for the
   whole statement and qualifies; row-dependent sub-trees stay with the
   keyed memo tiers of ADR-0640, keyed by the row-dependent input.
3. **Clock and entropy classification.** The gate is a new derived
   predicate, "samples per call", true exactly for the `.realtime`
   accessors, `rand()` and `randomUUID()`, derived from the registry row and
   its clock mode, never a second list. Statement-pinned functions (above)
   are hoistable, `timestamp()` and no-argument `datetime()` included, and
   return the same instant hoisted or not. The existing nondeterminism
   predicate and its consumers are unchanged by this decision.
4. **Lazy.** A hoisted sub-expression is evaluated at first use, at most once
   per statement, and its result served to every later use. Eager evaluation
   would fail a statement whose rows never reach the expression. A failure
   is the answer for every row, so the statement ends at it and no error
   cache is needed.
5. **Always on.** No diagnostic switch: the hoist changes no result, the
   memo tiers are always on by the same argument, and a switch would add a
   branch to the path being shortened.
6. **Placement.** Decided at plan lowering (maximal invariant sub-trees
   only, containing at least one call or operator) and realised as a
   statement-scoped once-cell on the node.
7. **Streaming and memory.** A hoisted cell holds one value, charged to the
   retained-container meter ([ADR-0463](./0463-expression-allocation-budget.md))
   when it is a container. Nothing grows with row count
   ([ADR-0555](./0555-operators-stream-rows-never-accumulate.md)).
8. **Interaction with memo tiers.** The hoist subsumes ADR-0640's no-key tier
   for the general evaluator and is applied first; a row-dependent remainder
   is memoised as ADR-0640 specifies. The kernel's prepared slot stays.

## Alternatives considered

- **Resolve at parse time, or hoist eagerly at plan time.** Rejected: moves
  errors earlier and fails statements whose rows never reach the call.
- **Fold constants in the planner IR.** Rejected: the planner would have to
  evaluate builtins and could not see parameters.
- **A process-wide dispatch cache keyed by call text.** Rejected: a hash
  probe per row is the cost being removed; likewise a memo, which still
  probes per row.
- **Keep every clock function unhoisted.** Rejected: the pinned functions
  return one value per statement; excluding them has no semantic reason.
- **A diagnostic switch; eager evaluation.** Rejected: see B5 and B4.

## Consequences

- Dispatch costs one slot read; an invariant sub-tree one cell read per row.
- A new builtin declares its sampling on its registry row, test-covered.
- The plan node gains a small slot; the cell must be safe where plans are
  shared across threads.

## Acceptance

Measured with the query-latency profiler (small and medium shapes, repeated
rounds, A/B against the commit before):

- The loop-invariant filter, projection and ordering-key groups and the
  invariant clock-constructor groups improve; the improvement threshold is
  not fixed here and is set from the first measurement and recorded in the
  change.
- The row-dependent clock group and the memo-owned groups do not regress
  beyond noise; the 30-query SF0.1 suite stays inside the
  [ADR-0245](./0245-query-latency-baseline-environment-and-gate.md) bands.
- **Memory impact is measured and recorded** on a memo- and hoist-heavy
  statement set: peak RSS, allocation bytes, and retained memo bytes,
  before and after, in the same environment. A regression beyond the
  ADR-0245 memory band blocks landing.
- Correctness: a failing call on an empty input still succeeds; first-failure
  error text is byte-identical; `rand()`, `randomUUID()` and `.realtime`
  accessors still vary across rows; `timestamp()` and no-argument
  `datetime()` return one value per statement; an own-write read is not
  hoisted; a parameter-only sub-tree is evaluated once (test-only counter).
