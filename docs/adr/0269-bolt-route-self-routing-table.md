---
type: adr
area: surface
date: 2026-09-29
deciders: CypherLite maintainers
name: 0269-bolt-route-self-routing-table
status: accepted
tags:
  - bolt
title: Bolt ROUTE answers with a single-server self routing table
REFERENCES:
  - "[[adr/0126-bolt-basic-auth]]"
  - "[[adr/0140-bolt-route-advertises-dialed-address]]"
  - "[[adr/0266-bolt-websocket-transport]]"
  - "[[adr/0318-bolt-route-home-db-and-role-order]]"
---

# 0269. Bolt ROUTE answers with a single-server self routing table

- **Renumbered**: from 0137 (shared-number cleanup)

## Context

neo4j-browser's default scheme for a bare host is `neo4j://`, whose driver
sends `ROUTE {address: "host:port"} [] {db: "system"}` (message tag `0x66`)
after HELLO and before any session opens, to fetch a routing table. The
server's message-tag match in `surfaces/cypherlite-bolt/src/protocol.rs` had
no `0x66` arm, so the connection died on
`FAILURE 'unknown message tag 0x66'` and no browser client could connect.
[ADR-0266](./0266-bolt-websocket-transport.md) item 4 settles that this Bolt-layer gap is fixed in the WebSocket
feature branch under its own ADR — this is that record.

CypherLite is a single embedded server: there is no cluster, no second
server to route to, and no multi-database routing. The only truthful routing
table is one that points every role back at the server itself.

## Decision

`protocol.rs` handles ROUTE (`0x66`) with a **single-server self routing
table**. The response is `SUCCESS` with metadata:

```
{rt: {ttl: 300,
      servers: [{role: "ROUTE", addresses: [<self>]},
                {role: "READ",  addresses: [<self>]},
                {role: "WRITE", addresses: [<self>]},
      db: <requested db or "system">}}
```

- **ttl** is a fixed **300 seconds**. There is no topology to age out; 300
  matches the served idle-timeout default and keeps drivers from
  re-requesting the table on every transaction.
- **`<self>`** is the server's own bind address: the listener's local
  address after `--bind` resolves. When the bind is unspecified
  (`0.0.0.0`/`::`), that address is not advertiseable, so each connection's
  own local address is used instead, resolved in `tls.rs` `serve_conn` while
  the socket is still a `TcpStream` (`handle_conn` is generic over the
  stream and cannot inspect it). A `handle_conn` caller that never went
  through `serve_conn` falls back to the `address` the client sent in the
  ROUTE routing context.
- **db** echoes the requested `db` field, defaulting to `"system"`; the
  engine is single-database, so the value is informational only.
- ROUTE is answered regardless of the connection's failed state (it is
 session management, like RESET, not query work). The [ADR-0126](./0126-bolt-basic-auth.md) auth gate is
  unchanged: with auth configured, a validated HELLO still comes first —
  the browser driver's handshake → HELLO → ROUTE order already satisfies it.

## Alternatives considered

- **Keep rejecting ROUTE as an unknown tag.** Rejected: it is the status quo
  that kills the neo4j-browser connect sequence before any session opens;
 the whole point of [ADR-0266](./0266-bolt-websocket-transport.md)'s browser work is unreachable without it.
- **FAILURE with a "routing unsupported" hint so the driver falls back to
  direct mode.** Rejected: the official drivers treat a ROUTE FAILURE as a
  hard error on `neo4j://` and do not silently downgrade to `bolt://`;
  users would have to know to change the scheme.
- **Echo the requested `address` as the self address unconditionally.**
  Rejected: the client-supplied string can be a DNS name, a container alias,
  or a stale port; the listener's own local address is authoritative, with
  the requested address kept only as the no-socket fallback.
- **ttl of 0 (always re-fetch) or a multi-hour ttl.** Rejected: 0 makes
  every transaction re-route for no topology gain; a very long ttl strands a
  driver on a dead cached table after a restart on a new port. 300 seconds
  is the middle ground Neo4j's own defaults inhabit.

## Consequences

- `docs/api/bolt/spec.md` gains a ROUTE row in the message table and drops
  `routing (ROUTE)` from the not-supported list.
- A driver that believes the table sends every subsequent RUN — read or
  write — to this same server, which is correct: there is exactly one
  server and it does both.
- The routing table claims no capability CypherLite lacks: it advertises
  addresses, not cluster features. Multi-database `db` values are echoed,
  not honored.
- Tests pin the exact browser message shape, the ttl value, the three
  roles, and the self-address derivation.

## Later references

- [ADR-0140](./0140-bolt-route-advertises-dialed-address.md) supersedes the `<self>` address derivation: ROUTE advertises the
  routing-context `address` the client dialed when present and non-empty,
  falling back to the connection's local socket address otherwise, because
  the bind/local-address rule breaks behind Docker port mapping and NAT. The
  roles, ttl, and `db` echo settled here are unchanged.
- [ADR-0318](./0318-bolt-route-home-db-and-role-order.md) reverses the `db` default and the role order: a ROUTE whose
  extra names no database answers the home database `neo4j` (not
  `system`), and the servers list the roles WRITE, READ, ROUTE, as latest
  Neo4j does.
