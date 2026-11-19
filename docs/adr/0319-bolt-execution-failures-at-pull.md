---
type: adr
area: surface
claim: c84df8aabff60d841ee7bc8a7e5f485e820857b0
name: 0319-bolt-execution-failures-at-pull
status: proposed
tags:
  - bolt
  - errors
title: Bolt reports execution-phase failures at the first PULL or DISCARD
REFERENCES:
  - "[[adr/0028-user-visible-transactions]]"
  - "[[adr/0082-bound-bolt-decoded-value-count]]"
  - "[[adr/0110-bolt-connection-query-governance]]"
  - "[[adr/0177-bolt-parameter-conversion]]"
  - "[[adr/0224-result-column-types]]"
---

# ADR-0319: Bolt reports execution-phase failures at the first PULL or DISCARD

## Context

Neo4j plans a statement at RUN and executes it as the client pulls. A
failure while planning (syntax, semantic validation, a missing
parameter, an unknown database) fails the RUN. A failure while executing
an already planned statement (`10 / 0`, a constraint violation, a
write-write conflict) is reported at the PULL or DISCARD that drives the
execution: RUN answers SUCCESS with the planned columns and the PULL
answers FAILURE. Drivers follow that split: neo4rs's `execute()` succeeds
and `next()` returns the error (LABS-323 finding D-49).

cypherlite-bolt executes eagerly at RUN and buffers the result
([ADR-0110](./0110-bolt-connection-query-governance.md)), so every failure fails the RUN. Inside the engine,
`Connection::run_query` already has the split: parse, lower, plan and the
compile-time gates come first, then dispatch executes the plan.

## Decision

1. **Report point.** A failure raised while executing an already planned
   statement (the engine's dispatch phase) is reported at the first PULL
   or DISCARD of that result. RUN answers `SUCCESS {fields: <planned
   columns>, columnTypes, t_first}` (plus `qid` in a transaction), with
 `columnTypes` `"ANY"` per column ([ADR-0224](./0224-result-column-types.md)'s no-rows stamp: a failed
   execution yields no rows). The PULL or DISCARD then answers FAILURE
   with the error's code and message and no RECORDs, the result closes,
   and the session enters the failed state.
2. **What still fails the RUN.** Failures before execution: parse,
   semantic validation, planning, missing parameters, an unknown database,
 the [ADR-0177](./0177-bolt-parameter-conversion.md) parameter conversion, and the [ADR-0110](./0110-bolt-connection-query-governance.md) max-result-bytes
   cap.
3. **The phase comes from the engine.** Engine change (public Rust API):
   `Connection::query_staged(cypher, params, token, heartbeat)` returns
   `Err(error)` for a failure before dispatch, and `Ok(Executed)`
   otherwise. `Executed::Rows(QueryResult)` is a finished result;
   `Executed::Failed(ExecutionFailure)` carries the planned columns, their
   `ANY` column types, the query type, the namespace, the elapsed time and
   the error. The planned columns come from a new planner function,
   `cypherlite_planner::plan_output_names`, the same output-name analysis
   semantic validation uses. The existing `query*` entry points keep their
   signatures and turn `Failed` into `Err`.
4. **One Bolt classification.** A dispatch-phase error still fails the
   RUN when its typed variant is compile-class in Neo4j even though the
   engine raised it while executing: `Syntax`, `ProcedureNotFound` and
   `ParameterMissing`. Every other variant defers to PULL. This is one
   exhaustive `match` over `cypherlite::Error` in the Bolt session module,
   with no wildcard arm, so a new engine variant fails to compile until it
   is classified. No message text is inspected.
5. **Transactions.** The engine already marks an explicit transaction
 failed when a statement fails ([ADR-0028](./0028-user-visible-transactions.md) item 6), so a RUN or COMMIT
   sent before that PULL fails with the engine's `transaction failed;
   rollback required`.
6. **[ADR-0110](./0110-bolt-connection-query-governance.md) stays.** RUN still executes eagerly and buffers; only the
   report point moves, so nothing is superseded.

## Alternatives considered

- **True streaming execution.** It would report failures where they
  happen, including after some rows. Rejected: executor streaming stays
 deferred ([ADR-0082](./0082-bound-bolt-decoded-value-count.md)), and moving the report point gives drivers the
  Neo4j behaviour now.
- **Classify by error variant only, without the engine phase.** Rejected:
  `Semantic` is raised both by validation (compile-class) and by
  execution (for example deleting a node that still has relationships),
  so the variant alone cannot place it.
- **Classify by message text.** Rejected: the code-shape rules forbid
  stringly-typed discrimination.
- **Send `fields: []` on the deferred RUN SUCCESS.** Rejected: drivers
  expose `keys()` before the first record, and Neo4j reports the planned
  columns.

## Consequences

- neo4rs `execute()` succeeds and `next()` fails for runtime errors, as
  with Neo4j. The Python driver raises the same error at the same call it
  did before, since it pulls before returning records.
- The Rust API gains `Connection::query_staged`, `Executed` and
  `ExecutionFailure`, and the planner gains `plan_output_names`
  (`docs/api/rust/spec.md`).
- Pinned by `tests/compat/scripts/protocol/runtime-error-at-pull.bolt` and
  the `protocol_runtime_error_tests.rs` unit tests.
