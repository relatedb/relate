---
type: adr
area: engine
claim: d2b43b1fb1df1c9ae48c1696a34f045db6e63d3f
name: 0544-memory-envelope-improvements-warn
status: proposed
tags:
  - memory
  - observability
title: Memory envelope improvements warn
REFERENCES:
  - "[[adr/0526-memory-controls-gate]]"
  - "[[adr/0539-memory-shape-gate]]"
---

# ADR-0544: Memory envelope improvements warn

## Context

[ADR-0526](./0526-memory-controls-gate.md) and [ADR-0539](./0539-memory-shape-gate.md) fail the memory-controls and memory-shape jobs
when a lower-is-better metric falls below the frozen envelope. That
was meant to catch a broken measurement and to force a re-freeze
before a later regression could hide under a stale floor.

A real drop in memory use then blocks the pull request until a new
envelope lands. The comment already names the checks and asks for
that re-freeze. The failure does not add a signal the comment lacks.

## Decision

An envelope miss keeps its verdict. An improvement and a numeric
regression both stay in the compare document, the pull-request
comment, and the step summary. Inside GitHub Actions each such check
is a warning annotation. The compare exits 0, so the job stays green,
until a new envelope is frozen.

A behavioural regression still fails: a missed cap, a refused
connection, idle residue, a wrong shape count, a missing report, or a
schema the gate cannot read. A run whose regression is behavioural
still fails, including when the same run also has an envelope miss.
An incompatible environment class in CI still fails. Behaviour checks
have no improvement verdict, as [ADR-0526](./0526-memory-controls-gate.md) set out.

## Alternatives considered

- Keep failing on an improvement, as [ADR-0526](./0526-memory-controls-gate.md) and [ADR-0539](./0539-memory-shape-gate.md) decided.
  Rejected: a real drop blocks unrelated work, and the warning
  already asks for the investigation and the re-freeze.
- Drop the improvement verdict and only gate regressions. Rejected:
  the envelope would go stale with no record of the drop.

## Consequences

- A pull request whose only misses are envelope readings stays green.
  The comment still names each miss. A real drop, and a reading above
  the top, both want a new freeze.
- The nightly memory-controls and memory-shape runs use the same
  compare, so an envelope miss warns there too.
- The envelope can sit away from current use until someone re-freezes.
  The warning is the reminder. A behavioural miss still fails.
