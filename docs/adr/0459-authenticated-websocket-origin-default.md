---
type: adr
area: surface
claim: a6a26180966760d49666b6a6b30fbc4097c7fbda
name: 0459-authenticated-websocket-origin-default
status: proposed
tags:
  - security
title: The authenticated WebSocket Origin default is loopback plus the hosted Neo4j web apps
REFERENCES:
  - "[[adr/0110-bolt-connection-query-governance]]"
  - "[[adr/0141-bolt-single-handshake-deadline]]"
  - "[[adr/0266-bolt-websocket-transport]]"
  - "[[adr/0333-processing-memory-controls]]"
  - "[[adr/0368-one-product-binary-serve-absorbs-bins]]"
  - "[[adr/0451-bound-unauthenticated-network-connections]]"
---

# ADR-0459: The authenticated WebSocket Origin default is loopback plus the hosted Neo4j web apps

## Context

The 27 October security review's finding 6 (LABS-607) is that the Bolt
WebSocket upgrade admits any browser Origin whenever authentication is on,
and that the WebSocket path has no dedicated concurrency bound.

`surfaces/cypherlite-bolt/src/websocket.rs` resolves its Origin policy from
the `--ws-allow-origin` list and the auth mode ([ADR-0266](./0266-bolt-websocket-transport.md)). [ADR-0266](./0266-bolt-websocket-transport.md) chose:

- with auth on, **any** Origin — "the token is the access control, and
  neo4j-browser may be served from any host";
- with `--auth none`, loopback Origins only — the cross-site WebSocket
  hijacking (CSWSH) guard.

The any-Origin arm is not needed by the clients the feature exists for.
The hosted Neo4j Browser is a web application at
<https://browser.neo4j.io> (Neo4j Browser manual, "Connect to a Neo4j
Instance"), the hosted Neo4j Console is at <https://console.neo4j.io>,
and both are fixed origins. A self-hosted Browser, Neo4j Desktop, and a
driver page are normally on loopback. What the any-Origin arm does leave
open is a weak preset password: `--auth user:password` /
`CYPHERLITE_AUTH` is operator-chosen and low-entropy by accident, and a
malicious page's JavaScript can then attempt authentication from the
victim's browser against a loopback server — the standard CSWSH shape,
with the credential guess instead of the session as the payload. A
generated token is not practically guessable, which is why the review
rates the finding Low and why this ADR does not clamp the loopback arm
further.

The finding also asks to "restore bounded connections". The connection
cap has moved twice since [ADR-0266](./0266-bolt-websocket-transport.md): [ADR-0333](./0333-processing-memory-controls.md) made it one process-wide
opt-in (`0` unlimited), and [ADR-0451](./0451-bound-unauthenticated-network-connections.md) restored a non-zero default (128) for
`cypherlite serve` and the HTTP/MCP listeners — which already bounds
WebSocket connections under `cypherlite serve`, because WebSocket rides
the same Bolt listener and the same accept-time permit. The Bolt crate's
own listener (`cypherlite_bolt::serve`, the e2e/compat harness and
embedders) still defaults `--max-connections` to unlimited. The
maintainer confirmed the unbounded default there was intentional: a
database has legitimate bulk data-in/out workloads that run many parallel
connections, and a default WebSocket cap would refuse them.

Time and memory on the WebSocket path are already bounded, so an absent
concurrency cap does not enable a slowloris-style hold: the upgrade and
Bolt negotiation run under the [ADR-0141](./0141-bolt-single-handshake-deadline.md) accept-time handshake deadline;
session reads run under the [ADR-0110](./0110-bolt-connection-query-governance.md) idle deadline, which
`Framer::next_batch` re-arms as one absolute budget per message (a byte
per read window cannot hold a message open); and
`MAX_WS_FRAME_BYTES` (1 MiB), `MAX_WS_MESSAGE_BYTES` (16 MiB) and the
`MAX_PENDING_BYTES` framing bound cap memory. The NOOP keep-alive is a
different mechanism (server-to-client while engine work runs) and is not
part of this argument. What an absent cap leaves open is concurrency
itself — threads and file descriptors — which is exactly what the opt-in
cap added below bounds.

## Decision

### 1. The auth-on default: loopback plus the hosted Browser and Console

An empty `--ws-allow-origin` list resolves to loopback Origins (any port)
**plus** the hosted Neo4j web applications, with auth on:

- `https://browser.neo4j.io` — the Neo4j Browser web application.
- `https://console.neo4j.io` — the Neo4j (Aura) Console.

This is the same hosted set the HTTP surface's `OriginAllowlist::Default`
already allows, and the list now lives once in
`cypherlite_serve::origin::HOSTED_WEB_APP_ORIGINS`, consulted by both
surfaces so they cannot drift. Matching is canonical-to-canonical, the
same exact comparison `--ws-allow-origin` entries use. The loopback arm
is unchanged: host-based, any port, the serialized-spelling
classification [ADR-0266](./0266-bolt-websocket-transport.md) states. A missing `Origin` header remains a
non-browser client and is allowed.

Any other Origin needs an explicit `--ws-allow-origin` entry; `*` remains
the explicit any-Origin opt-in. `--auth none` keeps its loopback-only
default unchanged (the CSWSH guard).

Accepted tradeoff: a compromised *local* application on another localhost
port still passes the loopback arm. Local compromise is game over by
definition — the same reasoning [ADR-0266](./0266-bolt-websocket-transport.md)'s loopback arm already records.

`https://browser.graphapp.io` is **not** in the default set. The repo's
HTTP default does not include it, the Neo4j Browser manual documents
`browser.neo4j.io` as the hosted Browser, and the graph-app host is a
separate deployment; an operator using it passes `--ws-allow-origin`
(today's e2e WebSocket test already does). Extending the default set is
one entry in `HOSTED_WEB_APP_ORIGINS`.

### 2. The wider-origin opt-in is reachable from the product binary

`cypherlite serve` gains the repeatable `--ws-allow-origin ORIGIN` flag
(the crate-level `Args` already had it). Without it, the tightened
default would leave a deployment that serves a self-hosted Browser from a
non-loopback origin with no product-binary remedy, which is not a
compatibility break an operator can act on. The flag parses through the
shared origin grammar (`cypherlite_serve::origin::OriginFlagValue`), is
stored canonically, and an explicit list replaces the auth-dependent
default's wider reach exactly as the library flag does.

`--no-websocket` stays library-level only ([ADR-0368](./0368-one-product-binary-serve-absorbs-bins.md) records that split);
this ADR does not widen that surface set further.

### 3. An opt-in WebSocket session cap, default off

`--ws-max-connections N` (crate `Args` and `cypherlite serve`) /
`CYPHERLITE_WS_MAX_CONNECTIONS` bounds concurrent WebSocket sessions,
resolved flag → environment → unlimited (`0`). It reuses the shared
`cypherlite_serve::ConnectionCap`: the permit is taken when a connection
classifies as a WebSocket upgrade, before the upgrade read, held for the
whole session, and an over-cap connection is accepted then closed with no
HTTP response and the log line
`bolt: --ws-max-connections <N> reached; closing connection` — the same
accept-then-close policy the process cap applies.

The default is **off**, deliberately. The process-wide
`--max-connections` cap is unchanged and still bounds every accepted
connection, so `cypherlite serve` deployments are not exposed by this
default; the knob exists for an operator who raised the process cap for
bulk raw-Bolt transfer and wants browser sessions bounded separately, or
who runs the crate-level listener. A non-zero WebSocket default would
refuse the bulk-transfer workload the maintainer confirmed as the intent
behind the currently unbounded listener.

## Alternatives considered

- **Keep [ADR-0266](./0266-bolt-websocket-transport.md)'s any-Origin default with auth on.** Rejected: the
  documented clients do not need it (their origins are fixed or
  loopback), and it turns a weak preset password into a
  browser-originated credential-guessing surface against the loopback
  server.
- **Make the default hosted-only, dropping loopback.** Rejected: a
  self-hosted Browser, Neo4j Desktop, and a driver page on loopback are
  the primary browser use case; dropping the loopback arm breaks them.
- **Include `https://browser.graphapp.io` in the default.** Rejected:
  it is not the hosted Browser origin the Neo4j documentation names, the
  HTTP default does not include it, and adding it would allow a
  third-party graph-app host by default; the explicit flag covers it.
- **Reuse the HTTP surface's `OriginAllowlist` type.** Rejected: it
  classifies the bare host, while the WebSocket guard deliberately uses
 the serialized-spelling loopback arm [ADR-0266](./0266-bolt-websocket-transport.md) chose (a bracketed
  non-IPv6 host does not pass). The shared pieces — the parse, the
  loopback set, the hosted list, the wildcard/exact matcher — are already
  factored into `cypherlite-serve`, and only the projections differ.
- **Restore a non-zero default WebSocket cap.** Rejected: a database has
  legitimate bulk data-in/out workloads with many parallel connections;
  the maintainer confirmed the unbounded listener was intentional, and
  `cypherlite serve` is already bounded by the process cap.
- **Bound WebSocket sessions at accept time only (reuse
  `--max-connections`).** Rejected: the accept-time permit is taken
  before the protocol is known, so it cannot express "raise the process
  cap for bulk raw-Bolt transfer but bound browser sessions"; the
  WebSocket-specific permit is taken after classification.
- **Leave `--ws-allow-origin` library-level and document the break.**
  Rejected: the tightened default would be actionable only by embedding
  the crate, and the task's premise is that the wider-origin opt-in is
  retained.

## Consequences

- **Compatibility break.** An authenticated WebSocket client whose
  Origin is neither loopback nor `https://browser.neo4j.io` /
  `https://console.neo4j.io` is now answered `HTTP/1.1 403 Forbidden`
  unless the deployment passes `--ws-allow-origin`. A self-hosted
  Browser, Console, or driver page served from a non-loopback origin must
  name that origin. Generated-token deployments that only use the hosted
  apps or loopback are unaffected.
- `--auth none` is unchanged: loopback only, no widening without an
  explicit flag.
- `cypherlite serve` gains `--ws-allow-origin` (repeatable) and
  `--ws-max-connections`; both are rejected as unbound on a run that does
  not bind `--bolt`, and both appear in the hand-written `serve --help`
  and `docs/api/cli/spec.md` in the same change.
- The WebSocket-only cap defaults to unlimited, so an unconfigured
  listener still admits as many WebSocket sessions as the process cap
  allows. Operators who need a bound set it; over-cap sessions are
  rejected, not queued.
- `docs/api/bolt/spec.md`, `docs/api/bolt/description.md`, the WebSocket
  example, and `docs/api/cli/spec.md` state the new default, the flags,
  and the compatibility break.
- Tests cover the allowed/foreign Origin matrix under both auth modes
  (including loopback on any port, the hosted Browser/Console, and the
  missing-Origin case), and the cap: off by default, over-cap refusal with
  no response, and slot release after a session ends.
