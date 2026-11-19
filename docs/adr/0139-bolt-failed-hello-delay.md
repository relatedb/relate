---
type: adr
area: surface
date: 2026-09-29
deciders: CypherLite maintainers
name: 0139-bolt-failed-hello-delay
status: accepted
tags:
  - bolt
  - security
title: Bolt failed-HELLO delay
REFERENCES:
  - "[[adr/0110-bolt-connection-query-governance]]"
  - "[[adr/0126-bolt-basic-auth]]"
  - "[[adr/0266-bolt-websocket-transport]]"
---

# 0139. Bolt failed-HELLO delay

## Context

[ADR-0126](./0126-bolt-basic-auth.md) validates credentials at HELLO: on a bad or missing token the server
answers `Neo.ClientError.Security.Unauthorized` and closes — immediately.
With Bolt over WebSocket ([ADR-0266](./0266-bolt-websocket-transport.md)) any website can open a `ws://` connection
to a developer's loopback Bolt server from a visitor's browser, so when the
operator has chosen a preset credential (`--auth user:pw`, [ADR-0126](./0126-bolt-basic-auth.md)), that
browser channel enables remote password guessing at network speed. The
generated-token default (128-bit random, [ADR-0126](./0126-bolt-basic-auth.md)) is not guessable and needs
no rate limiting; the risk is operator-chosen preset credentials, which can be
short and human-memorable.

The decisions below were settled by the maintainer before this ADR was
written; this record states them rather than reopening them.

## Decision

A **failed HELLO** — bad scheme, bad principal, or bad credentials
([ADR-0126](./0126-bolt-basic-auth.md)) — waits a fixed delay before the server sends
`Neo.ClientError.Security.Unauthorized` and closes.

- The delay is a **fixed constant of 1000 ms**
  (`FAILED_HELLO_DELAY_MS: u64 = 1000`), compiled in, with **no flag**. One
  second is long enough to crush online guessing throughput (see
  Consequences) and short enough that a human who typed a wrong password
  experiences a one-second pause, not a hang. A flag would invite operators
  to set it to zero and silently remove the protection; a fixed constant
  keeps the posture uniform.
- The delay applies **only to failed HELLO**. A successful HELLO is answered
  immediately — there is no penalty on the happy path, so latency-sensitive
  clients are unaffected.
- The delay is an **async sleep** (`tokio::time::sleep`) in the connection
  task: it blocks no worker thread, and other connections — including other
  failing ones — proceed concurrently.
- The delay applies **identically on all transports**: raw TCP Bolt, TLS,
  `ws://`, and `wss://`. It lives in the HELLO handling of
 `protocol::handle_conn`, below the transport split ([ADR-0266](./0266-bolt-websocket-transport.md)), so no
  transport can bypass it.

## Alternatives considered

- **Lockout after N failures.** Rejected: per-credential lockout state is
  either global (one attacker's guesses DoS the legitimate user) or keyed by
  something forgeable (source IP is one NAT away from shared; principals are
  attacker-chosen); a fixed delay bounds the guess rate with no state and no
  lockout foot-gun.
- **Per-IP throttling.** Rejected: requires a shared rate-limit table sized
  against memory exhaustion, punishes every legitimate client behind a NAT
  for one guesser, and still lets a botnet multiply the rate; a
  per-connection delay composes with the existing `--max-connections` cap
 ([ADR-0110](./0110-bolt-connection-query-governance.md)) instead of adding a second accounting system.
- **Restricting the default Origin policy for preset credentials.**
  Rejected **for now**: refusing non-loopback Origins whenever `--auth
  user:pw` is set would remove the browser channel from the guessing threat
  model entirely, but it breaks the legitimate "neo4j-browser hosted
 elsewhere + preset password" deployment that [ADR-0266](./0266-bolt-websocket-transport.md)'s default
  (auth-on ⇒ any Origin) was chosen to serve. Recorded here as a considered,
  stronger option, deferred to a later ADR if deployment experience calls
  for it.
- **No delay.** Rejected: it leaves browser-driven online guessing of preset
  credentials at wire speed on the exact feature (WebSocket) that made the
  loopback server reachable from arbitrary websites.

## Consequences

- Online guessing of a preset credential is bounded by the delay and the
 connection cap: at the default `--max-connections` of **128** ([ADR-0110](./0110-bolt-connection-query-governance.md)),
  an attacker sustains at most **128 concurrent guesses per 1000 ms delay
  period — ≤ 128 guesses/second** — against a single server, versus
  effectively unbounded wire-speed attempts before this ADR. Sequential
  (single-connection) guessing is bounded at 1 guess/second.
- The generated-token default ([ADR-0126](./0126-bolt-basic-auth.md)) is untouched and remains the
  recommended posture: 128 bits are unguessable at any rate this delay
  permits.
- Legitimate clients see no change on success; a mistyped password costs one
  extra second.
- The constant, its failed-HELLO-only scope, and its all-transports
  application must be covered by tests; `docs/api/bolt/spec.md` documents
 the behavior alongside the [ADR-0126](./0126-bolt-basic-auth.md) auth contract.
- Refines [ADR-0126](./0126-bolt-basic-auth.md) (the Unauthorized path is no longer immediate) and
 [ADR-0266](./0266-bolt-websocket-transport.md) (whose Governance section states HELLO auth is unchanged — the
  credential check is unchanged; only the failure timing changes).
