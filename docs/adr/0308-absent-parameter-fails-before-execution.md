---
type: adr
area: cypher
claim: db7790e1780b44e27c13ebd8416efe7c3573861a
name: 0308-absent-parameter-fails-before-execution
status: proposed
tags:
  - conformance
  - errors
title: Statements that reference an absent parameter fail with ParameterMissing before execution
REFERENCES:
  - "[[adr/0028-user-visible-transactions]]"
  - "[[adr/0307-bolt-failure-carries-neo4j-status-codes]]"
---

# ADR-0308: Statements that reference an absent parameter fail with ParameterMissing before execution

## Context

`RETURN $p AS p` with no `p` in the supplied parameters returns one row
with `null` on every surface: the Rust API, the CLI, Python, the C ABI and
its JS/Java/Go clients, WASM, HTTP, MCP and Bolt. Neo4j fails the
statement before it runs, with `Neo.ClientError.Statement.ParameterMissing`
and the message `Expected parameter(s): p`. The probes record the
divergence (`bolt-compat-findings.json` F-42 and F-67,
`neo4rs-compat-findings.json` D-48). A driver gets a wrong answer instead
of an error. A misspelled parameter name in a write, such as
`CREATE (:User {email: $emial})`, stores `null` without a word.

The cause is one silent fallback in the executor's expression evaluator
(`cypherlite-executor/src/eval/mod.rs`):
`Expr::Parameter(n) => Ok(params.get(n).cloned().unwrap_or(Value::Null))`.
AGENTS.md bans exactly this `unwrap_or(Value::Null)`. The engine already
has the typed `Error::ParameterMissing` and its TCK class (`MissingParameter`,
Call1 [11]). The procedure-argument path (`CALL p` with implicit
arguments) already raises it. Plain parameter references do not.

The probe for this decision runs on every surface, so this is the
engine's policy, not a Bolt-only rule.

## Decision

A statement that references a parameter missing from the supplied
parameters fails before execution with `Error::ParameterMissing`.

1. **Where.** `Connection::run_query` collects the parameter names the
   planned statement references (`cypherlite_planner::plan_parameter_names`,
   a walk of every clause and expression of the `LogicalPlan`, `NEXT`
   stages, subqueries and pattern payloads included). The check runs after
   planning and the compile-time gates, after the EXPLAIN early return,
   and before dispatch. So `EXPLAIN RETURN $p` still plans without values,
   as in Neo4j, and `PROFILE` checks like a normal run.
2. **What.** The error is
   `Error::ParameterMissing("Expected parameter(s): <names>")`, with each
   missing name once, in the order of first appearance in that walk,
   joined by `, `. It renders
   `semantic error: ParameterMissing: Expected parameter(s): p`.
   `Error::parameters_missing(names)` in `cypherlite-core` builds it. It
   is the one spelling of the message.
3. **Nothing executes.** No row, no write and no procedure call happens.
   In an explicit transaction the failure fails the transaction like any
 other statement error ([ADR-0028](./0028-user-visible-transactions.md)), so it needs a rollback.
4. **The executor fails too.** The `Expr::Parameter` arm returns the same
   `parameters_missing` error instead of `null`. The plan walk covers
   every `Expr::Parameter` the plan holds, so the run-time arm only fires
   for expressions the walk cannot see: the parser-level expressions that
   catalog commands (`SHOW SETTINGS $p`, `OPTIONS {…}` maps) lower when
   they run. The catalog commands that read a parameter by name — a schema
   name (`CREATE INDEX $name …`), `OPTIONS $map`, `SHOW DATABASE $p` —
   return the same error for an absent parameter instead of a type error
   or, for `SHOW DATABASE`, no rows.
5. **Unchanged.** The implicit-procedure-arguments path keeps its
   `MissingParameter: implicit args for <name>` error and TCK Call1 [11].
   Path-selector counts (`SHORTEST $k`) keep their existing
   `ParameterMissing` check. A parameter that is supplied with the value
   `null` is still `null`, including the `null`-argument rules of
   `SHOW SETTINGS` and `SHOW DATABASE` under `CYPHER 5` and `CYPHER 25`.
6. **Every surface.** The engine is the one policy, so the CLI, Python
   (`ProgrammingError`, class `ParameterMissing`), the C ABI and its
   clients, WASM, HTTP (`42N02`), MCP and Bolt
 (`Neo.ClientError.Statement.ParameterMissing`, [ADR-0307](./0307-bolt-failure-carries-neo4j-status-codes.md)) all report it
   with no surface change.

## Alternatives considered

- **Keep `null` substitution and document it.** Rejected: it is the
  silent fallback AGENTS.md bans, it diverges from Neo4j on every driver,
  and it turns a typo into stored `null`s.
- **Check only at the Bolt RUN.** Rejected: the same statement through the
  CLI, Python or HTTP would still return `null`. The engine owns
  parameter binding, and per-surface checks would drift.
- **Fail only in the evaluator (lazily).** Rejected: a reference that no
  row reaches (`MATCH (n:Missing) RETURN $p`) would still succeed, and a
  write could run half its rows before failing. Neo4j checks before
  execution.
- **Check before EXPLAIN too.** Rejected: Neo4j plans `EXPLAIN` without
  parameter values, and tools send EXPLAIN to validate a query whose
  parameters they do not have yet.
- **Collect names from the parser AST instead of the plan.** Rejected: the
  check would need a second walker over a second tree. The planner IR
  already has the exhaustive clause/expression walk (`walk_clauses`)
  every other plan analysis uses.
- **Turn the evaluator arm into a panic (`expect`).** Rejected: catalog
  commands evaluate expressions the plan walk does not hold, so the arm is
  reachable. A typed error is the honest result there.

## Consequences

- `RETURN $p` without `p` fails on every surface with `ParameterMissing`;
  with `p` supplied, even as `null`, it behaves as before.
- Callers that relied on `null` substitution must pass the parameter. The
  Python test that pinned `null` now pins `ProgrammingError`, the CLI test
  pins exit 6, the engine tests that used an absent `$x` as a `null`
  argument pass `x: null` explicitly, and `docs/api` plus the skill
  bundles say that a referenced parameter must be supplied.
- The TCK (3892 of 3892), the CIP-eval scoreboard and the cookbook supply
  every parameter they reference, so their gates do not move.
- Tests: `cypherlite/cypherlite/tests/parameter_missing.rs` pins the
  engine behavior (message, name order, EXPLAIN, transactions, `null`
  value, no write), the planner pins the walk, and the Bolt script
  `tests/compat/scripts/failures/parameter-missing.bolt` plus a
  `failures_tests.rs` unit test pin the wire.
