---
type: adr
area: surface
date: 2026-09-29
deciders: CypherLite maintainers
name: 0140-bolt-route-advertises-dialed-address
status: accepted
tags:
  - bolt
title: Bolt ROUTE advertises the dialed address
REFERENCES:
  - "[[adr/0126-bolt-basic-auth]]"
  - "[[adr/0128-docker-bolt-server-image]]"
  - "[[adr/0269-bolt-route-self-routing-table]]"
SUPERSEDES:
  - "[[adr/0269-bolt-route-self-routing-table]]"
---

# 0140. Bolt ROUTE advertises the dialed address

## Context

[ADR-0269](./0269-bolt-route-self-routing-table.md) settled that ROUTE (`0x66`) answers with a single-server self
routing table, and chose the `<self>` address as the listener's resolved bind
address, falling back to the connection's local socket address only when the
bind is unspecified (`0.0.0.0`/`::`).

That rule breaks behind Docker port mapping and NAT. The shipped image
([ADR-0128](./0128-docker-bolt-server-image.md)) binds `0.0.0.0:7687` inside the container, so the bind is
unspecified and the fallback fires: the connection's local address is the
**container-internal** IP (e.g. `172.17.0.2:7687`). A driver on the host
dialed `localhost:7687` (published port), receives `172.17.0.2:7687` in the
routing table, and cannot reach it — every subsequent RUN fails. The rule
also **discloses the internal address** to clients, which is information a
server should not volunteer.

The address the client dialed — the routing-context `address` the driver
sends in ROUTE — is reachable from that client by construction: it is the
address this very connection arrived on.

This decision supersedes [ADR-0269](./0269-bolt-route-self-routing-table.md)'s `<self>` derivation; it was settled by
the maintainer before this ADR was written.

## Decision

ROUTE (`0x66`) fills `rt.servers[*].addresses` with, in order:

1. **The routing-context `address` the client sent**, when present and
   non-empty. That address reached this server for this client, so it is
   routable for this client.
2. Otherwise, **the connection's local socket address** — the fallback for
   clients that send an empty or absent routing context (some non-browser
   drivers do).

Everything else in [ADR-0269](./0269-bolt-route-self-routing-table.md) stands: the three roles (`ROUTE`/`READ`/`WRITE`),
ttl 300, the `db` echo, and ROUTE being answered regardless of failed state
behind the [ADR-0126](./0126-bolt-basic-auth.md) auth gate.

## Alternatives considered

- **Keep the [ADR-0269](./0269-bolt-route-self-routing-table.md) rule (resolved bind, else connection-local).**
  Rejected: behind Docker port mapping or NAT it hands the client an
  unroutable container-internal address, breaking every routed session, and
  it leaks the internal address to clients.
- **An `--advertised-address` flag.** Deferred: an operator-set advertised
  address is the right answer for deployments where even the dialed address
  is not the desired long-term one (multi-homed hosts, proxies that rewrite
  destinations), but no such deployment need is in front of us; it is
  recorded here as the future step, not built now.
- **Always echo the routing-context address, with no fallback.** Rejected:
  non-browser clients may send an empty or absent routing context, and they
  still need a usable table; the connection-local address covers them.

## Consequences

- The single-server scope is unchanged ([ADR-0269](./0269-bolt-route-self-routing-table.md)): one server, three roles,
  every RUN routed back to the connection the driver already has.
- Clients receive the address they dialed, which is reachable for them by
  construction; Docker and NAT deployments work without operator
  configuration, and the container-internal address is no longer disclosed.
- A client that dialed a DNS name gets that DNS name back; one that dialed
  an IP gets the IP. Neither is resolved or rewritten.
- `--advertised-address` is the named follow-up if a deployment needs an
  operator-set address; adding it will be a new ADR refining this one.
- Tests must pin the dialed-address-first rule, the local-address fallback
 for empty/absent routing contexts, and the [ADR-0269](./0269-bolt-route-self-routing-table.md) invariants (roles,
  ttl, db echo).
- Supersedes the `<self>` address derivation of [ADR-0269](./0269-bolt-route-self-routing-table.md); [ADR-0269](./0269-bolt-route-self-routing-table.md) gains a
  **Later references** note pointing here.
