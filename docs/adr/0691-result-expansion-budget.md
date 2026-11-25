---
type: adr
area: engine
claim: 1529195d2a06a8f761d4a8c7ef202eb50dcab767
name: 0691-result-expansion-budget
status: proposed
tags:
  - memory
  - security
title: A result cell's expanded size is charged against the expression cap
REFERENCES:
  - "[[adr/0170-shared-value-payloads]]"
  - "[[adr/0462-value-nesting-depth]]"
  - "[[adr/0463-expression-allocation-budget]]"
---

# ADR-0691: A result cell's expanded size is charged against the expression cap

## Context

[ADR-0463](./0463-expression-allocation-budget.md) bounds each allocation an expression builds. A list
literal over shared payloads ([ADR-0170](./0170-shared-value-payloads.md)) allocates one small buffer of
references, so every step passes the cap while the value it builds
doubles in expanded size:

```cypher
RETURN reduce(acc = [1], x IN range(1, 60) | [acc, acc])
```

The engine holds a few kilobytes. Every surface that crosses the value
out (the Value-to-CypherValue bridge, the table, JSON, Bolt and HTTP
renderers) walks it as a tree of 2^60 leaves, spending CPU and memory the
engine never charged. Streaming rows does not help: the expansion is
inside one cell.

## Decision

The statement boundary counts the expanded size of every result cell
(each reference counted, as `logical_bytes_within` already does for Bolt)
and refuses the statement with the existing `Error::Capacity` expression
class when one cell passes the expression cap
(`--expression-memory-limit`, default 256 MiB, `none` opts out).

- The count stops at the cap and remembers each container payload it
  measured (by allocation, for the call, over rows it holds), so the check
  costs at most the cap per distinct cell and is linear in the distinct
  payloads whatever wraps them. It runs on the materialized result and on
  each streamed batch.
- The refusal is typed and carries the cap; there is no silent
  truncation of a wire value.
- The `table` and `markdown` data cells additionally elide an over-long
  cell with an ellipsis; that is presentation of a value already within the cap.

## Consequences

A value that is small in memory but over the cap in expanded size is now
refused where it was previously rendered without bound. Per-surface
renderers keep walking the expanded tree, now bounded by the cap.
