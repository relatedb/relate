# 0281. Bolt session and PackStream are callable as a library

- **Status**: Accepted
- **Renumbered**: from 0159 (shared-number cleanup)
- **Date**: 2026-10-04
- **Deciders**: CypherLite maintainers

## Context

`cypherlite-bolt` is the standalone Bolt server: one bind, a first-byte
peek, WebSocket or raw Bolt, and a preset basic-auth password
(ADR-0126, ADR-0266). `handle_conn` runs the session against an
embedded `Database`, but it is `pub(crate)` inside the binary.
PackStream encode and decode live in the same binary.

A client implementing embedded Relate needs that session in-process,
where the database already runs, and needs PackStream so it can read
HELLO, answer ROUTE, and relay the other messages. The client must not
reimplement the session.

## Decision

`cypherlite-bolt` keeps its library target and exports the session
server and PackStream:

- `handle_conn`, `read_message`, `ConnConfig`, `ConnLimits`
- `AuthConfig`, including `AuthConfig::basic`
- `Pack`, `decode`, `encode`, `try_struct_msg`, `chunk_message`,
  `routing_table`

The standalone listener, `--auth`, the generated token, and
WebSocket-on-the-same-bind stay in the binary. Exporting the session
does not add a second public protocol and does not change the wire
behavior of `cypherlite-bolt`.

## Alternatives considered

- **Copy the session into the client.** Rejected. Two Bolt
  implementations drift, and the engine tests would no longer cover
  the code the client runs.
- **Make the client shell out to the `cypherlite-bolt` binary.**
  Rejected. A client implementing embedded Relate already embeds
  `Database`. A second process would open the same store.

## Consequences

- A client implementing embedded Relate links this crate and calls
  `handle_conn` on a stream it accepts itself.
- The binary and its tests call the same items through the library.
- No CLI flag changes. `docs/api/bolt/` for the standalone server is
  unchanged.
