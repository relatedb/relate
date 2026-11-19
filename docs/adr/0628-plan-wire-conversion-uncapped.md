---
type: adr
area: engine
claim: ed6f2e3525783946ebd641fd93364b88a2cf5554
date: 2026-11-19
deciders: the maintainers
name: 0628-plan-wire-conversion-uncapped
status: proposed
tags:
  - result-metadata
  - values
title: The plan wire conversion does not ride the value-nesting depth cap
REFERENCES:
  - "[[adr/0257-query-input-limits]]"
  - "[[adr/0345-plan-surface-alignment-cli-bolt-http]]"
  - "[[adr/0462-value-nesting-depth]]"
---

# 0628: The plan wire conversion does not ride the value-nesting depth cap

## Context

[ADR-0462](./0462-value-nesting-depth.md) caps runtime **data values** at 500 container levels: a dynamically built value (`reduce`'s accumulator, a `WITH [a] AS a` chain) can reach a depth the query text never shows, so every infallible walker saturates at the cap — the model bridge `value_to_cypher` crosses a container past it as `CypherValue::Null`.

A query plan tree is not a runtime data value. It is engine-produced metadata whose depth scales with the statement's clause count — every chained plan node costs about two container levels (its node map plus the children list around the next). A 251-clause statement (a MATCH, 125 node CREATEs, 124 relationship CREATEs, a RETURN) reaches ~502 levels; past the cap its deep nodes cross the bridge as `Null`, where the invariant-bound plan-wire accessor panics loudly on every plan-rendering surface: the wire envelope's plan document (the Python, FFI and MCP surfaces), the formatter's table and JSON renders (the CLI), and the HTTP Query API v2 plan field.

The cap was never aimed at plan trees. [ADR-0462](./0462-value-nesting-depth.md)'s threat is unbounded dynamic construction invisible to the parser; a plan tree's depth is bounded by the statement's own shape ([ADR-0257](./0257-query-input-limits.md)'s 4 MiB text limit bounds the clause count), and the producer `PlanNode::to_value` already recurses that depth to build the tree — an uncapped crossing adds no stack exposure the producer did not already take.

## Decision

One canonical plan-wire crossing beside `value_to_cypher` — `plan_value_to_cypher`, the identical mapping without the depth guard — and every plan-path crossing routes through it: the wire envelope's plan document projection, the formatter's plan presentation, and the HTTP Query API v2 plan field. Never a per-surface re-derivation.

The 500-level cap is unchanged for data values, as are its checked constructors and saturation valves.

Bolt's plan crossing is unchanged: it packs the engine value through its own codec, whose 128-level PackStream limit degrades to the documented `planTruncated` summary marker — a wire-protocol limit with a client-safe fallback, not the model bridge.

## Alternatives considered

- **Raise `MAX_VALUE_DEPTH`.** Rejected: it reopens [ADR-0462](./0462-value-nesting-depth.md)'s axis for data values to fix a crossing plan trees never needed to ride.
- **Let the wire accessor tolerate a `Null` node.** Rejected: a silently truncated plan is a silent fallback; the accessor's loud wire-contract panic stays.
- **A plan-tree depth cap of its own.** Rejected: any constant re-creates the same panic at a larger clause count; the honest bound is the statement's own shape.
- **A flatter plan wire form.** Rejected: the Neo4j-shaped nested tree is the consumer contract ([ADR-0345](./0345-plan-surface-alignment-cli-bolt-http.md)); reshaping it to dodge a bridge policy breaks every reader.

## Consequences

- Long-clause statements plan and render on every surface; the clause-count panic threshold is gone. Pinned by the 251-clause repro (both its EXPLAIN and its plain executing form) and a bridge unit test contrasting the two crossings past 500 levels.
- The plan-wire crossing recurses to the tree's full depth, mirroring the producer's own recursion — the governing stack exposure stays the producer's, unchanged.
- [ADR-0462](./0462-value-nesting-depth.md)'s scope is refined: its cap governs runtime data values; engine-produced plan metadata crosses uncapped. It gains a Later references note to this ADR.
- Data-value behavior is unchanged: the cap, the checked constructors and the `Null` saturation valve all stand.

## Later references

- The rust review round refined the bound twice, ending at: the crossing
  is **iterative** — an explicit heap work stack building the value
  bottom-up, constant caller-stack depth however deep the value nests —
  and "uncapped relative to the data-value policy" became bounded by a
  walk-sized plan budget, `MAX_PLAN_WIRE_DEPTH` = 1 000 container levels
  (~500 chained clauses, ~2× headroom over this ADR's own 251-clause
  repro). The budget sizes what still recurses per level on the calling
  surface's thread — the consumer WALKS of the crossed value: its
  recursive drop (~1 frame/level) and the document builds
  (`plan_document`/`node_json`, `v2_plan_node`, the plan table's layout:
  1–3 frames/level), ~600 B/level at the worst walk — so every walk
  stays under ~600 KB at the budget, inside the smallest
  caller-provided stack (an FFI host thread: Java and .NET default to
  ~1 MiB) with margin. Past the budget the crossing errors loudly: the
  typed capacity refusal in the `PlanWire` class, whose text states the
  scope (the statement itself executed; only its plan document is
  refused). An earlier 10 000-level draft was sized against the engine's
  8 MiB stacks, ran the crossing itself recursively (~2 frames/level ≈
  6 MB of walk at the budget, not the ~3 MiB its doc derived), and so
  overran a ~1 MiB FFI host thread before the budget fired — the
  review's two defects. The security round then closed the step before:
  the producer build (`to_value`) also recursed per plan operator on the
  caller's thread, unbounded, before the crossing ran — so every
  plan-crossing boundary now probes the TYPED tree first
  (`PlanNode::ensure_plan_wire_budget`, an iterative depth probe, no
  build) and refuses there, and the recursive builds and document walks
  only ever run on trees within the budget: the probe bounds the builds,
  the iterative crossing bounds the conversion, the budget bounds the
  remaining recursive walks — and the typed tree's own `Drop` is an
  iterative drain (a heap worklist surrendering each node's children),
  so the tree built on the engine's big-stack worker drops stack-flat
  wherever the caller's result drops, at any depth.
