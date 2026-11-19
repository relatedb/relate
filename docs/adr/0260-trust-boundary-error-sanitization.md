---
type: adr
area: surface
date: 2026-09-26
deciders: CypherLite maintainers
name: 0260-trust-boundary-error-sanitization
status: accepted
tags:
  - errors
  - security
title: Sanitize internal-class errors at the Bolt and load trust boundaries
REFERENCES:
  - "[[adr/0057-embedded-c-abi-and-bolt]]"
  - "[[adr/0080-cli-pull-command]]"
  - "[[adr/0097-exclusive-storage-ownership]]"
  - "[[adr/0122-pull-auth-hardening]]"
  - "[[adr/0138-bolt-procedure-not-found-code]]"
  - "[[adr/0307-bolt-failure-carries-neo4j-status-codes]]"
---

# 0260. Sanitize internal-class errors at the Bolt and load trust boundaries

- **Renumbered**: from 0109 (shared-number cleanup)

## Context

`cypherlite_core::Error` (re-exported as `cypherlite::Error`) carries a
`String` message on every variant. Two surfaces currently forward those
messages to a remote or unprivileged party unchanged.

**Bolt.** Every `FAILURE` sent by the server interpolates `e.to_string()` at
`surfaces/cypherlite-bolt/src/protocol.rs:109, 157, 171, 185`, and all of
them share one pinned code constant `CypherLite.Error`
(`protocol.rs:242`). The `Internal` variant is where this hurts: a caught
panic payload becomes `query worker thread panicked: <payload>` via
`Error::internal(...)` in `cypherlite/cypherlite/src/connection.rs:77-80`.
That message can describe host paths, memory layout, or other engine
internals to any connected client. `Storage`, `Io`, and `Locked` likewise
name filesystem state (`storage locked: <path>`) that is operator
information, not client information; `Storage`/`Locked` exist to enforce
exclusive storage ownership ([ADR-0097](./0097-exclusive-storage-ownership.md)), an operator-only concern that no
remote client can act on. Bolt is a network transport ([ADR-0057](./0057-embedded-c-abi-and-bolt.md)),
so its bytes cross a trust boundary.

**cypherlite-load / `pull`.** `query_api_url` formats the whole `NEO4J_URI`
with `{uri:?}` when the host or scheme is missing
(`surfaces/cypherlite-load/src/http.rs:23, 26`). A URI such as
`user:pass@host` with no scheme therefore prints the password to stderr
through `surfaces/cypherlite-cli/src/pull_cmd.rs:80`. `pull` is the CLI face
of the [ADR-0080](./0080-cli-pull-command.md) Query API v2 loader.

The engine's own errors are grouped as `Syntax`, `Semantic`, `Type`,
`Execution`, `Conflict`, `Capacity`, `Storage`, `Locked`, `Io`, `Internal`.
The first six describe the caller's query and are safe to echo; the last
four describe the engine or host and are not.

## Decision

**Internal-class engine errors are genericized at both trust boundaries;
caller-facing errors keep their text.**

1. **Bolt genericizes `Internal`, `Storage`, `Io`, `Locked`.** A `FAILURE`
   for one of these variants carries the fixed client message
   `internal error` and the status code `CypherLite.InternalError`. The
   original `e.to_string()` is written to the server's own stderr
   (`eprintln!`) so operators keep full detail. No path, errno, panic
   payload, or OS text reaches the client.

2. **Bolt passes every other variant through verbatim.** `Syntax`,
   `Semantic`, `Type`, `Execution`, `Conflict`, and `Capacity` keep their
   own message under the existing pinned code `CypherLite.Error`. The
   `write-write conflict` (Conflict) and `capacity exceeded: …` (Capacity)
   messages remain exactly as pinned in `docs/api/bolt/spec.md`, and the
   unknown-tag `FAILURE` is unchanged.

3. **The generic code uses the `CypherLite` namespace, not `Neo.*`.** Bolt
 is a transport ([ADR-0057](./0057-embedded-c-abi-and-bolt.md)), not Neo4j feature parity; CypherLite does not
   claim Neo4j's status-code taxonomy. The genericized code is
   `CypherLite.InternalError`, consistent with the existing
   `CypherLite.Error`.

4. **cypherlite-load strips userinfo before formatting URI errors.** The
   missing-host and missing-scheme messages in `query_api_url` redact the
   `user:pass@` userinfo component before the URI is interpolated, so no
   credential reaches stderr. The rest of the message and the parse
   behavior are unchanged.

This is a boundary-hygiene change only: the engine still produces typed
errors with full messages, and server-side diagnostics are not reduced.

## Alternatives considered

- **Genericize every engine error over Bolt.** Rejected: `Syntax`,
  `Semantic`, `Type`, `Execution`, `Conflict`, and `Capacity` deliberately
  describe the caller's own query so drivers and users can act on them;
  blanking them would break the documented Bolt contract and the pinned
  `write-write conflict` / `capacity exceeded` messages.
- **Log nothing and send the full detail to the client.** Rejected: it is
  the status quo that leaks panic payloads and host paths across the trust
 boundary; [ADR-0057](./0057-embedded-c-abi-and-bolt.md) makes Bolt remote transport, so its messages must be
  safe for an untrusted peer.
- **Use a `Neo.ClientError.*` / `Neo.DatabaseError.*` code for the generic
  case.** Rejected: CypherLite is not Neo4j and does not implement Neo4j's
  status taxonomy; claiming a `Neo.*` code would imply parity it does not
  have, whereas `CypherLite.InternalError` sits beside the existing
  `CypherLite.Error`.
- **Redact only `Internal` and leave `Storage`, `Io`, `Locked` verbatim.**
  Rejected: those three expose the same class of host-side detail (locked
  paths, filesystem errors) that a remote client has no legitimate need
  for, and grouping them under one generic message keeps the rule simple.
- **Strip userinfo only in the CLI instead of the loader.** Rejected: the
  URI is already formatted inside `cypherlite-load`, and other callers of
  `query_api_url` would keep leaking; the redaction belongs where the error
  message is built.

## Consequences

- Bolt `FAILURE` messages are now partitioned into caller-facing errors
  (`CypherLite.Error`, verbatim) and internal-class errors
  (`CypherLite.InternalError`, message `internal error`). `docs/api/bolt/spec.md`
  must list both codes and the generic message.
- Operators retain full detail for internal failures through the server's
  stderr; the change removes information from the client only, not from the
  server.
- The existing pinned `CypherLite.Error` code still covers every
  caller-facing variant, so drivers keying on `CypherLite.Error` for those
  errors are unaffected; only `Internal`/`Storage`/`Io`/`Locked` move to
  the new code.
- `cypherlite-load` no longer prints credentials when a `NEO4J_URI` is
  missing its host or scheme; `docs/api/cli/spec.md` should note the redacted
 error. [ADR-0080](./0080-cli-pull-command.md)'s `pull` command behavior is otherwise unchanged.
- Tests must cover each genericized variant mapping to
  `CypherLite.InternalError` + `internal error`, each pass-through variant
  mapping to `CypherLite.Error`, the stderr log, and a
  `user:pass@host` URI producing a credential-free message.

## Later references

- [ADR-0138](./0138-bolt-procedure-not-found-code.md) revises item 2 for one caller-facing error: a `Syntax` error
  whose message starts with `ProcedureNotFound: ` is sent verbatim under
  `Neo.ClientError.Procedure.ProcedureNotFound` instead of
  `CypherLite.Error`, because neo4j-browser branches on that exact code.
  Every other variant keeps the partitioning pinned here.
- [ADR-0122](./0122-pull-auth-hardening.md) supersedes item 4: `cypherlite-load` no longer strips userinfo
  for error formatting. All `NEO4J_URI` error text is fixed (no URI
  interpolation at all), and any URI carrying userinfo — including passwords
  containing `/` — is rejected outright at the `query_api_url` boundary.
- [ADR-0307](./0307-bolt-failure-carries-neo4j-status-codes.md) supersedes items 2 and 3 and the `CypherLite.InternalError` code
  of item 1: every Bolt FAILURE now carries a Neo4j status code, mapped by
  typed engine variant (`Syntax` → `Neo.ClientError.Statement.SyntaxError`,
  `Conflict` → `Neo.TransientError.Transaction.Outdated`, …), and the
  internal-class variants go out as `Neo.DatabaseError.General.UnknownError`.
  Item 1's genericization stands: those variants still reach the client
  only as `internal error`, with the full error on the server's stderr.
