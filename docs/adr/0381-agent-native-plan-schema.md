---
type: adr
area: engine
claim: 278bbb8371559fec84a5305564d4bac35b35f677
name: 0381-agent-native-plan-schema
status: proposed
tags:
  - agents
  - observability
title: The agent-native plan schema (`cypherlite.plan/v1`)
REFERENCES:
  - "[[adr/0357-plan-summary-placement-and-neo4j-plan-args]]"
  - "[[adr/0359-single-plan-renderer-at-the-boundary]]"
  - "[[adr/0363-query-notifications]]"
  - "[[adr/0365-http-query-api-v2-plan-node-shape]]"
---

# ADR-0381: The agent-native plan schema (`cypherlite.plan/v1`)

## Context

The structured plan reaches Neo4j's own tooling: the Bolt and HTTP
surfaces project the engine's plan tree onto Neo4j's node shape
([ADR-0357](./0357-plan-summary-placement-and-neo4j-plan-args.md), [ADR-0365](./0365-http-query-api-v2-plan-node-shape.md)) so drivers, cypher-shell and Browser read it, and
the CLI renders it through the formatter ([ADR-0359](./0359-single-plan-renderer-at-the-boundary.md)). Our **own** bindings
— the C ABI and everything that wraps it (Python, JS, Java, Go, WASM) and
the MCP server — expose no plan at all: an application or agent using
them cannot see why a query is slow.

Neo4j's shape is the wrong contract for those consumers: it is a
presentation-era format — metrics and display text share one nested
`args` map, values are stringly typed, warnings carry no remedy, and the
key names (`operatorType`, `EstimatedRows`, `TimeNs`) are Neo4j's, not
ours to version. Agents and applications need the opposite: a small,
stable, versioned schema with real numbers and stable field names.

A node's `warnings` carry the engine's remedy-bearing notifications
([ADR-0363](./0363-query-notifications.md)) for that operator — code, GQLSTATUS, severity, category, title,
description and the actionable remedy — joined by the operator id the
engine stamps, so an agent reads the diagnosis and the action beside it.

## Decision

1. **One versioned schema, `cypherlite.plan/v1`**, is the plan contract
   for our own surfaces. A document is
   `{schema, profiled, statement, root}` where each node is
   `{operator, details, identifiers, estimated_rows, actual, children,
   warnings}` and `actual` is
   `{rows, time_ms, db_hits, memory_bytes, hops, neighbor_walks,
   node_reads, start?}` — `snake_case`, real numbers, no `args` nesting,
   no rendered text required to read it. A breaking change bumps the
   version and ships its own ADR; fields are added, never renamed in
   place.

2. **The projection lives in the model**
   (`cypherlite_model::wire::agent_plan::plan_json`), reads the engine's
   wire form through the one typed accessor (`plan_node_wire`), and is
   the single spelling every binding hands out. It never re-matches the
   raw wire map.

3. **The C ABI is the exposure point**: the query result carries the
   projected document (`cypherlite_result_plan_json`, result-owned,
   empty on a statement with no plan). Language bindings wrap that
   accessor — they do not invent a parallel protocol — so every binding
   hands out the same bytes.

4. **The Neo4j-facing projection stays** for Bolt and HTTP
 ([ADR-0357](./0357-plan-summary-placement-and-neo4j-plan-args.md), [ADR-0365](./0365-http-query-api-v2-plan-node-shape.md)): Neo4j's shape is the *wire dialect* for Neo4j
   tooling, `cypherlite.plan/v1` is *our API dialect*. Both project the
   same typed view of the same tree; neither re-derives the other's
   facts.

## Alternatives considered

- **Mirror Neo4j's plan shape in our bindings too.** Rejected: it is a
  presentation-era contract — nested `args`, string-typed values,
  warnings without remedies, key names we do not own and cannot version.
  An agent reading it must re-derive facts the engine already knows.
- **Expose the engine's raw wire form.** Rejected: it is an internal
  contract (our `args` key names, our metric field names) with no
  stability promise, and it carries the engine's own vocabulary rather
  than a documented API.
- **Let each binding define its own JSON.** Rejected: the same plan would
  be shaped differently per language, and every consumer's parsing would
  drift with the first change.
- **Return the rendered text only.** Rejected: text is for humans and
  LLM context; an application needs the tree with numbers and codes
  (the remedy in particular is unusable as prose).

## Consequences

- The model gains one projection function and the schema becomes a
  documented contract (each binding's spec names the version and its
  fields).
- The C ABI grows one result accessor; every language binding that
  exposes plans wraps it and ships spec + description + example in the
  same change (the surface-documentation rule).
- A schema change is a versioned, ADR-backed change rather than an
  incidental wire edit.
- The CLI's and HTTP's own JSON projections can later adopt the model's
  value encoding, removing private duplicates, without changing this
  schema.
