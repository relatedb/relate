---
type: adr
area: engine
date: 2026-09-07
deciders: MemoryDB maintainers
name: 0044-withdraw-engine-s3
status: accepted
tags:
  - storage
title: Withdraw engine-level S3
REFERENCES:
  - "[[adr/0004-memory-first-parquet-object-store]]"
  - "[[adr/0012-wal-and-parquet-checkpoints]]"
  - "[[adr/0014-single-wal-write-path]]"
  - "[[adr/0022-id-window-checkpoint-shards]]"
  - "[[adr/0029-withdraw-localhost-http]]"
  - "[[adr/0072-virtual-filesystem]]"
---

# 0044. Withdraw engine-level S3

## Context

[ADR-0004](./0004-memory-first-parquet-object-store.md) and [ADR-0012](./0012-wal-and-parquet-checkpoints.md) made `s3://` a first-class engine URI:
`AmazonS3Builder` for Parquet checkpoints, a local WAL sidecar
(`--wal-dir` / `MEMORYDB_WAL_DIR` / a temp hash), and a MinIO CI job.
Clients implementing embedded Relate already own hosted concerns
([ADR-0029](./0029-withdraw-localhost-http.md)). Keeping S3 in the engine couples every embed, CLI, and
default CI build to AWS `object_store` features the laptop/product
surfaces do not need.

## Decision

Remove engine `s3://`.

- Supported URIs are `:memory:`, a filesystem path, and `file://`.
  Opening `s3://` (or another remote scheme) fails with a storage
  error. The engine does not advertise a URI it cannot open.
- Object storage belongs to clients implementing embedded Relate.
- Keep Apache `object_store` **without** the `aws` feature, for
  `LocalFileSystem` and `InMemory` only.
- `--wal-dir` remains for filesystem URIs (default `<dir>/wal`).
  Drop S3-only `MEMORYDB_WAL_DIR` and `MEMORYDB_S3_*`.

## Alternatives considered

- **Optional `s3` Cargo feature** — still invites `--all-features` and
  keeps an AWS client in this repo. Clients implementing embedded
  Relate should own the object-store client. Rejected.
- **Keep documenting `s3://` as provided by an embedding client** — the
  engine would advertise a URI `Database::open` rejects. Rejected.
- **WAL as S3 objects** — `put` has no `fsync`. Already rejected in
 [ADR-0012](./0012-wal-and-parquet-checkpoints.md); still rejected.

## Consequences

- MinIO CI and `s3_minio_roundtrip` go away.
- Durable form in this repo is a fsync’d local WAL plus local Parquet
 checkpoints. `:memory:` stays process-ephemeral ([ADR-0014](./0014-single-wal-write-path.md)).
- [ADR-0004](./0004-memory-first-parquet-object-store.md) / [ADR-0012](./0012-wal-and-parquet-checkpoints.md) / [ADR-0022](./0022-id-window-checkpoint-shards.md) remain the history of the S3-era
  design; this ADR withdraws the engine URI, not those files.

## Later references

Post-merge repo hygiene (`gbuild/memdb-merge/10-repo-hygiene`, commit
`f077f3e` and its repair): `cypherlite_storage::parse_uri` tightens this
URI contract. A **single-slash** remote URI such as `s3:/bucket/prefix`
no longer falls through to the filesystem branch — where it created a
literal `s3:` directory under the process cwd, and one such tree was
committed by accident. Any first component that parses as a valid URI
scheme now fails with the same “unsupported URI scheme” storage error as
the `://` form, before any disk I/O. Trade-off: the strictness also
rejects a few odd but legal filesystem relative paths whose first
component looks like a scheme (e.g. `logs:2026/db`); Windows
drive-letter paths (`C:\db`, `C:/db`) are carved out and still parse as
filesystem paths. Recorded here instead of a new ADR because the change
is a strictness refinement of this ADR’s “supported URIs” decision, not
a new direction.

[ADR-0072](./0072-virtual-filesystem.md) replaces in-process `object_store` (local + memory) with a
synchronous `Vfs`. Engine `s3://` stays rejected; an object-store VFS is
follow-on work and must not advertise a URI until `Database::open` can
open it.
