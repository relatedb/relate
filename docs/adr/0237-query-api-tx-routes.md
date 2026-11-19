---
type: adr
area: surface
name: 0237-query-api-tx-routes
status: accepted
tags:
  - http
  - transactions
title: Query API v2 managed-transaction routes on engine transactions
REFERENCES:
  - "[[adr/0028-user-visible-transactions]]"
  - "[[adr/0232-network-surfaces-charter]]"
  - "[[adr/0435-managed-tx-lifecycle]]"
SUPERSEDES:
  - "[[adr/0232-network-surfaces-charter]]"
---

# ADR-0237: Query API v2 managed-transaction routes on engine transactions

## Context

[ADR-0232](./0232-network-surfaces-charter.md) §3 chartered the HTTP Query API surface with the auto-commit
statement route only; managed-transaction routes
(`/db/{name}/query/v2/tx…`) were explicitly excluded. In practice the
official clients open an explicit transaction for managed work: the
experimental JS Query API wrapper's `session.executeRead`/`executeWrite`
POSTs `/tx` (a pure BEGIN can be just `{"accessMode":"READ"}`), runs
statements on `POST /tx/{id}`, and closes with `POST /tx/{id}/commit` or
`DELETE /tx/{id}`. Neo4j Browser's connection bootstrap (the
`dbms.components()` version check) runs through `executeRead`, so the
missing route 404'd the whole connect flow.

The engine has had real explicit transactions all along ([ADR-0028](./0028-user-visible-transactions.md):
`Connection::begin`/`commit`/`rollback`, one consistent snapshot per tx),
so an honest mapping exists — no need to fake one.

## Decision

Implement the tx subset of the Query API v2 on top of engine
transactions:

- `POST /db/{name}/query/v2/tx` — begin: opens `Connection::begin()` on
  the namespace, registers it in a per-server `TxRegistry` (process-local
  ids `tx-N`, never reused), runs the body's optional statement, answers
  **201** with the success envelope plus `transaction: {id, expires}`. A
  failing begin statement rolls the fresh transaction back instead of
  leaking it.
- `POST /db/{name}/query/v2/tx/{id}` — run a statement in the open tx
  (200 + envelope + `transaction` block). An empty body is the keep-alive
  probe.
- `POST /db/{name}/query/v2/tx/{id}/commit` — run the optional final
  statement, commit, close (200 + envelope, no `transaction` block).
- `DELETE /db/{name}/query/v2/tx/{id}` — roll back, close (202).
- An unknown or closed tx id is a 404 error envelope.
- `accessMode: "READ"` is honored: a read transaction rejects write and
  schema statements (classified through EXPLAIN, the same discipline the
  MCP tools use); the tx stays open for the client to close.

Semantics honesty notes: the engine is single-writer optimistic, and the
registry serializes tx statements behind a mutex (the engine's
`Connection` is `Send` but not `Sync`). The reported `expires` (60 s out)
is a client bookkeeping hint; we do not expire open transactions — they
close on commit, rollback, or server shutdown. There is no cross-server
tx affinity (`tx_host` is omitted).

## Alternatives considered

- **Keep the routes 404 (charter status quo).** Rejected: the official
  clients' managed-transaction flows (Browser's version check, any
  `executeRead/executeWrite`) all begin at `POST …/tx`; without it the
  surface is unusable for them.
- **Fake transactions (auto-commit per statement, commit a no-op).**
  Rejected: rollback would silently not roll back. The engine's real
  explicit transactions make the honest mapping cheap.
- **Idle-timeout enforcement matching `expires`.** Deferred: enforcement
  needs a reaper; the field is reported for client bookkeeping only and
  documented as such.

## Consequences

- [ADR-0232](./0232-network-surfaces-charter.md)'s charter exclusion of managed-transaction routes is superseded
  for this subset (a Later references note records it).
- `HttpConfig` gains a `transactions: TxRegistry` shared by all
  connections of a server; `query_api::handle` takes it.
- The discovery document advertises `transaction` (the routes exist).
- Statement bodies on tx routes reuse the auto-commit shape with
  `statement` optional; `accessMode` is honored on tx routes and still
  ignored (documented) on the auto-commit route.

## Later references

- [ADR-0435](./0435-managed-tx-lifecycle.md)'s managed-transaction lifecycle supersedes this decision's two
  semantics-honesty statements. The registry no longer serializes tx
  statements behind a mutex held across the statement: the run route leases
  its entry out of the registry, runs the statement unlocked, and returns
  the entry, so no engine statement runs under the registry mutex
 ([ADR-0435](./0435-managed-tx-lifecycle.md) §1). Open transactions now expire: an idle timeout (60 s
  default) is enforced lazily by every registry operation's sweep, and a
  cap (128 default) bounds open entries, answering `404`/`42N90` and
  `429`/`42N91` respectively; the "we do not expire open transactions"
  sentence and the deferred idle-timeout alternative above are superseded
 ([ADR-0435](./0435-managed-tx-lifecycle.md) §1 and §2).
