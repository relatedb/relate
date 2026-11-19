---
type: adr
area: surface
date: 2026-09-28
deciders: CypherLite maintainers (agreed in conversation)
name: 0264-bolt-signal-shutdown
status: accepted
tags:
  - bolt
  - serve
title: cypherlite-bolt exits on SIGINT/SIGTERM
REFERENCES:
  - "[[adr/0111-engine-bounded-execution]]"
---

# 0264. cypherlite-bolt exits on SIGINT/SIGTERM

- **Renumbered**: from 0125 (shared-number cleanup)

## Context

The server is typically PID 1 in a container (the image entrypoint execs it),
and the kernel ignores unhandled signals for PID 1 by design. Before this
ADR, Ctrl-C on an attached `docker run` and `docker stop` did nothing — the
container kept running and holding its ports until SIGKILL.

## Decision

`cypherlite-bolt` installs explicit SIGINT/SIGTERM handlers (tokio
`signal`; the workspace tokio gains the `signal` feature). `main` wraps
`serve` in `tokio::select!` with the two signal arms: on the first signal
the server prints a shutdown line and exits — no drain, no in-flight
query cancellation beyond what [ADR-0111](./0111-engine-bounded-execution.md) already provides. The SIGTERM
arm is unix-only (cfg-gated; on Windows it is a never-completing future),
so the binary still builds where `tokio::signal::unix` does not exist.

Durability is unaffected: the WAL is fsync'd on commit, so any exit —
signal or otherwise — replays cleanly on the next open.

## Alternatives considered

- **Rely on `docker run --init`** — offloads signal handling to tini, but
  only helps containers; a bare server still ignores Ctrl-C.
- **Graceful drain (close listeners, finish in-flight queries)** — deferred;
  nothing today keeps a client's PULL open indefinitely against a shutdown,
  and a bounded drain is a later decision if long-running pulls appear.

## Consequences

- `Ctrl-C`, `docker stop`, and `docker kill --signal=SIGINT|SIGTERM` all
  stop the server within the docker grace period (no SIGKILL fallback).
- The fleet entrypoint (the stacked Docker PR) relies on this: the server
  child exits on signal, and the entrypoint kills the rest of the fleet.
