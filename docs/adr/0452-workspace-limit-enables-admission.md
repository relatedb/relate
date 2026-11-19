---
type: adr
area: engine
claim: 1ffe592eaff0aef9e69b4ef0656159357c6a4caf
name: 0452-workspace-limit-enables-admission
status: proposed
tags:
  - memory
title: The concurrent logical-workspace limit is the admission switch
---

# ADR-0452: The concurrent logical-workspace limit is the admission switch

## Context

Token admission caps the logical workspace executing queries hold at once.
It shipped as two knobs. `--token-admission` (`OpenOptions.token_admission`,
`CYPHERLITE_TOKEN_ADMISSION`) turned the pool on, and
`--concurrent-logical-workspace-limit` set its capacity. The limit was
required only when the switch was on, and the switch defaulted off.

The live-graph cap is one knob on every open path: the `cypherlite` query
flags, `cypherlite serve`, the bolt crate's `Args`, and `OpenOptions`,
resolved as the flag, then `CYPHERLITE_LOGICAL_GRAPH_SIZE_LIMIT`, then
unlimited. The workspace limit was not. `cypherlite --help` showed the graph
cap and not the workspace cap, and a query session hardcoded admission off.
The same store could be capped when served and uncapped when opened from
the CLI or from `OpenOptions`, unless the caller also set the boolean.

A set limit with the switch left off looks like a cap that does not cap.

## Decision

A positive concurrent logical-workspace limit turns admission on. Unset
leaves it off. There is no separate switch.

- Remove `--token-admission`, `OpenOptions.token_admission`, and
  `CYPHERLITE_TOKEN_ADMISSION`. A set `CYPHERLITE_TOKEN_ADMISSION` fails
  open and names `CYPHERLITE_CONCURRENT_LOGICAL_WORKSPACE_LIMIT`, the same
  "set but ignored" contract as a renamed limit variable.
- `--concurrent-logical-workspace-limit SIZE` is accepted wherever
  `--logical-graph-size-limit SIZE` is: the query session, `cypherlite
  serve`, the bolt crate's `Args`, and
  `OpenOptions.concurrent_logical_workspace_limit`.
- Resolution matches the graph cap. The flag or `Some(n)` wins, otherwise
  `CYPHERLITE_CONCURRENT_LOGICAL_WORKSPACE_LIMIT`, otherwise off. `SIZE`
  is the graph cap's grammar (bytes or `mb`/`mib`/`gb`/`gib`). `0` is
  rejected. A bad value fails the same way as a bad graph cap (CLI exit 2;
  a library open is `Error::Semantic`).
- Off, overlapping queries are not refused and responses carry no
  workspace receipt. On, the pool capacity is that positive limit. The
  engine does not invent a default pool.

## Alternatives considered

- **Keep `--token-admission` and only add the limit flag to the query
  session.** Rejected: two switches for one pool, and a set limit with the
  switch off still looks like a cap that does not cap.
- **Treat `0` as off, matching the graph cap's unlimited.** Rejected: a
  zero pool is not unlimited, and a typo of `0` would silently disable
  admission. A positive value is the on switch; zero fails open.
- **Default a pool when the flag is omitted.** Rejected: admission stays
  opt-in. Unset means the process behaves as it does today.

## Consequences

- A deployment that set `CYPHERLITE_TOKEN_ADMISSION=1` together with the
  limit drops the admission variable. The limit alone enables admission.
  A deployment that set only the admission variable fails open until it
  sets the limit; that open already failed, because the limit was required.
- `CYPHERLITE_TOKEN_ADMISSION=0` no longer forces admission off when a
  limit is set. The limit is the switch.
- Query sessions and library opens honor the same flag and environment
  variable as `serve`.
