---
type: adr
area: surface
claim: 27bb915fd20b20f0cf302c9197d7b3327c667af0
name: 0428-embeddable-http-query-api
status: accepted
tags:
  - http
title: The HTTP Query API is embeddable over any byte stream
REFERENCES:
  - "[[adr/0368-one-product-binary-serve-absorbs-bins]]"
  - "[[adr/0435-managed-tx-lifecycle]]"
  - "[[adr/0440-follower-writer-toggle]]"
  - "[[adr/0452-workspace-limit-enables-admission]]"
  - "[[adr/0501-drop-token-schedule]]"
---

# ADR-0428: The HTTP Query API is embeddable over any byte stream

## Context

`cypherlite serve --http` accepts plaintext TCP and runs the Query API in
this process. An embedding host already terminates TLS on its own listener
and needs the same query protocol on the decrypted stream. The handler
today takes a `TcpStream` (`surfaces/cypherlite-http/src/server.rs`),
clones it, and sets a read timeout on that socket. A rustls
`StreamOwned` cannot be cloned, so the host cannot call `serve_handler`
after its handshake.

The v2 value, envelope, and plan builders, and the transaction registry's
operations, are `pub(crate)`. A second process that needs those pieces
reimplements them. The registry also keys a transaction only by id. A
host that authenticates one shared credential for many callers cannot
keep those callers' transactions apart.

`AuthConfig` is `None` or one Basic credential. The host's check is not a
user and password stored in this crate. It still has to yield the string
the transaction registry binds, without that string becoming the engine
principal `dbms.showCurrentUser` reports.

`run_error` maps a retryable `ResourceLimit` to HTTP 429 and every other
engine error, including `Capacity`, to 400. A host that admits work on
peak tokens needs 507 for `Capacity` and 429 for the admission refusal,
with `peakTokens` when the error carries a receipt.

This crate does not terminate TLS. Product `serve` stays plaintext
([ADR-0368](./0368-one-product-binary-serve-absorbs-bins.md)).

## Decision

1. `serve_connection` runs one HTTP/1.1 query connection on any
   `Read + Write` stream. `serve` and `serve_handler` stay the plaintext
   `TcpListener` path and set the read timeout before calling it. The
   crate does not gain a TLS implementation.
2. An embedder calls `serve_connection`. The public surface is
   `HttpConfig`, `HttpExtension`, `Reply`, `TxRegistry::new`,
   `framing` (including `BodyEncoding`), and `query_plan_field`. The
   transaction route handlers and the v2 builders stay in this crate.
   `TxRegistry::with` and `TxRegistry::take` are crate-private.
3. A transaction may record an owner. When it has one, a different owner
   receives the same not-found result as an unknown id. `None` and
   `Basic` record no owner.
4. `AuthConfig::Custom` holds an `Authenticator`. The trait checks a
   bearer token or a HELLO password and, for HTTP, returns the
   transaction owner from that verified credential. A host may instead
   return a header value only when a trusted proxy stripped any
   client-supplied copy and set the header from an identity it
   authenticated. A rejected credential is HTTP 401. An authenticator
   that cannot complete the check is HTTP 502. `None` and `Basic` record
   no owner: they have one principal, or none, and do not isolate
   callers. Bolt's HELLO adapter calls the same trait and treats either
   failure as an unauthorized HELLO. The owner is not
   `Connection::set_principal`.
5. `HttpConfig` may carry an extension invoked for a path that is not a
   query route. The extension names which of its paths skip the
   credential check (a health probe). That answer is ignored on
   `/db/{database}/query/v2` and its transaction suffixes. The query
   routes stay authenticated.
6. `Capacity` is HTTP 507. A `ResourceLimit` named
   `concurrent-peak-token-limit`, and any retryable `ResourceLimit`, is
   HTTP 429. When the error carries a receipt, the error object includes
   `metrics.peakTokens` and `metrics.tokenSchedule`. Other engine errors
   keep their GQLSTATUS codes.

## Alternatives considered

- **TLS inside `cypherlite-http`.** Rejected. The host already terminates
 TLS, and [ADR-0368](./0368-one-product-binary-serve-absorbs-bins.md) left TLS off product `serve`.
- **A pure request-to-response function and a host HTTP framework.**
  Rejected. The framing, compression, and connection policy would fork
  again. The host hands over the decrypted stream.
- **Keying transactions on the Basic user.** Rejected. `Basic` is one
  shared credential. The owner is a separate string the custom
  authenticator returns.
- **Setting `dbms.showCurrentUser` from that owner.** Rejected. The owner
  is an opaque caller id, not an engine user.

## Consequences

- An embedding host accepts TCP, finishes its own TLS handshake, and
  calls `serve_connection` on a blocking thread. One request per
  connection and `Connection: close` are unchanged.
- `cypherlite serve` behavior for `None` and `Basic` is unchanged,
  including unscoped transaction ids.
- `docs/api/http/spec.md` documents `serve_connection`, `AuthConfig::Custom`,
  the extension hook, and the 507/429 mapping.

## Later references

- [ADR-0435](./0435-managed-tx-lifecycle.md)'s managed-transaction lifecycle work retired the crate-private
  hand-off pair named in item 2: `TxRegistry::with` and `TxRegistry::take`
  are gone. Every route's lookup is now the lease: the run route leases
  the entry out with `lease` and returns it with `Lease::release`; commit
  and rollback lease it too and exit one-way with `Lease::take` — no
  engine statement ever runs under the registry mutex.
  The public surface item 2 lists is unchanged — `TxRegistry::with` and
  `TxRegistry::take` were crate-private then and their successors are
  crate-private now — and item 3's owner scoping is untouched.
- [ADR-0501](./0501-drop-token-schedule.md) removes `metrics.tokenSchedule`. A receipt still carries
 `metrics.peakLogicalBytes` (the name [ADR-0452](./0452-workspace-limit-enables-admission.md) gave the former
 `metrics.peakTokens`) and, since [ADR-0440](./0440-follower-writer-toggle.md), `metrics.retryable`.
