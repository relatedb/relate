---
type: adr
area: surface
name: 0249-docker-unified-serve-image
status: accepted
tags:
  - serve
title: Docker image serves Bolt + HTTP + MCP from one `cypherlite serve` process
REFERENCES:
  - "[[adr/0128-docker-bolt-server-image]]"
  - "[[adr/0232-network-surfaces-charter]]"
  - "[[adr/0248-serve-host-flag]]"
SUPERSEDES:
  - "[[adr/0128-docker-bolt-server-image]]"
---

# ADR-0249: Docker image serves Bolt + HTTP + MCP from one `cypherlite serve` process

## Context

[ADR-0128](./0128-docker-bolt-server-image.md) built the container image around the bolt-only
`cypherlite-bolt` binary: `CMD --bind 0.0.0.0:7687 --uri /data/graph
--allow-plaintext`, `EXPOSE 7687 7474` with 7474 declared "reserved for
the future HTTP surface". That future arrived: `cypherlite serve`
([ADR-0232](./0232-network-surfaces-charter.md)) hosts the HTTP Query API v2, MCP, and Bolt surfaces
in-process on one open store with one shared credential ([ADR-0232](./0232-network-surfaces-charter.md) §1),
and [ADR-0248](./0248-serve-host-flag.md) gives it the `--host` flag a container needs. Keeping the
image bolt-only would force anyone wanting the HTTP or MCP surface in
Docker to hand-roll a container, and running three single-protocol
processes against one store would triple the seeded-store coordination
the entrypoint exists to do once.

## Decision

The image's primary runtime becomes **`cypherlite serve`** — one
process, all three surfaces, one credential:

- `CMD ["--host", "0.0.0.0", "--database", "/data/graph", "--bolt",
  "7687"]`. The port map is Bolt 7687, HTTP Query API v2 7474 (the Neo4j
  HTTP convention, already reserved), MCP streamable-HTTP 7475; all three
  are `EXPOSE`d. `--http`/`--mcp` stay out of the CMD on purpose: the
  entrypoint appends their defaults for a single run and derives
  non-overlapping bases under `INSTANCES=N`, which an explicit CMD pin
  would defeat.
- The `entrypoint` binary stays the PID-1 seed-then-serve supervisor
 ([ADR-0128](./0128-docker-bolt-server-image.md)) but now execs/supervises `/bin/cypherlite serve`. Its arg
  mapping retargets: `--uri` is a legacy alias of `--database`; `--bind
  HOST:PORT` is a legacy spelling of `--host HOST --bolt PORT` (the
  fleet port base); `--allow-plaintext` is accepted and dropped with a
  note — `serve` is plaintext-only and `--host 0.0.0.0` is itself the
 deliberate, loudly-warned exposure ([ADR-0248](./0248-serve-host-flag.md)). When argv omits them,
  the entrypoint appends the image defaults (host `0.0.0.0`, ports
  7687/7474/7475, database `/data/graph`) so the seeded store is the one
  served.
- `INSTANCES=N` fleet semantics are unchanged except the port step now
  applies consistently to all three ports: instance *i* gets base+*i*−1
  on each surface. An unset `--http`/`--mcp` base derives from the bolt
  base so the three stepped ranges can never overlap — bolt B..B+N−1,
  http B+N..B+2N−1, mcp B+2N..B+3N−1 (the adjacent single-run defaults
  7474/7475 would collide under stepping; measured: INSTANCES=2 bound
  instance 2's HTTP onto instance 1's MCP port). Explicitly-set bases
  must be pairwise N apart, a loud startup error otherwise. Per-instance
  suffixed URIs are unchanged.
- Auth is the shared [ADR-0232](./0232-network-surfaces-charter.md) story: `CYPHERLITE_AUTH` (or `--auth`, or
  the generated token in `docker logs`) is one credential that unlocks
  all three surfaces.
- The `cypherlite-bolt` binary **stays in the image** as the
  single-protocol alternative: it is the only shipped server with TLS
  (`--tls-cert`/`--tls-key`), both binaries are musl-static and ride the
  same cargo-chef layer, so keeping it costs a few MB of scratch-image
  size against losing the TLS-capable path. `cypherlite` stays for
  seeding and ad-hoc queries (`docker exec`).
- Seeding (`/config/seed.json`, env fallbacks) is unchanged — it runs
  `cypherlite` CLI steps before any server starts.

## Alternatives considered

- **Keep the image bolt-only, document a "build your own" recipe for
  serve**: rejected — the three-surface server is the product's network
  story; shipping the container one surface behind it strands exactly
  the users the image serves (dev/test deployments).
- **Three processes (bolt + http + mcp) under the fleet supervisor**:
  rejected — triple the stores-open coordination for a worse auth story
  (three resolutions instead of one); `serve` exists precisely to host
  them in one process.
- **Drop `cypherlite-bolt` from the image**: rejected — it is the only
  TLS-capable server shipped, and its marginal cost in the static,
  chef-cached build is negligible.
- **Rename the image tag** (`cypherlite-bolt` → `cypherlite-serve`):
  rejected for now — the tag is wired through CI caching, the smoke
  suite, and published docs; the image keeps its historical name while
  its contents generalize. A rename is a separate distribution decision.

## Consequences

- [ADR-0128](./0128-docker-bolt-server-image.md)'s bolt-specific decisions (CMD, EXPOSE set, the bolt-binary
  arg pass-through) are superseded by this ADR; its scratch/chef/UID
  1000/`/data` volume/PID-1-signal decisions stand unchanged. A **Later
 references** note records this on [ADR-0128](./0128-docker-bolt-server-image.md).
- `docs/api/bolt/spec.md`'s Docker section, the README Docker section,
  and `docs/api/http/spec.md` / `docs/api/mcp/spec.md` (which gain a
  Docker note) describe the three-port, one-credential story.
- The container smoke suite
  (`surfaces/cypherlite-bolt/tests/docker_smoke.py`) exercises all three
  ports and keeps pinning the legacy `--bind`/`--uri`/`--allow-plaintext`
  spellings as accepted aliases.
- `docker run` with no args now warns (loudly, by design per [ADR-0248](./0248-serve-host-flag.md))
  that `0.0.0.0` exposes plaintext surfaces; the warning names the
  posture the image documents.
