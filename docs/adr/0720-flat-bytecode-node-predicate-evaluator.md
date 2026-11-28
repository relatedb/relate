---
type: adr
area: engine
claim: aa52002c0484c2a174f5c30cfd938de63f9b7411
name: 0720-flat-bytecode-node-predicate-evaluator
status: proposed
tags:
  - pattern-matching
  - executor
  - perf
title: Flat bytecode node-predicate evaluator replaces the one-predicate-per-variable kernel cap
REFERENCES:
  - "[[adr/0468-soa-batch-substrate]]"
  - "[[adr/0473-contracted-traversal-kernels]]"
  - "[[adr/0478-frontier-nfa-qpp-kernel]]"
  - "[[adr/0666-specialized-path-change-verification]]"
---

# ADR-0720: Flat bytecode node-predicate evaluator replaces the one-predicate-per-variable kernel cap

## Context

[ADR-0473](./0473-contracted-traversal-kernels.md) kept kernel admission monomorphic by bounding each node's compiled program (`Prog`) to three variants: labels only (`Bare`), one comparison (`Cmp`), or one null check (`Null`). Any node carrying two predicates (`WHERE f.age > 25 AND f.gender = 1`), a property map beside a `WHERE` conjunct, a multi-entry property map, or a boolean `OR`/`NOT` expression over its own properties declines the entire clause — dropping `Chain`, `HubStar`, and `FrontierNfa` back to the per-row walker.

Alongside `Prog`, three other partial predicate compilers (`BatchPred`, `WalkSpec`, and `PathPred`) grew with subtly different vocabularies and decline boundaries. On a 30.6M-edge social graph, adding a second predicate on the target node of a 2-hop distinct reach query (`WHERE f.age > 25 AND f.gender = 1`) forces `FrontierNfa` to decline with `Conjunct` even when forced, regressing to 3.1M db hits on the per-row walker.

Runtime JIT codegen remains ruled out by WASM support and sub-ms embedded query latency ([ADR-0468](./0468-soa-batch-substrate.md)). A small flat stack bytecode evaluator compiles once per plan, runs in a tight non-recursive loop over interned property tokens and once-evaluated row-invariant constants, reuses one scratch stack across candidates, and works identically on native and WASM targets.

## Decision

1. **Introduce `eval::bytecode::NodeBytecode`** — a flat stack-based bytecode program for single-node predicates over one variable:
   - Instructions: `LoadProp(u16)` (interned `NamedToken` read via `ExecGraph::node_prop_resolved`), `LoadConst(u16)` (row-invariant `Expr` evaluated lazily once per stage on first candidate), `Cmp(BinaryOp)` (delegating to canonical `binary_pure` with exact Cypher three-valued logic), `IsNull`, `IsNotNull`, `Not`, `And`, `Or`.
   - Compiles arbitrary boolean trees (`AND`, `OR`, `NOT`), multiple conjuncts on the same variable, multi-entry property maps, and mirrored comparisons over row-invariant bounds.
   - Evaluates with a stage-owned reusable `Vec<Value>` stack (zero per-candidate heap allocation) and exact Cypher three-valued boolean truth tables (`true`/`false`/`null`).
2. **Add `Prog::Bytecode(NodeBytecode)` as the fourth `Admission` / `NodePred` arm** in `exec/kernels/program.rs`:
   - Zero- and single-predicate nodes keep the existing `Bare`, `Cmp`, and `Null` arms with zero overhead.
   - Multi-predicate nodes, multi-entry property maps, and `OR`/`NOT` single-variable predicates compile into `Prog::Bytecode` across `Chain`, `HubStar`, and `FrontierNfa` instead of declining.
3. **Convergence path:** Follow-up slices migrate `BatchPred`, `WalkSpec`, and `PathPred` onto the same bytecode evaluator and retire those partial compilers, reducing both code size and specialized-path divergence surface ([ADR-0666](./0666-specialized-path-change-verification.md)).

## Alternatives considered

- **Add a `Multi(Vec<Prog>)` conjunction-only list:** Handles `AND` of comparisons but still declines `OR` and `NOT`, and cannot serve as the shared evaluator for `BatchPred` / `WalkSpec` / `PathPred`.
- **Keep declining to the per-row walker:** Leaves a severe, surprising cliff whenever a user adds a second filter predicate to a traversal or quantified path pattern.
- **Runtime JIT / Cranelift:** Incompatible with WASM and adds unacceptable compile latency on sub-ms embedded queries.

## Consequences

- Multi-predicate filters, multi-entry property maps, and `OR`/`NOT` single-node predicates stay inside `Chain`, `HubStar`, and `FrontierNfa` without falling back to the per-row walker.
- `Bare`, `Cmp`, and `Null` keep identical codegen and performance for single-predicate queries.
- Differential tests pin exact parity with the per-row walker across `AND`, `OR`, `NOT`, `NULL` three-valued logic, and error propagation under [ADR-0666](./0666-specialized-path-change-verification.md).
