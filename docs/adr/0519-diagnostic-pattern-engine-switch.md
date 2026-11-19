---
type: adr
area: engine
claim: aee82c9112e1597f2af78744f215ba1362f959c7
name: 0519-diagnostic-pattern-engine-switch
status: proposed
tags:
  - pattern-matching
title: The diagnostic pattern-engine switch `RELATE_PATTERN_ENGINE` (auto|nfa|walk)
REFERENCES:
  - "[[adr/0046-cypherlite-parser-memorydb-runtime]]"
  - "[[adr/0277-one-cypher-parser]]"
  - "[[adr/0371-preview-feature-gating]]"
  - "[[adr/0473-contracted-traversal-kernels]]"
  - "[[adr/0478-frontier-nfa-qpp-kernel]]"
  - "[[adr/0516-frontier-rearrival-budget-fallthrough]]"
---

# ADR-0519: The diagnostic pattern-engine switch `RELATE_PATTERN_ENGINE` (auto|nfa|walk)

## Context

The pattern-engine spike (the `perf/pattern-engine-spike` line) needs to
A/B the executor's pattern engines against each other on whole workloads
— the frontier NFA rung ([ADR-0478](./0478-frontier-nfa-qpp-kernel.md)) against the per-row GPM DFS, and both
against the contracted kernels ([ADR-0473](./0473-contracted-traversal-kernels.md)) — from a real process: the CLI,
the network surfaces, the loadtest harnesses. The only existing steering
is `#[cfg(test)]` thread-local hooks (`with_kernels_disabled`,
`with_frontier_disabled`), which a real process cannot reach; a benchmark
binary cannot link them.

The repo has the precedent for exactly this shape of knob:
`RELATE_PARSER` ([ADR-0046](./0046-cypherlite-parser-memorydb-runtime.md), retired by [ADR-0277](./0277-one-cypher-parser.md)) was a diagnostic
engine switch resolved once at `Database::open` — unset keeps the day's
behavior exactly, an unknown value fails the open loudly, and the value
never enters the language. `RELATE_PREVIEW` ([ADR-0371](./0371-preview-feature-gating.md)) carries the
same fail-closed open contract today.

## Decision

Ship `RELATE_PATTERN_ENGINE`, a diagnostic-only engine switch with
three values:

- **`auto`** — the default: unset, empty, and `auto` route exactly as
  today; every rung's own proofs and cost gates decide, unchanged.
- **`nfa`** — force the frontier family's rung onto every
  frontier-compilable clause: `frontier_match::compile_frontier` bypasses
  its cost gate (`frontier_engages`) alone. Every other proof stands —
  the structural and answer-shape gates, the table-allocation ceiling,
 the re-arrival budget's run-time fall-through ([ADR-0516](./0516-frontier-rearrival-budget-fallthrough.md)) — and every
  decline still fails through to the rungs below. The batch
  scan-filter chain and the chain/hub-star kernels stand down for the
  clauses the frontier takes: they sit below it on both operator
  ladders, and the contracted matcher declines quantified shapes
  structurally, so the families are disjoint.
- **`walk`** — stand the traversal kernels down: the chain/hub-star
  kernels and the frontier family decline every clause, and the batch
  scan-filter chain plus the per-row walker serve — the differential
  suites' `with_kernels_disabled` behavior, now reachable from a real
  process.

Resolution follows the `RELATE_PARSER` precedent's site and
precedence: read once at `Database::open`, in the fail-before-lease
preflight (beside `RELATE_PREVIEW`), parsed case-insensitively; a
present value that is not one of the three fails the open with
`invalid RELATE_PATTERN_ENGINE: … expected auto, nfa, or walk` — no
silent fallback. The resolved value is installed on the store-level
`StoreServices` bundle (beside the preview set, through
`Graph::configure_pattern_engine`), so every namespace of the store
routes alike and a follow reload keeps it; the rungs read it through
`Graph::pattern_engine`.

Boundaries, stated once:

- **Diagnostic-only.** The switch is a measurement instrument for the
  spike, not a supported tuning knob: no stability promise, no
  performance claim for either forced value.
- **Never Cypher syntax.** No query-level spelling, no `USING` hint, no
  plan-level surface; a statement cannot select its engine, and PROFILE
  output changes only through the operators' existing annotations.
- **Removal condition.** The switch dies with the old engines it
  switches between: when the spike concludes and the per-row walker or
  the kernels it selects among are replaced or removed, the switch is
  removed in the same change. While it lives, `auto` must remain
  today's routing exactly.

Executor tests pin the routing (`exec/kernels/engine_switch_tests`):
forced-`nfa` engages the frontier on a cost-declined frontier-compilable
shape (engagement and frontier counters), forced-`walk` leaves a
chain-kernel shape and a frontier shape to the per-row walker (zero
engagements, identical rows), and the `auto` twins route as before. The
variable is documented beside `RELATE_PREVIEW` in the Rust and CLI
specifications and in the dialect notes (`AGENTS.md`), the doc set the
`RELATE_PARSER` mentions live in.

## Alternatives considered

- **Cargo features per engine.** A compile-time matrix cannot A/B two
  engines over one workload in one process, and it multiplies the
  feature-bundle lanes every gate already carries. Rejected: the spike
  needs process-level selection, not build-level.
- **A query-level hint (`USING ENGINE …` or a procedure).** Rejected:
  the switch is never Cypher syntax — a diagnostic spelling would leak
  into the language contract, the TCK pin, and every client surface,
  and would outlive the spike it serves.
- **Promoting the `#[cfg(test)]` hooks to a production global.** The
  hooks are thread-locals with no open-time resolution site, so the
  fail-open contract (an unknown value must reject the open, before any
  store is leased) has no home, and a mutable global would race the
  executor's per-statement access. Rejected: resolution belongs at
  `Database::open`, where every value-carrying variable already
  resolves.
- **An `OpenOptions` field beside the variable.** Rejected: that is
  public Rust API with a stability promise for a value whose whole
  contract is that it is temporary and dies with the old engines; the
  environment variable is the entire surface.

## Consequences

- A store opened with the variable unset, blank, or `auto` behaves
  byte-for-byte as before; the executor's default `StoreServices`
  bundle carries `Auto`, so hand-built graphs (tests, tools) route as
  today with no configuration step.
- A bad value fails every open — the CLI's query session and `serve`,
  and every library embedding — with the authored error naming the
  variable and the three valid values, before a store directory is
  leased or written.
- The switch rides the store's one service bundle: every namespace
  (`neo4j`, `system`, `system_ontologies`) routes alike, and a follower
  keeps the value across a reload with the rest of the store's
  configuration.
- The forced values change only routing, never answers: every forced
  engagement still runs the same proofs, and every decline (including
  the run-time re-arrival fall-through) keeps the rung below, so the
  openCypher TCK remains the net under any value.
- Removing the switch later is one change: the enum, the two
  `Graph` accessors, the resolver, the stand-down checks at the rung
  entries, the cost-gate override in `compile_frontier`, the doc rows,
  and this ADR's record of why it existed.
