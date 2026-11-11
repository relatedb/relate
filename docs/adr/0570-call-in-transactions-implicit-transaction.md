# ADR-0570: CALL { … } IN TRANSACTIONS requires an implicit transaction

Status: Proposed
Claim: e2004f83392a8d037bfefb89e5e38e08d7a4b59c

## Context

ADR-0210 gave the engine per-batch commits for `CALL { … } IN
TRANSACTIONS` and recorded a consequence for explicit transactions: the
batches do not commit independently but accumulate in the enclosing
transaction's write overlay and become visible when it commits.

Live Neo4j 2026.10.1 refuses the statement instead. A query carrying the
clause inside an open explicit transaction fails with GQLSTATUS 25N17
("A query with 'CALL { ... } IN TRANSACTIONS' can only be executed in an
implicit transaction, but tried to execute in an explicit transaction"),
for both the `IN TRANSACTIONS` and the `IN CONCURRENT TRANSACTIONS`
spelling, and the openCypher corpus pins that rejection. The overlay
behavior also has no defined commit boundary: the statement's batch
machinery assumes it owns the commits.

## Decision

A statement whose plan contains `CALL { … } IN TRANSACTIONS` is refused at
admission while an explicit transaction is open, before any work runs. The
refusal leaves the open transaction usable and rollback-able. Statements
running in an implicit (auto-commit) transaction keep ADR-0210's per-batch
commit behavior unchanged.

## Alternatives considered

- **Keep the overlay behavior.** Rejected: the reference rejects the same
  statement, the corpus pins the rejection, and batching inside another
  transaction has no defined commit boundary.
- **Refuse at execution time rather than admission.** Rejected: whether
  the statement can run is a static fact about the plan and the open
  transaction, so admission is the earliest point and the one every other
  transaction-shape rule uses.
- **Run the body as ordinary single-transaction work when an explicit
  transaction is open.** Rejected: that silently changes what the
  statement means; the reference refuses it rather than reinterpreting.

## Consequences

- The explicit-transaction batching contract and its tests are replaced by
  a refusal test; implicit-transaction batching is unchanged.
- ADR-0210's consequence about explicit transactions is superseded; its
  implicit-transaction contract stands.
- The refusal carries the engine's semantic class; the corpus enforces
  only that an error is raised for GQLSTATUS 25N17.
