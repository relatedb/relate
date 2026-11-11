# 0028. User-visible multi-statement transactions

- **Status**: Accepted
- **Date**: 2026-08-31
- **Deciders**: MemoryDB maintainers

## Context

ADR-0011, ADR-0012, and ADR-0015 left Bolt `BEGIN` / `COMMIT` /
`ROLLBACK` as auto-commit no-ops. Each Cypher statement is its own WAL
commit. Clients cannot group statements, roll back uncommitted writes, or
pin one snapshot across a `CREATE` then `MATCH`. Engine concurrency
(ADR-0013 through ADR-0019) already gives snapshot reads and optimistic
write-write at WAL append. What is missing is a connection-scoped
transaction the client can start and finish.

OpenCypher TCK scenarios are each one auto-commit statement. This ADR
must not change that default.

## Decision

A `Connection` is auto-commit until the client starts an explicit
transaction. Isolation is the existing snapshot + write-write protocol,
stretched across statements. SSI / write-skew detection is out of scope
(same rejection as ADR-0015).

1. **Auto-commit (default).** Unchanged: a write statement appends redo
   and `apply_redo`s before `query` returns. A read pins `commit_ts` at
   statement start.
2. **`begin`.** Pins `snapshot_ts = last_committed` and allocates one
   `txn_id`. Nested `begin` is a semantic error (`already in a
   transaction`).
3. **Statements in the transaction.** Every subsequent `query` on that
   connection — read or write — runs on the shared graph plus the same
   private `WriteScratch` (ADR-0016). The connection sees the pinned
   snapshot and its own writes. Other connections do not see uncommitted
   writes. Stats on `QueryResult` stay per statement.
4. **`commit`.** The accumulated redo is one WAL record and one
   `commit_ts` (ADR-0015, ADR-0017, ADR-0019). Empty redo is a no-op (no
   WAL append). Write-write conflict discards the overlay and returns
   `Error::Conflict`. After `commit` (success or conflict) the connection
   is auto-commit again. `commit` without `begin` is `no transaction`.
5. **`rollback`.** Drops the overlay; nothing is appended. Connection
   drop is implicit rollback. `rollback` without `begin` is `no
   transaction`.
6. **Failed transaction.** A statement error marks the transaction
   failed. Further `query` / `commit` return `transaction failed;
   rollback required`. Only `rollback` (or drop) clears it. Partial
   overlay from the failed statement is discarded with the transaction.
7. **Surfaces.** Rust `Connection::{begin, commit, rollback,
   in_transaction}`; C ABI `memorydb_begin` / `memorydb_commit` /
   `memorydb_rollback`; Bolt `BEGIN` / `COMMIT` / `ROLLBACK` (RESET
   rollbacks an open transaction). Language clients wrap the C ABI.
   CLI meta: `.begin` / `.commit` / `.rollback`. HTTP `POST /query`
   stays auto-commit per request (no session affinity).
8. **Not a Cypher clause.** `BEGIN` / `COMMIT` / `ROLLBACK` are not
   added to the OpenCypher parser. The pinned TCK has no such statements.

## Alternatives considered

- **Keep auto-commit only** — simplest, matches TCK. Rejected; clients
  cannot group writes or undo a multi-statement attempt.
- **Serializable snapshot isolation** — predicate locks / write-skew
  detection. Rejected; same as ADR-0015. Write-write on the transaction
  write set at `commit` is enough.
- **Cypher `BEGIN` / `COMMIT` statements** — convenient in the shell.
  Rejected; not in the pinned TCK, and would invent dialect. API, Bolt,
  and CLI meta-commands are the surfaces.
- **HTTP session transactions** — would need connection affinity or
  cookies. Rejected; the localhost sidecar is request-scoped.
- **Conflict check after every write statement** — fails faster.
  Rejected; the WAL append is the serialize point (ADR-0015). Checking
  earlier would reject transactions that could still commit if the
  conflicting writer rolled back.

## Consequences

- ADR-0011 / ADR-0012 / ADR-0015 “BEGIN/COMMIT remain no-ops” is
  reversed here, not edited in those files.
- TCK and existing auto-commit callers are unchanged.
- A long-lived explicit transaction raises the chance of
  `write-write conflict` at `commit`.
- Uncommitted `CREATE`s consume process-wide node/rel ids; rollback
  leaves gaps (same as an aborted auto-commit write).
- Checkpoint and other connections see only committed rows.

## Later references

ADR-0029 removes engine HTTP. Auto-commit per HTTP request belongs
to clients implementing embedded Relate, not to `memorydb-http`.

ADR-0164 records the cypherlite CLI exit contract for an open `.begin`
transaction: the historical stderr warning is restored verbatim on every
exit path, one-shot and piped-stdin runs exit 1 when a transaction is
open at exit, and interactive exit (`.quit` / EOF) warns but stays 0.
The discard itself — an open transaction is never checkpointed — is the
implicit-rollback-on-drop behavior specified here, unchanged.
