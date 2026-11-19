---
type: adr
area: cli
claim: c47ec0e106a329d76d49be8245f51cdc92602ffe
name: 0501-drop-token-schedule
status: proposed
tags:
  - memory
  - output
title: Receipts report workspace memory and carry no token schedule
REFERENCES:
  - "[[adr/0428-embeddable-http-query-api]]"
  - "[[adr/0440-follower-writer-toggle]]"
  - "[[adr/0452-workspace-limit-enables-admission]]"
---

# ADR-0501: Receipts report workspace memory and carry no token schedule

## Context

Admission and charging are logical workspace bytes. `--concurrent-logical-workspace-limit`
caps the shared pool, and a statement's receipt is the high-water mark of
the workspace it held (`peak_logical_bytes`). One quantum is 4096 bytes
(`CHARGE_QUANTUM_BYTES`). That replaced the old token unit, where one
token was one quantum (the 0.14 rename, [ADR-0452](./0452-workspace-limit-enables-admission.md)).

The receipt still published a schedule name beside that peak:
`ExecutionReceipt.token_schedule` and `TokenEstimate.token_schedule` were
the string `"v1"` (`cypherlite_core::tokens::SCHEDULE_V1`). PROFILE plan
args carried `TokenSchedule`, and an HTTP error receipt carried
`metrics.tokenSchedule`. The string named the charging rules. It was not
a measure of workspace, and there is no second schedule. Readers that
want the memory the statement held already have the peak in logical bytes.

## Decision

Drop the schedule name. The reported figure is the peak logical workspace.

- Remove `token_schedule` from `TokenEstimate` and `ExecutionReceipt`.
  Remove `SCHEDULE_V1`. The type names stay.
- PROFILE and Bolt plan args carry `PeakLogicalBytes` and do not carry
  `TokenSchedule`.
- An HTTP error receipt carries `metrics.peakLogicalBytes` and
  `metrics.retryable`, and does not carry `metrics.tokenSchedule`.
- No alias and no replacement version field. The quantum stays
  `CHARGE_QUANTUM_BYTES` in the engine. It is not a receipt field.

`EXPLAIN` still does not reserve and still attaches no peak. Admission
off still attaches no receipt.

## Alternatives considered

- **Keep `TokenSchedule: v1` as a version of the charge rules.** Rejected.
  The published name is a token schedule, and the unit is workspace bytes.
  One quantum constant already fixes the rules. A version string that
  never changes is not the memory the statement held.
- **Rename the field to a workspace schedule.** Rejected. The receipt's
  job is the peak workspace, not a label for the rules that produced it.

## Consequences

- Breaking for a reader of `token_schedule`, plan arg `TokenSchedule`, or
  HTTP `metrics.tokenSchedule`. `peak_logical_bytes`, `PeakLogicalBytes`,
  and `metrics.peakLogicalBytes` are unchanged.
- `docs/design/token-budgets.md`, `docs/api/http/spec.md`,
  `docs/api/bolt/spec.md`, and `docs/api/rust/spec.md` describe the peak
  and not a schedule.
- [ADR-0428](./0428-embeddable-http-query-api.md)'s `metrics.tokenSchedule` and [ADR-0440](./0440-follower-writer-toggle.md)'s consequence that names
  it describe the wire those ADRs shipped. This ADR removes that key.
