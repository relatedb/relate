---
type: adr
area: surface
date: 2026-09-29
deciders: CypherLite maintainers
name: 0138-bolt-procedure-not-found-code
status: accepted
tags:
  - bolt
  - errors
title: ProcedureNotFound carries the Neo4j FAILURE code at the Bolt boundary
REFERENCES:
  - "[[adr/0260-trust-boundary-error-sanitization]]"
  - "[[adr/0266-bolt-websocket-transport]]"
  - "[[adr/0307-bolt-failure-carries-neo4j-status-codes]]"
---

# 0138. ProcedureNotFound carries the Neo4j FAILURE code at the Bolt boundary

## Context

[ADR-0260](./0260-trust-boundary-error-sanitization.md) pins the Bolt FAILURE codes: internal-class errors genericize to
`CypherLite.InternalError`, and every caller-facing error passes through
verbatim under `CypherLite.Error`, deliberately **not** claiming Neo4j's
`Neo.*` status-code taxonomy. neo4j-browser, however, branches on exact
Neo4j codes: at minimum `Neo.ClientError.Procedure.ProcedureNotFound` gates
its `dbms.clientConfig` → `dbms.listConfig` fallback during the connect
sequence. With every engine error shipped as `CypherLite.Error`, that branch
never fires and the browser connect dies. [ADR-0266](./0266-bolt-websocket-transport.md) item 4 settles that this
Bolt-layer gap is fixed in the WebSocket feature branch under its own ADR —
this is that record.

The engine raises a call to an unregistered procedure as
`Error::Syntax("ProcedureNotFound: <name>")`
(`cypherlite/cypherlite-executor/src/exec/procedures.rs`,
`cypherlite/cypherlite/src/procedures.rs`), displayed as
`syntax error: ProcedureNotFound: <name>`. No typed variant crosses the
public `cypherlite::Error` enum, and the `cypherlite/` engine crates are out
of scope for this change.

## Decision

At the Bolt trust boundary, **a `Syntax` engine error whose message starts
with `ProcedureNotFound: ` is sent verbatim under the code
`Neo.ClientError.Procedure.ProcedureNotFound`** instead of
`CypherLite.Error`. The Bolt layer detects it by the pinned message prefix;
the message text is unchanged.

[ADR-0260](./0260-trust-boundary-error-sanitization.md)'s partitioning is otherwise intact:

- internal-class errors (`Internal`, `Storage`, `Io`, `Locked`) still
  genericize to `CypherLite.InternalError` with the fixed message
  `internal error`;
- every other caller-facing error keeps `CypherLite.Error`;
- the scope of this revision is **ProcedureNotFound only** — no other
  `Neo.*` code is emitted.

## Alternatives considered

- **Add a typed `ProcedureNotFound` variant to `cypherlite_core::Error`.**
  Rejected: an engine-crate change to serve one Bolt client's
  string-matching; the engine's message is already pinned by test, so the
  Bolt layer can detect the condition without widening the public engine
  error enum.
- **Map the full Neo4j status-code taxonomy.** Rejected: [ADR-0260](./0260-trust-boundary-error-sanitization.md) already
  decided CypherLite does not claim Neo4j parity, and one precise code fixes
  the observed browser break; a broad mapping would imply a compatibility
  surface CypherLite does not implement.
- **Leave the code as `CypherLite.Error`.** Rejected: neo4j-browser keys its
  `dbms.clientConfig` → `dbms.listConfig` fallback on the exact
  `Neo.ClientError.Procedure.ProcedureNotFound` code, so the connect
  sequence stays broken.

## Consequences

- `docs/api/bolt/spec.md`'s error-partitioning paragraph names the single
  `Neo.*` exception and its scope.
- [ADR-0260](./0260-trust-boundary-error-sanitization.md) gains a **Later references** note pointing here; its other
  decisions are unchanged.
- Drivers keying on `CypherLite.Error` for syntax errors see one fewer
  syntax-error shape; drivers keying on the Neo4j code (neo4j-browser) now
  take their intended fallback path.
- Tests pin the mapping, its prefix precision (a near-miss
  `ProcedureNotFound:` message without the trailing space and name is not
  mapped), and the unchanged behavior of ordinary syntax and internal-class
  errors.

## Later references

- [ADR-0307](./0307-bolt-failure-carries-neo4j-status-codes.md) keeps this ADR's code for the typed `ProcedureNotFound` variant
  but reverses "no other `Neo.*` code is emitted": every Bolt FAILURE now
  carries a Neo4j status code, and an ordinary syntax error is
  `Neo.ClientError.Statement.SyntaxError` instead of `CypherLite.Error`.
