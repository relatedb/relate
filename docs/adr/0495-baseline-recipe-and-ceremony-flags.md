---
type: adr
area: infra
claim: 3c602f8c6c08414a14ac7df518dcff23c86bd8ba
name: 0495-baseline-recipe-and-ceremony-flags
status: proposed
tags:
  - ci
  - loadtest
title: One baseline image recipe, ceremony-owned flags
REFERENCES:
  - "[[adr/0046-cypherlite-parser-memorydb-runtime]]"
  - "[[adr/0070-docker-baseline-loadtest-environment]]"
  - "[[adr/0071-ci-owned-loadtest-freeze-fingerprint]]"
  - "[[adr/0289-graph-shape-memory-pins]]"
  - "[[adr/0333-processing-memory-controls]]"
  - "[[adr/0390-serve-memory-limit-flag]]"
  - "[[adr/0484-bolt-5-4]]"
---

# ADR-0495: One baseline image recipe, ceremony-owned flags

## Context

[ADR-0070](./0070-docker-baseline-loadtest-environment.md)'s Docker recipe existed once per image-built lane, and the copies
(`Dockerfile.memory-shape-loadtest`, `Dockerfile.processing-memory-loadtest`)
are the same file except for three facts: the cargo binaries the lane
measures, the suffix of its BuildKit target-cache id and the container's
entrypoint. The CA-bundle install and the cache mounts are carried per copy,
so a fix in one is a fix owed in the other. The lane scripts
meanwhile share `scripts/baseline-container.sh` for build/create/log/copy/stamp,
but each still carries its own path-flag handling beside the ceremony's —
several spellings of one rule is several places to get the next flag wrong.

## Decision

1. **One build recipe: `Dockerfile.baseline`.** The lane supplies what makes
   it itself through three build args: `LANE_BINS` (space-separated cargo
   binary names, built and copied to `/usr/local/bin`), `LANE_CACHE_ID` (its
   BuildKit target-cache suffix, so one lane's cargo cache is not another's)
   and `LANE_ENTRYPOINT` (the container command, exec'd through one `sh -c`
   shim because the exec form cannot carry a build arg). The per-lane
   Dockerfiles are deleted. A build with an empty `LANE_BINS` or
   `LANE_CACHE_ID` fails with an authored message instead of building the
   workspace's default members.

2. **`scripts/baseline-container.sh` owns every ceremony flag.** `--out` is
   always its own: a caller copy (`--out <path>` or `--out=<path>`) is
   dropped from the forwarded binary args and re-appended once as
   `/report/<basename>`; the host path is resolved against the repo root
   once. Each lane declares the rest of its path flags with `--path
   <flag>=<value>`: the ceremony applies the same strip-and-re-append rule,
   resolves the value against the repo root once, creates it and bind-mounts
   it (a bind mount keeps it off the `/tmp` tmpfs, whose size counts against
   the cgroup memory limit). The lane scripts keep only their own flags, and
   the container memory cap rides the `BASELINE_MEMORY` environment with a
   `2g` default — no lane overrides it today, so a cap flag returns with the
   first lane that needs one.

3. **A session's stalled statement fails with authored text.** The Bolt
   session's socket read timeout (`DEFAULT_READ_TIMEOUT`, 1800 s) is the
   statement's own bound: a statement that produces no byte for that long
   fails with the authored timeout text, never the platform's socket
   error, and the run reports that query as failed.

## Alternatives considered

- **Keep the per-lane Dockerfiles with a shared base stage** — rejected: a
  base stage still leaves one file per lane to keep in step for the same
  three values, and the difference is data (build args), not build logic.
- **A generic `--build-arg NAME=VALUE` passthrough in the ceremony** —
  rejected: the recipe needs exactly three facts a lane can get wrong, and
  naming them in the ceremony lets it validate the required two up front
  instead of failing inside `cargo build` with no bins.
## Consequences

- One Dockerfile and one flag-owning ceremony; a new image-built lane passes
  three build args and declares its path flags, and inherits the `--out`
  handling, the bind-mount rule and the `--clear` guard.
- `scripts/tests/test_baseline_container.sh` asserts the ceremony's flag
  refusals without a Docker daemon (they run before the docker lookup) and
  the lane scripts' env-knob contracts; the shared Bolt session's unit tests
  cover the timeout's authored error against a stalled listener.
- `docs/datasets.md` (bench flag tables and lane knobs), `docs/testing/
  memory-testing.md`, `AGENTS.md` and the lane scripts point at
  `Dockerfile.baseline`.
- A first Docker build of a lane with a new cache id is a cold cargo build;
  the per-lane ids stay, so the lane caches do not evict each other.

References: [ADR-0046](./0046-cypherlite-parser-memorydb-runtime.md), [ADR-0070](./0070-docker-baseline-loadtest-environment.md), [ADR-0071](./0071-ci-owned-loadtest-freeze-fingerprint.md), [ADR-0289](./0289-graph-shape-memory-pins.md), [ADR-0333](./0333-processing-memory-controls.md), [ADR-0390](./0390-serve-memory-limit-flag.md),
[ADR-0484](./0484-bolt-5-4.md).
