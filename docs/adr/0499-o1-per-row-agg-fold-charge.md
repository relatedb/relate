---
type: adr
area: engine
claim: c8d7838c03d1672496ab8097a4c0562a189b64ed
name: 0499-o1-per-row-agg-fold-charge
status: proposed
tags:
  - executor
  - perf
title: Flatten the aggregate fold's per-row charge chain, keep the walk exact
REFERENCES:
  - "[[adr/0463-expression-allocation-budget]]"
---

# ADR-0499: Flatten the aggregate fold's per-row charge chain, keep the walk exact

## Context

[ADR-0463](./0463-expression-allocation-budget.md) charges every aggregate call's per-group `vals` vector through one
retaining guard: the slot buffer up front (`retain_slots`), then each row's
operand value through `RetainedTotal::add_owned` — an `owned_heap` walk of
the value's heap — for **every** aggregate kind. LABS-789's perf-lab round
(a development worktree, in-process engine-only) measured
that per-row charge at **46.5% of a plain `count`**; re-measured on this
tree with the in-process sampler over `UNWIND range(1, 500000) AS i RETURN
count(toString(i) + '…')`, `add_owned`'s self-time is **53.8%** of the
query.

The cost was the walk's **call chain**, not its semantics. A shared payload
(a variable-read operand — the common `count(n)`, `count(p)` shape) is one
`Arc` count read away from a zero charge, but that one read cost a
non-inlined call graph: `owned_heap` recursing through `unique_list` and
the entity walks, plus the cross-crate `str_logical_bytes` — several
hundred nanoseconds per row of call overhead for an O(1) answer. A flat
payload (the repro's constructed string) paid the same chain for a length
read.

Meanwhile the charge itself is load-bearing for every fold, not just
`collect`: a transient fold's `vals` physically holds every operand payload
until the fold completes, and the exact walk is the only cumulative bound
on rows × payload. The security review of this change's first form (a
fixed-width per-row figure with no descent into elements) demonstrated the
class it would open: a constructed flat payload nested one level deep —
`count([substring($big, 0, 250_000_000)])`, each element under the
per-allocation cap — escapes any fixed-depth estimate, because a container
contributes only its own buffer while its element's per-allocation check is
stateless. Nesting depth is attacker-chosen query text; every fixed depth
loses.

## Decision

1. **The exact `owned_heap` charge stays for every fold.** `collect`'s
   result retains the vals; every other fold's vals hold the payloads until
   the fold consumes them — both need the same cumulative bound, and the
   exact walk is the only sound one (the nested-constructed shape above
 refuses at the second row through it). [ADR-0463](./0463-expression-allocation-budget.md)'s refusal semantics are
   unchanged everywhere.
2. **The per-row overhead is removed by flattening the chain, not by
   weakening the charge.** `owned_heap` and its helpers (`unique_list`,
   `map_value_heap`, `property_heap`, `node_heap`, `rel_heap`) are
   `#[inline]`, and the core leaf formulas `str_logical_bytes` /
   `bytes_logical_bytes` are `#[inline]` so they inline across the crate
   boundary. The common arms dissolve into the charging loop: a shared
   payload is one `Arc` count read, a flat payload inline arithmetic, and
   the deep walk is paid only by a row whose own operand construction was
   that deep — proportional to work the expression layer just did in the
   same iteration, at the construction site, never re-walked for values the
   row merely moved.
3. **The other three retaining containers are untouched.** Comprehension
   outputs and a projection's output rows keep their exact per-element
   `add_owned` totals; UNWIND keeps its fixed-width row figure.

## Alternatives considered

- **A fixed-width per-value figure for the transient folds** (the first
  form of this change: `retains_input_values` gating `collect` to the exact
  walk, everything else charging a no-descent `owned_heap_shallow`).
  Rejected — the security review's finding: a constructed flat payload
  nested one level in a container escapes it, and every fixed nesting depth
  loses to attacker-chosen query text; the exact walk is the only sound
  cumulative bound.
- **Slot figure only for the transient folds.** Rejected — same class,
  worse: the rows × payload product is bounded by nothing at all.
- **Sampling the walk** (every Nth row, scaled). Rejected — a sampled
  estimate under-charges whatever the unsampled rows carried; the
  guardrail's refusals would turn probabilistic where they were exact.
- **Streaming the scalar folds** so the vals never materialize. Rejected
  for this change — that is LABS-685's chunk-native fold redesign; this ADR
  fixes the charge's cost, not the materialization.
- **Charging construction sites into the enclosing container's total** (a
  current-container slot in the charge seam). Rejected — it counts values
  the loop built and dropped (`count(size(n.friends))` would charge the
  list the fold never kept), reintroducing the cumulative-meter false
 positive [ADR-0463](./0463-expression-allocation-budget.md) was revised to remove.

## Consequences

- The measured per-row overhead is gone: with the chain flattened, the
  repro query runs at parity with a no-charge build (measured on this
  tree: 0.248 s → 0.223 s p50 over 500k rows; the `add_owned` frame leaves
  the sampler's profile entirely), and every shape with shared operands —
  the plain `count(n)` / `count(p)` family — keeps its one-read zero
  charge.
- Every [ADR-0463](./0463-expression-allocation-budget.md) refusal stands exactly as before, and the charge-test
  family pins the two directions the first form put at risk: a shared
  operand charges nothing (the O(1) common case), and a constructed
  payload — flat or nested one level — refuses through the exact running
  total, for the transient folds as for `collect`.
- The deep walk's cost is now proportional to the charging row's own
  construction work only; moved values never re-walk. A fold over
  per-row-constructed deep containers still pays a walk proportional to
  what it just built — the price of the sound bound.
- PROFILE's `Expression Built (Bytes)` telemetry is unchanged:
  `charge_retained` never recorded, and the slot-buffer charge
  (`retain_slots` → `charge_expression`) still does.
- The aggregate charge tests live in their own module
  (`exec/aggregate_charge_tests.rs`) beside the fold-semantics tests, with
  the shared row fixtures in `exec/test_support.rs` beside `agg_fn`.
