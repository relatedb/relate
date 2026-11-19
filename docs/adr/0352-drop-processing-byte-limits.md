---
type: adr
area: engine
claim: 707b172146ac4b1a500039984de2fa2107709821
name: 0352-drop-processing-byte-limits
status: accepted
tags:
  - memory
title: Drop processing byte limits
REFERENCES:
  - "[[adr/0042-graph-memory-limit]]"
  - "[[adr/0333-processing-memory-controls]]"
  - "[[adr/0452-workspace-limit-enables-admission]]"
---

# ADR-0352: Drop processing byte limits

## Context

[ADR-0333](./0333-processing-memory-controls.md) names four processing controls. Two of them cap estimated
bytes: `--query-memory-limit` for one query, and
`--processing-memory-limit` for active queries plus checkpoint work.
Neither flag was implemented. The engine admits a query by taking one
`--max-active-queries` slot, and the operator sizes the process from
concurrency:

```text
9 MB + Graph Logical × 1.7 + 300 MB × --max-active-queries
```

A byte budget asks the customer to estimate retained rows, write
scratch, and checkpoint buffers, then asks the engine to charge those
same quantities before they exist. The estimate is not RSS. A
conservative charge refuses work that would have fit; an optimistic
charge still runs out of memory. A concurrency cap is a number the
customer can set from how many queries they want in flight.

## Decision

`--query-memory-limit` and `--processing-memory-limit`, and their
environment variables, are not controls. They are not accepted flags,
not admission meters, and not terms in the operating request.

Processing memory is bounded by `--max-active-queries`. Session count
stays `--max-connections`. Logical graph memory stays `--memory-limit`
([ADR-0042](./0042-graph-memory-limit.md)). All three remain as decided in [ADR-0333](./0333-processing-memory-controls.md) and [ADR-0042](./0042-graph-memory-limit.md).

The 300 MB factor is planning headroom for one executing query. It is
not a per-query byte cap. A query is admitted when an active-query slot
is free. It is not refused because an estimate of its rows, scratch, or
checkpoint bytes crossed a budget.

Out of scope: changing the 300 MB allowance, streaming results, and any
hidden internal byte meter.

## Alternatives considered

- **Implement the [ADR-0333](./0333-processing-memory-controls.md) byte budgets.** Rejected. Customers cannot
  reason about an estimated-byte cap the way they can reason about how
  many queries run at once, and the estimate does not match RSS.
- **Leave the flags specified but unimplemented.** Rejected. An accepted
  control that the CLI rejects is a false contract. The flags are
  already unknown arguments.
- **Admit on measured RSS.** Rejected. RSS is allocator- and
  platform-dependent, so it stays a measurement rather than an admission
  meter.

## Consequences

- [ADR-0333](./0333-processing-memory-controls.md)'s `--max-connections` and `--max-active-queries` decisions
  stay in force. Its byte-limit clauses, the rule that the query limit
  be at most the processing limit, the pre-growth byte charges, and the
  `--processing-memory-limit` term in its operating ceiling do not.
- There is no typed refusal for query or processing bytes. One query can
  use more than 300 MB. The formula is a container request, not a
  guarantee that every query stays inside that allowance.
- Unbounded row, scratch, and result retention is a memory-optimization
  problem. It is not closed by bringing these flags back.
- Checkpoint work still does not take an active-query slot, and it is
  not charged against a processing-byte budget.
- The CLI test that rejects `--query-memory-limit` remains the pin that
  the flag is not a surface.

## Later references

[Token budgets](../design/token-budgets.md) removes
`--max-active-queries`. The active-query slot is not a control. Opt-in
token admission is the query-concurrency gate. `--max-connections`
stays. [ADR-0452](./0452-workspace-limit-enables-admission.md) makes a positive `--concurrent-logical-workspace-limit`
that gate; there is no separate admission switch.
