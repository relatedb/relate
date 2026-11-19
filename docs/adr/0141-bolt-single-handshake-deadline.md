---
type: adr
area: surface
date: 2026-09-29
deciders: CypherLite maintainers
name: 0141-bolt-single-handshake-deadline
status: accepted
tags:
  - bolt
  - security
title: Bolt one handshake deadline per connection
REFERENCES:
  - "[[adr/0110-bolt-connection-query-governance]]"
  - "[[adr/0266-bolt-websocket-transport]]"
---

# 0141. Bolt one handshake deadline per connection

## Context

[ADR-0110](./0110-bolt-connection-query-governance.md) gave the Bolt handshake a timeout (default 5 s), and [ADR-0266](./0266-bolt-websocket-transport.md)'s
Governance section extended it to the WebSocket upgrade read. Both were
implemented as **per-phase budgets**: each negotiation phase — the first-byte
peek, the TLS handshake, the post-TLS first-byte re-sniff, the WebSocket
upgrade read, and the Bolt magic/version exchange — gets a fresh copy of the
timeout when it starts.

A slow client can therefore consume one full budget per phase: up to **5× the
timeout (25 s at the default)** holding a `--max-connections` permit
([ADR-0110](./0110-bolt-connection-query-governance.md)) before serving a single byte of Bolt. With the cap at 128
permits, 128 slow-dripping clients can occupy the entire server for 25
seconds per rotation — exactly the connection-governance failure mode
[ADR-0110](./0110-bolt-connection-query-governance.md) exists to prevent, amplified by the phase count [ADR-0266](./0266-bolt-websocket-transport.md) added.

This decision refines [ADR-0110](./0110-bolt-connection-query-governance.md)'s handshake timeout and [ADR-0266](./0266-bolt-websocket-transport.md)'s inheritance
of it; it was settled by the maintainer before this ADR was written.

## Decision

**One deadline** is computed when a connection is accepted — `now +
--handshake-timeout-secs` (default **5 s**, [ADR-0110](./0110-bolt-connection-query-governance.md)) — and it covers every
negotiation phase:

1. the first-byte read (transport peek),
2. the TLS handshake (when the first byte is `0x16`),
3. the post-TLS first-byte peek (re-sniff of the decrypted stream),
4. the WebSocket upgrade read (when the byte is `0x47`),
5. the Bolt magic/version exchange.

Each phase is wrapped in `tokio::time::timeout_at` (or equivalent) against
the **same** accept-time deadline; a phase that starts with no budget left
fails immediately. When the magic/version exchange completes, the deadline
is discharged and the [ADR-0110](./0110-bolt-connection-query-governance.md) idle per-read timeout (default 300 s) governs
the session from then on, unchanged.

## Alternatives considered

- **Keep per-phase budgets.** Rejected: 5× amplification of worst-case
  pre-service occupancy per connection, scaling with the phase count; the
  amplification is exactly what a slowloris-style client exploits.
- **A separate, shorter timeout for the WebSocket upgrade.** Rejected: one
  more knob for operators to learn, and no operator need for a distinct
  upgrade budget has been identified; the single deadline already bounds the
  upgrade read.

## Consequences

- Worst-case pre-service occupancy per connection drops from up to 5× the
  timeout to **one** `--handshake-timeout-secs`; at the defaults, 25 s → 5 s
  per held permit.
- No flag changes: `--handshake-timeout-secs` keeps its name, default, and
  meaning; it now bounds the whole negotiation instead of each phase.
- Legitimate clients are unaffected: every phase of a healthy handshake
  completes in milliseconds on loopback or LAN.
- Tests must pin that the deadline is computed once at accept time, spans
  all five phases across the raw/TLS/`ws://`/`wss://` paths, and discharges
  into the idle timeout after the version exchange.
- Refines [ADR-0110](./0110-bolt-connection-query-governance.md) (per-phase budgets → one deadline) and [ADR-0266](./0266-bolt-websocket-transport.md) (which
  inherited the per-phase rule for the upgrade read); both gain **Later
  references** notes pointing here.
