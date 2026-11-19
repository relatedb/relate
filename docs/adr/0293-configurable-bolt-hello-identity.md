---
type: adr
area: surface
name: 0293-configurable-bolt-hello-identity
status: accepted
tags:
  - bolt
title: Bolt HELLO SUCCESS metadata — configurable server identity (honest default) and a connection_id
REFERENCES:
  - "[[adr/0057-embedded-c-abi-and-bolt]]"
  - "[[adr/0109-bolt-tls]]"
  - "[[adr/0312-bolt-hello-recv-timeout-hint-and-noop-keepalive]]"
---

# ADR-0293: Bolt HELLO SUCCESS metadata — configurable server identity (honest default) and a connection_id

Renumbered: from 0250 (shared-number cleanup)

## Context

The Bolt HELLO SUCCESS carries metadata strict Neo4j tooling
hard-checks before any capability negotiation:

- The `server` agent string. CypherLite's Bolt server ([ADR-0057](./0057-embedded-c-abi-and-bolt.md),
 [ADR-0109](./0109-bolt-tls.md)) has always answered `CypherLite/<crate version>`, compiled
  in. `cypher-shell` refuses to connect with `Server does not identify
  as a genuine Neo4j instance: 'CypherLite/…'` because the value is not
  a Neo4j-shaped version string.
- The `connection_id` key. The Java driver's `HelloResponseHandler`
  (6.x) fails the connection with `IllegalStateException: Unable to
  extract connection_id from a response to HELLO message` when the
  metadata lacks the key; any string value satisfies it.

Operators who want `cypher-shell` or the current Java driver against a
CypherLite Bolt endpoint need these keys to satisfy the checks;
operators who do not should keep an honest identity.

## Decision

The HELLO SUCCESS metadata becomes a small, explicit contract:

- **`server`** is **configured data**, threaded from the command line
  into the connection configuration, with an honest default:
  - Default (flag unset): `CypherLite/<crate version>` — unchanged for
    every existing deployment and every embedded `serve --bolt` run.
  - `cypherlite-bolt` gains **`--identity <STRING>`** (env
    `CYPHERLITE_IDENTITY`, matching the crate's `CYPHERLITE_*`
    convention). The flag value is sent verbatim, e.g.
    `cypherlite-bolt --identity Neo4j/5.26.0`.
  - `cypherlite serve --bolt` gains **`--bolt-identity <STRING>`**
    (flag, then `CYPHERLITE_IDENTITY`, then the default) for the
    embedded Bolt surface. The surface-qualified name states which
    surface's identity it sets — `serve` hosts three surfaces and only
    Bolt has a HELLO agent string, but the prefix keeps that obvious at
    the command line and leaves room for any later per-surface knob.
- **`connection_id`** is always sent: `conn-N` from a per-listener
  atomic counter (the `Arc` inside the cloned `ConnConfig`), unique
  within one server like Neo4j's per-server ids. No `uuid` dependency;
  the key's presence is what the driver requires.
- **`hints`** is omitted: the only standardized hint
  (`connection.recv_timeout_seconds`) advertises a server-side receive
  timeout, and CypherLite's idle timeout is a governance bound, not the
  Neo4j hint's semantics; drivers treat the key as optional (verified
  against the Java driver 6.3.0 via sysbench4j).

## Alternatives considered

- **Default to a Neo4j-shaped identity** (spoof by default): rejected —
  the agent string would misreport the engine to every client, log, and
  driver metric; strict tooling checking identity is exactly the case
  where the honest string has diagnostic value, and silently claiming
  to be Neo4j misleads everyone else.
- **No flag — keep the identity compiled in**: rejected — `cypher-shell`
  and tools with the same hard check are then unusable against
  CypherLite Bolt, with no workaround short of a TCP proxy rewriting
  one field.
- **`--identity` on `serve` unprefixed**: rejected — `serve` hosts
  several surfaces; an unqualified `--identity` would read as a
  server-wide claim while only configuring the Bolt surface.
- **An empty `hints: {}` map**: rejected — a hintless empty map claims
  nothing and tests showed the driver neither requires nor reads it;
  omitting keeps the SUCCESS map to keys with meaning.
- **A process-global counter for `connection_id`**: rejected — ids are
  a per-listener concern; a global would couple embedded and standalone
  servers sharing one process (the CLI hosts one, but tests and
  embedders may host several) for no benefit.

## Consequences

- An operator-set identity is the operator's claim about the endpoint.
  A spoofed Neo4j identity may **unlock tooling behaviors the server
  does not implement** (cypher-shell's `:sysinfo`, version-gated
  driver features, browser sidebar behaviors that branch on the
  version): those fail later, at the feature boundary, not at HELLO.
  The flag is a compatibility shim for strict identity checks, not a
 Neo4j feature-parity claim — Bolt stays a transport ([ADR-0057](./0057-embedded-c-abi-and-bolt.md)).
- `connection_id` is a session label, not a handle: the server never
  reads it back (no connection-management messages exist to address
  one). If Bolt 5.x connection-scoped messages ever land, the id is
  already unique per listener.
- Support and issue reports must treat a configured identity as an
  operator override: the `--help` text and both flag tables document
  the default so the honest value is always one removal away.
- Tests pin both semantics: `ConnConfig::default()` (and a flagless
  `serve`) answers `CypherLite/<version>`; a configured value is
  carried verbatim; `connection_id` is present and unique per listener.
- Empirical acceptance: the Java driver 6.3.0 (via sysbench4j
  `prepare`) connects past HELLO against `cypherlite serve --bolt`
  with `--bolt-identity Neo4j/5.26.0`; anything beyond connectivity is
  workload scope, not this decision.

## Later references

- [ADR-0312](./0312-bolt-hello-recv-timeout-hint-and-noop-keepalive.md) reverses the hints-omitted bullet: HELLO SUCCESS now carries
  `hints: {"connection.recv_timeout_seconds": 120}`, and the server sends
  NOOP keep-alive chunks during long requests so that hint holds.
  `server` and `connection_id` are unchanged.
