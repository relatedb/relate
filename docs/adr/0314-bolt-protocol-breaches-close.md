---
type: adr
area: surface
claim: b344a9cd29a319441807ff8987446ee88985cc4a
name: 0314-bolt-protocol-breaches-close
status: proposed
tags:
  - bolt
  - errors
title: Bolt protocol breaches answer Neo.ClientError.Request.Invalid and close the connection
REFERENCES:
  - "[[adr/0028-user-visible-transactions]]"
  - "[[adr/0110-bolt-connection-query-governance]]"
  - "[[adr/0311-bolt-tx-open-results-by-qid]]"
  - "[[adr/0313-bolt-hello-requires-user-agent]]"
---

# ADR-0314: Bolt protocol breaches answer Neo.ClientError.Request.Invalid and close the connection

## Context

Some requests are invalid in the session's current state: COMMIT or
ROLLBACK with no open transaction, BEGIN inside one, and PULL or DISCARD
with no open result. Latest Neo4j (LABS-323 findings F-32 to F-35, F-43
to F-46) treats each as a protocol breach. It answers one FAILURE with
code `Neo.ClientError.Request.Invalid` and the message `Message of type
<CommitMessage|RollbackMessage|BeginMessage|PullMessage|DiscardMessage>
cannot be handled by a session in the <state> state.`, then closes the
connection.

cypherlite-bolt passes COMMIT, ROLLBACK and BEGIN to the engine, which
answers `CypherLite.Error` `semantic error: no transaction` / `already in
a transaction`, and the session continues. PULL with no result answers
SUCCESS.

The [ADR-0110](./0110-bolt-connection-query-governance.md) failed state answers IGNORED to RUN, PULL, BEGIN, COMMIT and
ROLLBACK, but not to DISCARD.

## Decision

1. The breach checks run in the Bolt session from its own state, before
   any engine call:
   - COMMIT or ROLLBACK with no open transaction;
   - BEGIN inside an open transaction;
 - PULL or DISCARD with no open result at all ([ADR-0311](./0311-bolt-tx-open-results-by-qid.md) covers a qid
     that misses while other results are open).
2. A breach answers exactly one FAILURE `{code:
   "Neo.ClientError.Request.Invalid", message: "Message of type <Name>
   cannot be handled by a session in the <STATE> state."}` and the server
   closes the connection without reading further. `<Name>` is Neo4j's
   message class name. `<STATE>` is the Neo4j 4.4 state the session is in:
   `READY` (no transaction, no open result), `STREAMING` (no transaction,
   an auto-commit result open), `IN_TRANSACTION` (a transaction, no open
   result) or `TX_STREAMING` (a transaction with open results).
3. The [ADR-0110](./0110-bolt-connection-query-governance.md) failed-state gate runs first, so in the failed state these
   messages answer IGNORED, not a breach. DISCARD joins the gate: in the
   failed state it answers IGNORED like PULL, as in Neo4j's FAILED state.
4. The code constant is `REQUEST_INVALID_CODE` in `failures.rs` (shared
 with [ADR-0313](./0313-bolt-hello-requires-user-agent.md)). The engine's `Connection::begin`/`commit`/`rollback`
 contract ([ADR-0028](./0028-user-visible-transactions.md)) is unchanged; the session no longer reaches its
   "no transaction" and "already in a transaction" errors.

## Alternatives considered

- **Keep the engine errors and continue the session.** Rejected: drivers
  treat these as bugs in their own state tracking, and Neo4j's close
  makes the bug visible at once instead of leaving a session whose state
  the client misunderstands.
- **Answer the FAILURE and keep the connection in the failed state.**
  Rejected: that differs from Neo4j, and a client that sent a breach
  cannot be trusted to recover with RESET.
- **Hard-code `READY` and `IN_TRANSACTION` as the only state names.**
  Rejected: a COMMIT sent while an auto-commit result is open happens in
  Neo4j's `STREAMING` state, and naming the real state costs one match.

## Consequences

- A client that commits without a transaction, nests BEGIN, or pulls with
  nothing to pull loses its connection with Neo4j's code and text.
- DISCARD in the failed state answers IGNORED instead of SUCCESS.
- Pinned by `tests/compat/scripts/protocol/breach-commit-without-begin.bolt`
  and its siblings `breach-rollback-without-begin.bolt`,
  `breach-nested-begin.bolt` and `breach-pull-without-result.bolt`, and by
  the `protocol_breach_tests.rs` unit tests.
