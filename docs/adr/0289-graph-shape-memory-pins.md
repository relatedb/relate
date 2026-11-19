---
type: adr
area: infra
date: 2026-10-08
deciders: CypherLite maintainers
name: 0289-graph-shape-memory-pins
status: accepted
tags:
  - loadtest
  - memory
title: Graph-shape logical memory pins and Bolt RSS baseline
REFERENCES:
  - "[[adr/0042-graph-memory-limit]]"
  - "[[adr/0070-docker-baseline-loadtest-environment]]"
  - "[[adr/0071-ci-owned-loadtest-freeze-fingerprint]]"
  - "[[adr/0495-baseline-recipe-and-ceremony-flags]]"
---

# 0289. Graph-shape logical memory pins and Bolt RSS baseline

- **Renumbered**: from 0188 (shared-number cleanup)

## Context

[ADR-0042](./0042-graph-memory-limit.md) caps a logical size of the live graph plus the indexes that
enter `IndexCharge`. That counter is a closed formula. The same nodes,
relationships, property bytes, and index charge produce the same integer
on every OS and allocator. Process RSS is a different meter: it includes
the server runtime, query scratch, and allocator slack, and it depends
on graph shape. [ADR-0042](./0042-graph-memory-limit.md) rejected RSS as the cap for that reason.

The cloud sizing job already showed the split. Property-heavy graphs and
relationship-dense meshes with the same logical size do not have the
same RSS. CypherLite had no engine gate for either fact. The fraud and
identity loadtests sample RSS of their own process, so the harness is
part of the measurement, and they compare logical bytes with a 10% band
because those runs are timed mixes.

## Decision

Two suites share one Cypher generator
(`testing/cypherlite-datasets/src/shapes.rs`).

**Logical pins** run in ordinary `cargo test`
(`make test` → `cargo test -p cypherlite-datasets --test memory_shapes`).
Shapes are `base`, `ring`, `fanout`, `flat`, and `dense ring`, at the
small scale. `ring` is a directed cycle (one `NEXT` per node, including
the wrap to id 0). `flat` has no relationships. `dense ring` keeps
degree 20 and also wraps. Every node carries `ATTRIBUTES` integer
properties (default 1; the first is `id`). `TEXT_INDEXES`,
`POINT_INDEXES`, and `RANGE_INDEXES` (each default 0) add that many
indexed attributes of each type on every filled shape. Text and point
properties sit on top of the integer list. Range indexes the first
integer properties and must not exceed `ATTRIBUTES`. The unindexed
small pins stay exact. One flat graph with one of each index is pinned
to its own integer: text and point move `memory_usage`, and range does
not. One file-backed `ring` graph is reopened and must report the same
`used` (replay always applies, [ADR-0042](./0042-graph-memory-limit.md)).
Asserts are exact. They do not use the loadtest 10% band.

**RSS** is `memory-shape-loadtest`, a new [ADR-0070](./0070-docker-baseline-loadtest-environment.md) baseline target.
`make memory-shape-loadtest` and `make baseline-loadtests` build
`Dockerfile.memory-shape-loadtest`: one filtered context, BuildKit caches
for the Cargo registry and `target/`, compile in `rust:1.88-bookworm`,
and a runtime image of `debian:bookworm-slim` plus the two binaries. The
run is 2 vCPU. This target's container is 2 GB (`MEMORY_SHAPE_MEMORY`).
Ctrl+C removes that container. Fraud, identity, and checkpoint stay on
`baseline-env.sh` and the 2 GB cap.
`BASELINE_ENV=host` stays a debug escape and must not be frozen.

For each large shape the binary loads the graph in-process on a
filesystem directory under `URI_MEMORY_SHAPE` (default
`/tmp/memory-shape-loadtest-<run>`), reads `memory_usage`, checkpoints,
and records the full size of that shape's directory, including the WAL.
It then drops that database. It spawns `cypherlite-bolt` on a sibling
directory (`<shape>-bolt`, removed after the sample) with `--auth none`
and runs the same Cypher over Bolt. RSS is the child's `VmRSS` (minimum of several
samples) and `VmHWM` from `/proc/<pid>/status`. The test process is
not the sample. The matrix runs `base`, `flat`, `fanout`, `ring`,
and `dense ring`, all with the same index counts. `base` has no index
DDL. `base` RSS is subtracted. The gated value is
`(rss − base) / logical_bytes`.

Logical bytes in the report must match the frozen file exactly. The
RSS ratio may be up to 15% above the frozen ratio
(`LOADTEST_MAX_RSS_RATIO`, default `1.15`). The freeze is the existing
worst-of-N envelope: the highest ratio per shape wins
(`BASELINE_RUNS`). Each filled shape is loaded until the database
reports the logical memory limit `LOGICAL_MB` (default 32, where MB is
1024×1024). The batch that crosses the limit is rejected and not
committed, so the measured size sits at most one batch under the limit.
`base` stays empty. The container cap is
`MEMORY_SHAPE_MEMORY` (default `2g`). `ATTRIBUTES` (default 1) is the
integer property count per node. `TEXT_INDEXES`, `POINT_INDEXES`, and
`RANGE_INDEXES` (each default 0) are the indexed-attribute counts. The
report records them, and each row's `index` field is `none` or a token
such as `text2+point1+range1`. The report prints logical bytes, the
full database-directory size, and Bolt RSS in MB, and records the cgroup
cap when the run is inside Docker. The first release run, at ~30 MB
logical and under the earlier names, showed RSS ratios from about 1.5×
(`rich` + `TEXT`) to about 42× (`dense`). At the 32 MB default that
dense-ring RSS is about 1.3 GB, and the 15% band stays inside the 2 GB
cap. A 128 MB logical target is about 5.3 GB RSS and needs a higher
`MEMORY_SHAPE_MEMORY`.

The envelope is CI-owned ([ADR-0071](./0071-ci-owned-loadtest-freeze-fingerprint.md)). Compare runs when
`baselines/memory-shape-loadtest.json` is present. Until the first
`create-loadtest-baselines` freeze, `make compare-loadtests` skips
this file. Do not commit a host-native report.

Fraud and identity keep their in-process RSS fields. This target does
not replace them.

## Alternatives considered

- **Pin both meters in `cargo test`.** Rejected. RSS is not stable
 across laptops, debug builds, and architectures. [ADR-0070](./0070-docker-baseline-loadtest-environment.md) already
  refuses to freeze host-native loadtest reports.
- **Sample RSS inside `fraud-loadtest`.** Rejected. That process
  includes the harness, and its envelope is throughput and latency.
  A footprint change and an ops/s change re-freeze separately.
- **Exact RSS equality.** Rejected. Allocator retention and the Bolt
  server's fixed cost move the number without a logical-byte change.
  The 15% band is the gate; the logical counter stays exact.
- **Read `memory_usage` from the child.** Rejected. No Cypher
  procedure returns it. The generator is deterministic, so the
  in-process probe is the same integer the child holds.

## Consequences

- A formula change updates the small-scale integers in the same commit.
  The RSS baseline's logical bytes move only through the CI freeze.
- The image build sends `Cargo.toml`, `Cargo.lock`, `cypherlite/`,
  `surfaces/`, and `testing/` (`.dockerignore`). `examples/`,
  `node_modules`, and `.venv` stay on the host. Both binaries are
  copied to `/usr/local/bin` in the runtime image.
- Agents freeze this suite only via `make memory-shape-loadtest` or
 `make baseline-loadtests` in the [ADR-0070](./0070-docker-baseline-loadtest-environment.md) environment.

## Later references

- [ADR-0495](./0495-baseline-recipe-and-ceremony-flags.md): the memory-shape
  lane's image is built by the shared `Dockerfile.baseline` (bin list,
  cache-id suffix and entrypoint as build args), not by
  `Dockerfile.memory-shape-loadtest`.
