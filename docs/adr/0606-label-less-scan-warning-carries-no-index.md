---
type: adr
area: engine
claim: b98af1a894c0a048ae30ec664545ad19c812f9c0
name: 0606-label-less-scan-warning-carries-no-index
status: proposed
tags:
  - observability
title: The label-less scan warning carries no index suggestion
REFERENCES:
  - "[[adr/0363-query-notifications]]"
---

# ADR-0606: The label-less scan warning carries no index suggestion

## Context

[ADR-0363](./0363-query-notifications.md) gave `AllNodesScan` an index payload: when a plan started with a
scan of every node and the store carried any property index, the warning
named one of those indexes as the remedy. The chosen index was arbitrary.
A pattern with no label cannot be served by a label-and-property index, so
the named index could never narrow the scan the warning was raised on. The
hint was wrong advice, rendered in the plan table, the notification text and
the wire form.

The engine's stance is that unlabeled patterns are antipatterns and are not
optimized. The honest remedy is to add a label.

## Decision

1. `AllNodesScan` is a bare warning with no payload. Its text says the scan
   matches every node and advises adding a label to narrow it.
2. The warning still fires only when the store carries at least one property
   index, as before; the gate is unchanged, only the advice differs.
3. The wire form carries the warning by name alone. A decoder rejects an
   `AllNodesScan` that arrives with an index payload rather than dropping it.
4. Plan-tree suggestions elsewhere (`LabelDisjunctionScan`) keep naming
   concrete indexes; they name pairs the pattern can actually use.

## Alternatives considered

- **Pick the index by label statistics.** There is no label to pick from; any
  choice stays arbitrary. Rejected: it cannot be correct for a label-less
  pattern.
- **Keep the payload and ignore it in renders.** A dead field invites a future
  reader to trust it. Rejected: parallel state with no meaning.
- **Remove the warning.** Loses the one signal that a label-less scan is
  costly in an indexed store. Rejected: the signal is right, only the advice
  was wrong.

## Consequences

- Drivers and clients that read the plan wire form see `AllNodesScan` without
  the index key; consumers that required it must tolerate its absence.
- The surface specs, the plan-table and notification texts, and the pinned
  tests move in the same change.
- [ADR-0363](./0363-query-notifications.md) gains a later-references note; its vocabulary otherwise stands.
