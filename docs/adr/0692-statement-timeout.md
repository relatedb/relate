---
type: adr
area: engine
claim: a2fa580be08951500a6199f9a11aeb394bff3f02
date: 2026-11-25
deciders: Relate maintainers
name: 0692-statement-timeout
status: proposed
tags:
  - bolt
  - http
  - cli
  - timeout
title: a statement deadline, defaulted on the network surfaces, overridable per request
REFERENCES:
  - "[[adr/0110-bolt-connection-query-governance]]"
  - "[[adr/0111-engine-bounded-execution]]"
  - "[[adr/0210-call-in-transactions-batching]]"
  - "[[adr/0241-query-timeout-flag-and-exit-code-7]]"
  - "[[adr/0311-bolt-tx-open-results-by-qid]]"
  - "[[adr/0682-engine-batch-consumer]]"
---

# a statement deadline, defaulted on the network surfaces, overridable per request

## Context

[ADR-0111](./0111-engine-bounded-execution.md) gave every statement a
cooperative `CancelToken`. Two surfaces use it for a wall-clock bound: the CLI's
`--timeout` ([ADR-0241](./0241-query-timeout-flag-and-exit-code-7.md), opt-in,
exit code 7) and Bolt's server-wide `query_timeout`
([ADR-0110](./0110-bolt-connection-query-governance.md), default off). HTTP,
MCP and the embedded API have no bound.

Streaming ([ADR-0682](./0682-engine-batch-consumer.md)) makes the gap sharper:
a statement's snapshot pin now lives until the last row is delivered, and the
socket write/idle timeouts bound each gap, not the transfer. A reader that
trickles keeps the pin alive indefinitely.

Neo4j's model: a server setting (`db.transaction.timeout`, default off) that a
client may override per transaction (Bolt `tx_timeout` in `BEGIN`/`RUN` extra,
Query API `maxExecutionTime`), including to a larger value.

## Decision

1. **One deadline per statement, enforced through `CancelToken`.** A shared
   timer thread in `relate-core` (`deadline`) owns every armed deadline, so
   arming a statement is a lock and a map insert, not a thread spawn. It
   cancels the token with a cause (`CancelToken::timed_out`), which the
   engine's one statement choke point turns into the typed
   `Error::TimedOut { limit_ms, requested }`. It replaces the Bolt-local
   watchdog and the CLI-local watch. A caller's own cancel stays
   `Error::Cancelled`.
2. **Network surfaces default to 30 s** (Bolt, HTTP, MCP). `0` disables the
   default. The embedded Rust API, the C ABI and the CLI keep no default
   (a load or an import is legitimately long); they take the option only.
   There is no timer on wasm32 (no threads).
3. **A client may shorten or extend, as in Neo4j.** The per-request value
   replaces the server default (a long compute or a load sets a larger
   one); `0` from a client means no bound. The operator keeps one lever, a
   server **ceiling** (`--query-timeout-max-secs`, default unset = none):
   when set, the effective budget is `min(ceiling, request-or-default)`,
   including for a request that asks for no bound. `TimeoutPolicy::resolve`
   is the one resolution.
4. **The clock covers the whole statement**, from the start of execution to
   the last row handed to the socket, so it bounds a slow reader too: the
   write timeout bounds each stalled write, the statement budget bounds the
   transfer. A streamed statement parked on its consumer keeps its clock
   (Bolt's worker polls the token while it waits for the next PULL). In an
   explicit transaction each statement gets its own budget (the transaction
   itself is bounded by its lease, [ADR-0311](./0311-bolt-tx-open-results-by-qid.md)).
   **`CALL { … } IN TRANSACTIONS`** ([ADR-0210](./0210-call-in-transactions-batching.md))
   is one statement with one budget covering all its batches. On expiry the
   in-flight batch is rolled back (the existing per-batch snapshot
   restore); batches that already committed stay committed, the documented
   IN TRANSACTIONS trade-off. A cancellation is never absorbed by
   `ON ERROR CONTINUE` / `BREAK` / `REPORT STATUS`: the batch failure arm
   propagates it as `FAIL` after the rollback.
5. **A timeout is a typed error with Neo4j's codes.** `Error::TimedOut`
   maps to `Neo.ClientError.Transaction.TransactionTimedOut`, or
   `…TransactionTimedOutClientConfiguration` when the request chose the
   value. Delivery per surface:
   - Bolt: FAILURE with that code at the PULL that reached it (in-band on
     a started stream);
   - HTTP: **408** with the error envelope before the first row, the
     in-band `errors` member after the chunked head, then close;
   - MCP: the tool's error text;
   - CLI: exit 7 `timeout` (unchanged, [ADR-0241](./0241-query-timeout-flag-and-exit-code-7.md));
   - Rust / C / bindings: the typed error, the error shape's code, the
     Python `OperationalError` of kind `StatementTimeout`.
6. **Config and request surface.**
   - Bolt: `--query-timeout-secs` (default now `30`),
     `--query-timeout-max-secs`; per-request `tx_timeout` (integer ms) in
     the `RUN` and `BEGIN` extra (a negative or non-Int value is a request
     failure).
   - HTTP: the same flags through `relate serve`; per-request
     `maxExecutionTime` (seconds) on the statement and transaction routes.
   - MCP: the same flags; the `timeout_seconds` tool argument.
   - `relate serve`: `--query-timeout-secs`, `--query-timeout-max-secs`
     (`RELATE_QUERY_TIMEOUT_SECS`, `RELATE_QUERY_TIMEOUT_MAX_SECS`).
   - CLI: `--timeout` and the new `.timeout [DURATION|off]` dot command.
   - Rust: `Connection::set_statement_timeout(limit)` (and `set_statement_limit` for a resolved policy answer).
   - C ABI: `relate_conn_set_statement_timeout(conn, ms, &err)` (additive).
   - Bindings: Python `Connection.statement_timeout`, JavaScript
     `setStatementTimeout(ms)`, Go `SetStatementTimeout`, Java
     `setStatementTimeout(Duration)`, Swift `setStatementTimeout`, .NET
     `SetStatementTimeout`.

## Consequences

- A stalled or trickling consumer releases its snapshot pin after at most
  the budget (30 s by default); no separate stream-duration cap is needed.
- Existing Bolt deployments with statements longer than 30 s see the new
  default; `--query-timeout-secs 0` or a per-request `tx_timeout` is the
  escape, and the operator ceiling is the hard bound.
- One deadline implementation instead of two.

## Not decided here

- Whether Neo4j gives each inner transaction of `CALL … IN TRANSACTIONS`
  the full budget or the statement one budget is undocumented; this ADR
  picks one budget per statement.
- Which of the two timeout codes Neo4j sends over Bolt is undocumented;
  this ADR picks by who set the value.
