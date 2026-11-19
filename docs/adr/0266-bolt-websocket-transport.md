---
type: adr
area: surface
date: 2026-09-29
deciders: CypherLite maintainers
name: 0266-bolt-websocket-transport
status: accepted
tags:
  - bolt
title: Bolt over WebSocket on the same bind
REFERENCES:
  - "[[adr/0057-embedded-c-abi-and-bolt]]"
  - "[[adr/0103-engine-surfaces-testing-layout]]"
  - "[[adr/0109-bolt-tls]]"
  - "[[adr/0110-bolt-connection-query-governance]]"
  - "[[adr/0126-bolt-basic-auth]]"
  - "[[adr/0141-bolt-single-handshake-deadline]]"
  - "[[adr/0142-browser-compat-record-shapes]]"
  - "[[adr/0459-authenticated-websocket-origin-default]]"
---

# 0266. Bolt over WebSocket on the same bind

- **Renumbered**: from 0136 (shared-number cleanup)
- **Later references**: [ADR-0142](./0142-browser-compat-record-shapes.md) resolves the `dbms.components` edition-column
  gap listed under "Browser-compat plan (settled)" (LABS-139) and gives SHOW
  DATABASES rows concrete values.

## Context

`cypherlite-bolt` is the optional remote transport ([ADR-0057](./0057-embedded-c-abi-and-bolt.md)), serving Bolt on
a single port with a first-byte peek ([ADR-0109](./0109-bolt-tls.md)): `0x16` routes to the TLS
handshake, `0x60` to plaintext Bolt, anything else is closed. The handshake is
bounded by a timeout and connections are capped ([ADR-0110](./0110-bolt-connection-query-governance.md)); authentication
runs in the Bolt HELLO message ([ADR-0126](./0126-bolt-basic-auth.md)).

Linear LABS-138
(<https://linear.app/neo4j/issue/LABS-138>) asks for browser access. Browsers
cannot open raw TCP sockets; the official Neo4j JavaScript driver's browser
build and neo4j-browser speak Bolt over WebSocket (RFC 6455). Today
`surfaces/cypherlite-bolt/src/tls.rs` `serve_conn` peeks the first byte and
closes anything that is not `0x16` or `0x60`, so a WebSocket upgrade request —
an HTTP `GET`, first byte `0x47` — is closed immediately and no browser client
can connect.

The decisions below were settled by the maintainer in planning before this ADR
was written; this record states them rather than reopening them:

1. Use `tokio-tungstenite` plus an own `AsyncRead`/`AsyncWrite` adapter so
   `protocol::handle_conn` is reused unchanged.
2. A repeatable `--ws-allow-origin` flag (`*` = any); default is any Origin
   when auth is on and loopback-only Origins with `--auth none`.
3. WebSocket is on by default with a `--no-websocket` opt-out.
4. Bolt-layer browser gaps are fixed in this feature: the ROUTE self-table and
   the ProcedureNotFound FAILURE-code mapping each get their own follow-up ADR
   on the same feature branch; engine gaps are filed as follow-up Linear
   issues.
5. The official JS driver browser build is the CI end-to-end client.

## Decision

### Transport routing

The first-byte peek in `serve_conn` gains a third arm:

- `0x16` (TLS `ClientHello`) → run the TLS handshake, then perform a **second
  first-byte sniff on the decrypted stream**:
  - `0x47` → WebSocket upgrade over TLS (`wss://`).
  - `0x60` → Bolt over TLS (`bolt+s://`).
  - anything else → close the connection.
- `0x60` (Bolt magic preamble) → plaintext Bolt, unchanged.
- `0x47` (`G` of `GET`) → WebSocket upgrade path (`ws://`).
- anything else → close the connection, unchanged.

`ws://` follows the [ADR-0109](./0109-bolt-tls.md) plaintext policy exactly as raw-TCP plaintext
Bolt: allowed silently on loopback binds, refused on non-loopback binds
without `--allow-plaintext`.

### Implementation

Use **`tokio-tungstenite`** as a workspace dependency with default features
off — server-side accept only; it needs no TLS feature because rustls
([ADR-0109](./0109-bolt-tls.md)) already terminates TLS before the second sniff. A thin adapter in
`surfaces/cypherlite-bolt/src/websocket.rs` implements `AsyncRead` +
`AsyncWrite` over the accepted WebSocket, so `protocol::handle_conn` is reused
unchanged. No WebSocket subprotocol is requested or required; the JS driver
sends none.

### Framing

- Inbound: WebSocket binary message payloads are concatenated into the Bolt
  byte stream. Frame and message boundaries carry no Bolt meaning — the
  driver's inbound `Dechunker` treats the stream as bytes and tolerates
  splits.
- Outbound: each Bolt write maps to one binary WebSocket message.
- A text frame closes the connection with close code **1003** (unsupported
  data).
- Ping frames are answered with pong by tungstenite automatically.
- A close frame closes the connection cleanly.

### Caps

- `max_upgrade_request_bytes` = **8192** — bounds the HTTP upgrade request
  (request line plus headers); browser upgrade requests are well under 1 KiB,
  so 8 KiB is generous headroom without inviting header-stuffing.
- `max_frame_bytes` = **1048576** (1 MiB) — a frame declaring more than this
  is rejected before its payload is buffered, so a hostile frame header cannot
  force a large allocation.
- `max_message_bytes` = **16777216** (16 MiB) — matches the existing Bolt
  message cap, so the WS path cannot buffer more than the raw-TCP path already
  accepts.

### HTTP responses on the upgrade path

- A `GET` (or any request) on the `0x47` path without a valid WebSocket
  Upgrade receives `HTTP/1.1 400 Bad Request` and the connection closes.
- A valid upgrade whose Origin is rejected receives `HTTP/1.1 403 Forbidden`
  and the connection closes.

Both are plain, minimal HTTP/1.1 responses written before any WebSocket
framing begins; 400 and 403 are the conventional statuses for a malformed
upgrade and a refused Origin respectively.

### Origin policy

Browsers always send an `Origin` header on a WebSocket upgrade; a **missing
Origin header means a non-browser client and is allowed**. The policy applies
only to present Origins:

- `--ws-allow-origin <ORIGIN>` is repeatable; the value `*` allows any Origin.
- Default with auth on ([ADR-0126](./0126-bolt-basic-auth.md)'s default is a generated token): **any**
  Origin is allowed — the token is the access control, and neo4j-browser may
  be served from any host.
- Default with `--auth none`: only loopback Origins are allowed —
  `http://localhost:*`, `http://127.0.0.1:*`, `https://localhost:*`,
  `https://127.0.0.1:*`, and the `[::1]` forms (`http://[::1]:*`,
  `https://[::1]:*`). This is the cross-site WebSocket hijacking guard:
  without auth, any website the victim visits could otherwise open a WebSocket
  to the loopback server and read/write the graph.

### Flags and banner

- `--no-websocket` — opt out of WebSocket serving (default: WebSocket on). A
  `0x47` connection then closes like any other unrecognized first byte.
- `--ws-allow-origin <ORIGIN>` — repeatable; `*` allows any Origin; overrides
  the auth-dependent default above.
- The startup banner line gains a `ws` field appended inside the parentheses
  of the existing line, after `auth`:

  ```
  CypherLite Bolt listening on 127.0.0.1:7687 (uri=..., tls=..., auth=..., ws=on)
  ```

  where `ws` is `on` or `off`.

### Governance

- The [ADR-0110](./0110-bolt-connection-query-governance.md) handshake timeout bounds the HTTP upgrade read: an upgrade that
  stalls mid-headers is dropped like a stalled Bolt handshake.
- The `--max-connections` cap ([ADR-0110](./0110-bolt-connection-query-governance.md)) applies to WebSocket connections
  exactly as to raw-TCP ones.
- Authentication ([ADR-0126](./0126-bolt-basic-auth.md)) is unchanged: HELLO auth runs identically over the
  WebSocket transport.

### Browser-compat plan (settled)

Bolt-layer gaps the browser clients expose are fixed in this feature branch
under their own ADRs, numbered after this one: the ROUTE self-table and the
ProcedureNotFound FAILURE-code mapping. Engine gaps — `dbms.components`
edition column, `dbms.clientConfig`, `dbms.listConfig`,
`dbms.showCurrentUser`, `dbms.licenseAgreementDetails` — are filed as
follow-up Linear issues and are not part of this branch.

## Alternatives considered

- **Hand-rolled RFC 6455.** Rejected: it is ~300–400 lines of
  security-critical untrusted-input parsing (frame headers, masking, length
  encodings, fragmentation, close codes) to own and fuzz, versus a mature,
  widely deployed crate; it would only save the small tungstenite dependency
  tree.
- **A separate WebSocket port.** Rejected: neo4j-browser users would need a
  second port and a deployment shape Neo4j does not have; the first-byte peek
 architecture ([ADR-0109](./0109-bolt-tls.md)) already multiplexes several protocols on one bind.
- **Accept any Origin unconditionally.** Rejected: with `--auth none` any
  website could drive a victim's browser to read and write a loopback graph —
  classic cross-site WebSocket hijacking; the loopback-Origin default closes
  it without affecting the authenticated case.
- **Opt-in `--websocket` flag.** Rejected: Neo4j parity serves WebSocket on
  7687 by default; on-by-default with a `--no-websocket` opt-out keeps the
  surface predictable and matches what browser clients expect.
- **A hyper-based HTTP server for the upgrade.** Rejected: the upgrade path is
  a single request/response exchange; pulling in hyper's connection management
  and HTTP machinery for one handshake adds a heavy dependency for no benefit
  over tungstenite's accept API.

## Consequences

- `cypherlite-bolt` gains `tokio-tungstenite` as a workspace dependency
  (default features off); transitively `tungstenite` brings `sha1` and
 `data-encoding`. `cypherlite-bolt` is a surface crate, so the [ADR-0103](./0103-engine-surfaces-testing-layout.md) R6
  engine-crate dependency ADR gate does not apply.
- HTTPS-hosted browsers (e.g. `https://browser.graphapp.io`) dial `wss://`:
  the driver browser channel (`determineWebSocketScheme` in
  `packages/bolt-connection/src/channel/browser/browser-channel.js`, identical
  in driver 5.26.0 and 6.2.0) picks `wss://` when `window.location.protocol`
  starts with `https` and `ws://` otherwise, with explicit `ENCRYPTION_ON`
  forcing `wss` and `ENCRYPTION_OFF` forcing `ws`; neo4j-browser does not
  override. Serving those pages therefore needs a **browser-trusted
  certificate** on `cypherlite-bolt`. HTTP-hosted or self-hosted
  neo4j-browser and Neo4j Desktop dial `ws://` and work with the default
 plaintext loopback posture ([ADR-0109](./0109-bolt-tls.md)).
- WebSocket connections count against `--max-connections` and are bounded by
 the [ADR-0110](./0110-bolt-connection-query-governance.md) handshake timeout like every other connection.
- With `--auth none`, cross-site WebSocket access from non-loopback Origins is
  refused by default; operators who need it must pass `--ws-allow-origin`
  explicitly.
- Tests and documentation must cover the `0x47` peek arm, the post-TLS
  re-sniff, the Origin policy matrix (auth on/off, loopback/cross-site,
  missing Origin, `*`), the caps, the 400/403 responses, text-frame close
  1003, `--no-websocket`, and the JS driver browser-build e2e in CI.

## Later references

- [ADR-0141](./0141-bolt-single-handshake-deadline.md) refines the handshake-timeout governance inherited here: one
  accept-time deadline of `--handshake-timeout-secs` spans every negotiation
  phase, including the WebSocket upgrade read, instead of a fresh budget per
  phase.
- [ADR-0459](./0459-authenticated-websocket-origin-default.md) replaces the authenticated WebSocket Origin default (any Origin)
  with loopback plus the hosted Neo4j Browser and Console, makes
  `--ws-allow-origin` a `cypherlite serve` flag, and adds the opt-in
  `--ws-max-connections` session cap (LABS-607, the 27 October security
  review's finding 6).
