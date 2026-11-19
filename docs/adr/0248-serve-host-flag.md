---
type: adr
area: surface
name: 0248-serve-host-flag
status: accepted
tags:
  - security
  - serve
title: |-
  `cypherlite serve --host HOST` — explicit bind host, loopback default, loud plaintext posture
REFERENCES:
  - "[[adr/0109-bolt-tls]]"
  - "[[adr/0128-docker-bolt-server-image]]"
  - "[[adr/0232-network-surfaces-charter]]"
  - "[[adr/0249-docker-unified-serve-image]]"
---

# ADR-0248: `cypherlite serve --host HOST` — explicit bind host, loopback default, loud plaintext posture

## Context

`cypherlite serve` ([ADR-0232](./0232-network-surfaces-charter.md)) binds every served surface to
`127.0.0.1` only — the DNS-rebinding-safe default for a developer
workstation, shared with the Browser-request guards of the HTTP surface.
That default is unreachable from outside a container: a Docker container
([ADR-0128](./0128-docker-bolt-server-image.md), now [ADR-0249](./0249-docker-unified-serve-image.md)) must bind `0.0.0.0` to be reachable from the
host at all, and a pod/VM deployment needs the same. The standalone
servers already expose the choice: `cypherlite-bolt` has `--bind
HOST:PORT` gated by `--allow-plaintext` ([ADR-0109](./0109-bolt-tls.md)), and `cypherlite-mcp`
has `--host HOST`.

`serve` needed the same escape hatch, shaped to its own flag model: its
surface flags carry bare ports (`--http PORT`, `--mcp PORT`, `--bolt
PORT`), so a bolt-style `--bind HOST:PORT` would double-encode the port
three times over.

## Decision

`cypherlite serve` gains **`--host HOST`** (double-dash kebab, per the
CLI conventions):

- Default `127.0.0.1` — loopback only, unchanged for every existing
  invocation.
- One value applies to every served surface; the per-surface port flags
  stay as they are. A repeat is a usage error, like the other
  once-only flags.
- Any non-loopback value is a **deliberate plaintext exposure**: at
  startup the server prints a loud `WARNING` on stderr naming the host
  and stating that the surfaces are plaintext (serve has no TLS) and
 reachable from the network. The warning is the mirror of [ADR-0109](./0109-bolt-tls.md)'s
  posture for Bolt: binding beyond loopback without transport
  encryption is risk-taking the operator must do on purpose.
- The HTTP discovery document's `bolt_direct` and the listening banner
  lines report the configured host.

The name is `--host` (matching `cypherlite-mcp`), not `--bind`: `serve`
already owns three port flags, and `--bind HOST:PORT` would collide
with them shape-wise.

## Alternatives considered

- **Bolt's exact two-flag model** (`--bind HOST:PORT` plus a refusal of
  non-loopback plaintext unless `--allow-plaintext`): rejected — `serve`
  has no TLS mode to opt out of, so the second flag would be ceremony
  attached to every non-loopback run; the explicit `--host` value is
  itself the opt-in, and the loud warning keeps the risk in the logs.
  `--bind` was also the wrong shape next to three separate port flags.
- **Per-surface host flags** (`--http-host`, …): rejected — one process,
  one posture; mixed bind hosts would complicate the banner, the
  discovery document, and the container port story for no known need.
- **An env var instead of a flag** (`CYPHERLITE_HOST`): rejected — bind
  posture is a deliberate per-run decision and belongs in argv where
  `--help` documents it; the container passes it through the `CMD`.

## Consequences

- The Docker image ([ADR-0249](./0249-docker-unified-serve-image.md)) runs `cypherlite serve --host 0.0.0.0 …`
  as its `CMD`, and the warning line in `docker logs` is the expected,
  documented posture there.
- `serve`'s `--help`, the flag table in `docs/api/cli/spec.md`, and the
  suggestion list name `--host` alongside the surface flags.
- The loopback default is unchanged; nothing about an existing
  loopback-only `serve` invocation shifts.
