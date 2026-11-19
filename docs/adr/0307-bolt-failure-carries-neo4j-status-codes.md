---
type: adr
area: surface
claim: 5a69b465ba0d39ac96184ef5ca3e56e51c19a864
name: 0307-bolt-failure-carries-neo4j-status-codes
status: proposed
tags:
  - bolt
  - conformance
  - errors
title: Bolt FAILURE carries Neo4j status codes
REFERENCES:
  - "[[adr/0017-client-conflict]]"
  - "[[adr/0057-embedded-c-abi-and-bolt]]"
  - "[[adr/0110-bolt-connection-query-governance]]"
  - "[[adr/0111-engine-bounded-execution]]"
  - "[[adr/0116-fixed-namespaces-community-edition-semantics]]"
  - "[[adr/0117-namespace-selection-apis-and-bolt]]"
  - "[[adr/0126-bolt-basic-auth]]"
  - "[[adr/0138-bolt-procedure-not-found-code]]"
  - "[[adr/0146-unknown-namespace-selection-is-graph-not-found]]"
  - "[[adr/0174-committed-catalog-constraint-rechecks]]"
  - "[[adr/0177-bolt-parameter-conversion]]"
  - "[[adr/0194-cli-error-envelope-and-exit-codes]]"
  - "[[adr/0260-trust-boundary-error-sanitization]]"
  - "[[adr/0311-bolt-tx-open-results-by-qid]]"
  - "[[adr/0313-bolt-hello-requires-user-agent]]"
  - "[[adr/0314-bolt-protocol-breaches-close]]"
  - "[[adr/0315-bolt-unknown-message-signature-fatal]]"
  - "[[adr/0319-bolt-execution-failures-at-pull]]"
SUPERSEDES:
  - "[[adr/0116-fixed-namespaces-community-edition-semantics]]"
  - "[[adr/0117-namespace-selection-apis-and-bolt]]"
  - "[[adr/0138-bolt-procedure-not-found-code]]"
  - "[[adr/0146-unknown-namespace-selection-is-graph-not-found]]"
  - "[[adr/0177-bolt-parameter-conversion]]"
  - "[[adr/0260-trust-boundary-error-sanitization]]"
---

# ADR-0307: Bolt FAILURE carries Neo4j status codes

## Context

The Bolt server ([ADR-0057](./0057-embedded-c-abi-and-bolt.md)) is meant to work with the official Neo4j
drivers and with neo4rs as if it were Neo4j 2026.09 speaking Bolt 4.4
[ADR-0260](./0260-trust-boundary-error-sanitization.md) pinned two CypherLite-only FAILURE codes:
every caller-facing engine error goes out as `CypherLite.Error`, and the
internal-class errors (`Internal`, `Storage`, `Io`, `Locked`) go out as
`CypherLite.InternalError` with the fixed message `internal error`.
[ADR-0138](./0138-bolt-procedure-not-found-code.md) made one exception (`Neo.ClientError.Procedure.ProcedureNotFound`),
and [ADR-0126](./0126-bolt-basic-auth.md) sends `Neo.ClientError.Security.Unauthorized` on a failed
HELLO. [ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md) §9, [ADR-0117](./0117-namespace-selection-apis-and-bolt.md) §7, [ADR-0146](./0146-unknown-namespace-selection-is-graph-not-found.md) and [ADR-0177](./0177-bolt-parameter-conversion.md) item 6 each restate
`CypherLite.Error` for their own failures.

Drivers do not read the message. They classify a failure by its code:

- the classification segment (`ClientError`, `TransientError`,
  `DatabaseError`) decides whether a managed transaction retries;
- the category and title pick the exception type (the Python driver's
  `CypherSyntaxError`, neo4rs' `Client(FatalDiscovery)` for an unknown
  database, and so on).

The probes (`bolt-compat-findings.json` F-41, `neo4rs-compat-findings.json`
D-46, D-47, D-50, D-51) show the result. A syntax error, an unknown
database and every other engine error reach the driver as
`CypherLite.Error`. neo4rs classifies that code as `Database`, so a driver
sees a server fault where Neo4j reports a client error. A write-write
conflict is never retried by a managed transaction, because
`CypherLite.Error` is not transient.

The typed engine error (`cypherlite_core::Error`, re-exported as
`cypherlite::Error`) already separates most of the classes Neo4j codes
need: `Syntax`, `ProcedureNotFound`, `ParameterMissing`, `Type`,
`TypeError`, `InvalidArgumentType`, `UniquenessViolation`, `Conflict` and
`Capacity`. Three classes are only visible in the message text:

- division and modulo by zero (`Type("/ by zero")`, `Type("% by zero")`)
  and `round(…, 'UNNECESSARY')` that needs rounding
  (`Execution("ArithmeticError: rounding necessary")`);
- an unknown database or `USE` target
 (`Semantic("DatabaseNotFound: Graph not found: <name>")`, [ADR-0146](./0146-unknown-namespace-selection-is-graph-not-found.md));
- query cancellation (`Execution("query cancelled")`, [ADR-0111](./0111-engine-bounded-execution.md)), which is
  how the Bolt `--query-timeout-secs` bound ends a query.

AGENTS.md forbids classifying these by matching the message.

## Decision

Every Bolt FAILURE carries a Neo4j status code. `CypherLite.Error` and
`CypherLite.InternalError` are no longer sent. The message is unchanged:
it is still the engine's `Display` text, verbatim, so CypherLite keeps its
own wording.

1. **Engine errors map by typed variant.** `failure_for_engine_error` in
   `surfaces/cypherlite-bolt/src/failures.rs` is one exhaustive match over
   `cypherlite::Error`, with no wildcard. A new engine variant does not
   compile until it is given a code.

   | Engine variant | Code |
   | --- | --- |
   | `Syntax` | `Neo.ClientError.Statement.SyntaxError` |
 | `ProcedureNotFound` | `Neo.ClientError.Procedure.ProcedureNotFound` (unchanged, [ADR-0138](./0138-bolt-procedure-not-found-code.md)) |
   | `ParameterMissing` | `Neo.ClientError.Statement.ParameterMissing` |
   | `Type`, `TypeError`, `InvalidArgumentType` | `Neo.ClientError.Statement.TypeError` |
   | `Arithmetic` (new) | `Neo.ClientError.Statement.ArithmeticError` |
   | `DatabaseNotFound` (new) | `Neo.ClientError.Database.DatabaseNotFound` |
   | `UniquenessViolation` | `Neo.ClientError.Schema.ConstraintValidationFailed` |
   | `Cancelled` (new; the query timeout) | `Neo.ClientError.Transaction.TransactionTimedOut` |
   | `Conflict` | `Neo.TransientError.Transaction.Outdated` |
   | `Capacity` (`--memory-limit`) | `Neo.ClientError.General.TransactionOutOfMemoryError` |
   | every other `Semantic` and `Execution` | `Neo.ClientError.Statement.SemanticError` |
   | `Internal`, `Storage`, `Io`, `Locked` | `Neo.DatabaseError.General.UnknownError`, message `internal error` |

2. **The [ADR-0260](./0260-trust-boundary-error-sanitization.md) trust boundary stays.** The four internal-class
   variants still reach the client only as the fixed message
   `internal error`, and the server still writes the full error to its
   stderr. Only their code changes.

3. **Server-side conditions get codes too.**
 - An [ADR-0177](./0177-bolt-parameter-conversion.md) parameter conversion failure is
     `Neo.ClientError.Request.Invalid`, with the same path-qualified
     message.
 - The [ADR-0110](./0110-bolt-connection-query-governance.md) `--max-result-bytes` cap
     (`query result exceeds max-result-bytes limit of <N> bytes`) is
     `Neo.ClientError.General.TransactionOutOfMemoryError`.
   - A failed HELLO stays `Neo.ClientError.Security.Unauthorized`
 ([ADR-0126](./0126-bolt-basic-auth.md)).
   - The protocol-level FAILUREs decided for the same feature use
     `Neo.ClientError.Request.Invalid` too: a HELLO without `user_agent`
 ([ADR-0313](./0313-bolt-hello-requires-user-agent.md)), a protocol breach ([ADR-0314](./0314-bolt-protocol-breaches-close.md)) and a PULL or DISCARD whose
 `qid` names no open result ([ADR-0311](./0311-bolt-tx-open-results-by-qid.md)). A message that is not a Bolt
 4.4 request gets no FAILURE; the connection closes ([ADR-0315](./0315-bolt-unknown-message-signature-fatal.md)).

4. **Memory errors are non-retryable.** Both memory conditions use
   `Neo.ClientError.General.TransactionOutOfMemoryError`, the only Neo4j
   memory code in the `ClientError` class. Its Neo4j meaning is a
   transaction that used more memory than allowed, which fits a write that
   grows the store past `--memory-limit` and a result over the byte cap.
   The other Neo4j memory codes (`General.TransactionMemoryLimit`,
   `General.OutOfMemoryError`, `General.MemoryPoolOutOfMemoryError`) are
   `TransientError`s, and drivers retry those. A retry cannot free memory
   in an embedded store, so it would only repeat the failure. The two
   conditions share the code and differ by message.

5. **A write-write conflict is retryable.** `Conflict` maps to the
   `TransientError` `Neo.TransientError.Transaction.Outdated`, so drivers'
   managed transactions retry it. The engine still never retries
 ([ADR-0017](./0017-client-conflict.md)).

6. **Three typed engine variants replace message matching.** Each keeps
   the `Display` text of the error it replaces byte for byte, and each is
   public API on `cypherlite::Error`:
   - `Error::Arithmetic(ArithmeticFailure)`. `ArithmeticFailure` is
     `DivisionByZero` (`type error: / by zero`), `ModuloByZero`
     (`type error: % by zero`) or `RoundingNecessary`
     (`ArithmeticError: rounding necessary`). The executor raises it
     where it raised the `Type`/`Execution` strings.
     `ArithmeticFailure::detail()` returns the payload the old variant
     carried (`/ by zero`, …).
   - `Error::DatabaseNotFound { name }`, displayed
     `semantic error: DatabaseNotFound: Graph not found: <name>`. It is
     returned by `ce_messages::graph_not_found` (an unknown static `USE`
     target) and so by `ce_messages::database_does_not_exist`, which
 delegates to it ([ADR-0146](./0146-unknown-namespace-selection-is-graph-not-found.md)). Live Neo4j sends
     `Neo.ClientError.Database.DatabaseNotFound` for both paths.
     `Error::typed_detail` carries it like the other typed detail
 variants. The `ALTER DATABASE <unknown>` text ([ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md) §2) has a
     different message and stays `Semantic`.
   - `Error::Cancelled`, displayed `query cancelled`. `Error::cancelled()`
     returns it. Before, it was `Error::Execution("query cancelled")`.

   The bindings' metadata (`binding_error_metadata`), the CLI error
 envelope ([ADR-0194](./0194-cli-error-envelope-and-exit-codes.md)) and the TCK runtime classify the new variants
   exactly as they classified the old ones. So Python, the C ABI, HTTP,
   MCP, the CLI and the TCK do not see a change.

7. **The constants live in `failures.rs`.** Each code is one
   `pub(crate) const` beside the mapping. `REQUEST_INVALID_CODE` sits
   directly after `UNAUTHORIZED_CODE`; the protocol-semantics decisions of
 this feature ([ADR-0311](./0311-bolt-tx-open-results-by-qid.md), [ADR-0313](./0313-bolt-hello-requires-user-agent.md), [ADR-0314](./0314-bolt-protocol-breaches-close.md)) use the same constant.
8. **Where the new variants are reported.** [ADR-0319](./0319-bolt-execution-failures-at-pull.md) reports execution
   failures at the first PULL or DISCARD, classified by `fails_run` in
   `surfaces/cypherlite-bolt/src/results.rs`. `DatabaseNotFound` is
   compile-class there, like `ParameterMissing`, and fails the RUN;
   `Arithmetic` and `Cancelled` are execution failures, reported at the
   PULL or DISCARD. The code does not depend on where it is reported.

Every code was checked against Neo4j's status-code list for 2026.09
(<https://neo4j.com/docs/status-codes/current/errors/all-errors/>) and
`Status.java` in neo4j/neo4j (`community/common`).
`TransactionTimedOut` is a `ClientError`, `Outdated` a `TransientError`,
and `UnknownError` a `DatabaseError`.

## Alternatives considered

- **Keep `CypherLite.Error` ([ADR-0260](./0260-trust-boundary-error-sanitization.md) item 3).** Rejected: drivers
  classify by code. neo4rs reports every CypherLite failure as a database
  fault, the Python driver raises no syntax error, and managed
  transactions never retry a conflict. Matching Neo4j at Bolt 4.4 is the
 goal, and [ADR-0057](./0057-embedded-c-abi-and-bolt.md)'s "transport, not parity" does not
  require inventing codes no driver understands.
- **Map the message text (`"/ by zero"`, `"query cancelled"`,
  `"DatabaseNotFound:"`) at the Bolt layer.** Rejected: AGENTS.md forbids
  shape-sniffing. A typed variant is one match arm, and a wording change
  could not silently move a code.
- **Add a `Cancelled` check (`e == Error::cancelled()`) instead of a
  variant.** Rejected: it compares the message string under another
  name. A unit variant is typed, costs nothing, and keeps the `Display`
  text.
- **One arithmetic variant per failure on `Error`.** Rejected: three
  top-level variants for one Neo4j code. A single `Arithmetic` variant
  with a small enum keeps the mapping to one arm and still keeps each
  `Display` text.
- **Use Neo4j's transient memory codes (`General.TransactionMemoryLimit`,
  `General.OutOfMemoryError`).** Rejected: drivers retry `TransientError`s,
  and retrying cannot free memory in an embedded store.
- **`Neo.TransientError.Transaction.DeadlockDetected` or a `ClientError`
  for write-write conflicts.** Rejected: `Outdated` is what Neo4j sends
  when a transaction saw state another commit invalidated. That is what
  `Conflict` means. A `ClientError` would stop drivers retrying.
- **Send the engine's internal-class detail now that the code is a Neo4j
 code.** Rejected: [ADR-0260](./0260-trust-boundary-error-sanitization.md)'s reason (host paths, panic payloads on an
  untrusted transport) is independent of the code.
- **Rewrite messages to Neo4j's wording.** Rejected: drivers branch on the
  code, not the text. CypherLite's messages are pinned by its own tests,
 docs and the CE-text fixture ([ADR-0146](./0146-unknown-namespace-selection-is-graph-not-found.md)).

## Consequences

- Drivers raise the exception type Neo4j would cause:
  `CypherSyntaxError`, `ClientError` for a missing database, a
  retryable `TransientError` for a conflict. neo4rs scenarios D-46, D-47,
  D-50 and D-51 match Neo4j.
- `cypherlite::Error` gains `Arithmetic`, `DatabaseNotFound` and
  `Cancelled`, and `cypherlite::ArithmeticFailure` is exported. Code that
  matched `Error::Execution` for a cancelled query, `Error::Type` for
  division by zero, or `Error::Semantic` for an unknown database matches
  the new variant. `docs/api/rust/spec.md` documents the variants. The
  rendered text is unchanged, so the C ABI, Python, JS, Java, Go, HTTP and
  MCP error messages are unchanged too.
- `docs/api/bolt/spec.md` lists the code table above. The
  `docs/api/bolt/examples` scripts that assert a code assert the new
  code.
- [ADR-0260](./0260-trust-boundary-error-sanitization.md) items 1–3 are superseded for the code (item 1's
 genericization stays). [ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md) §9, [ADR-0117](./0117-namespace-selection-apis-and-bolt.md) §7, [ADR-0138](./0138-bolt-procedure-not-found-code.md)'s "no other
 `Neo.*` code", [ADR-0146](./0146-unknown-namespace-selection-is-graph-not-found.md)'s Bolt code and [ADR-0177](./0177-bolt-parameter-conversion.md) item 6's code are
 superseded the same way. [ADR-0174](./0174-committed-catalog-constraint-rechecks.md) item 3 names the old cancellation
  variant. Each gains a **Later references** note.
- Tests pin every row of the table (`failures_tests.rs`), the wire codes
  through `handle_conn`, and the `.bolt` script
  `tests/compat/scripts/failures/neo4j-status-codes.bolt` (syntax error,
  unknown `db` extra, a Node struct parameter).

## Later references

- `bolt-compat-tests/fix-bolt-polish` (round-2 review): the classification
  is now `error_class` alone. The second enum `EngineFailure` and the
  `failure_for_engine_error` wrapper are deleted; `send_engine_failure`
  matches `error_class(e)` directly (caller-facing classes send the code
  and `e.to_string()`, `Internal` logs and sends
  `UNKNOWN_ERROR_CODE`/`INTERNAL_FAILURE_MESSAGE`), and the failure tests
  assert on `error_class`. The variant table above is unchanged.
