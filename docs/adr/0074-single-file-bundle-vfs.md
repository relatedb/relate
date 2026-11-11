# 0074. Single-file bundle VFS selected by `.cl` path

- **Status**: Accepted
- **Date**: 2026-09-21
- **Deciders**: CypherLite maintainers

## Context

ADR-0072 introduced a synchronous `Vfs` so WAL and snapshots share
one I/O boundary. Native directories and in-process `MemoryVfs` shipped
first. The remaining product need is **one host file** that still presents
the existing virtual tree (`wal/00000001.log`, `manifest.json`,
`nodes/…`).

ADR-0050 rejected a single-file *snapshot codec* (`.db` as GraphData).
This decision is a container VFS, not a codec. ADR-0072 listed a
dedicated URI scheme as a follow-on; a path suffix is enough to pick the
backend without advertising a scheme `Database::open` would otherwise
reject (ADR-0044).

## Decision

1. **One host file.** Dual superblocks, the inode catalog, WAL extents,
   and snapshot objects live in a single regular file. No sidecar
   `-wal`/`-shm`, no OS `wal/` directory, no OS `.tmp` files. Virtual
   `{path}.tmp` is an inode in the same container; `atomic_write` rename
   is a catalog pointer swap.

2. **URI selection.** After stripping `file://`, if the path ends with
   `.cl` (ASCII, case-insensitive, ignoring a trailing `/` or `\`) the
   engine opens `BundleVfs`. Every other filesystem URI stays
   `NativeVfs`. `:memory:` is unchanged. No `bundle://` scheme.

3. **Create vs open.** A missing `foo.cl` creates a new container.
   An existing **directory** named `*.cl` is an error. An existing
   regular file without bundle magic is an error.

4. **`OpenOptions.wal_dir` is rejected** on `.cl` URIs. A second host
   WAL would break “one file.”

5. **Durability (ADR-0077).** The container fd uses the same platform
   barrier as Native (`fdatasync` / `F_BARRIERFSYNC` / `sync_all`).
   Group-commit `durability_barrier` writes WAL tail pages, then a
   superblock generation (WAL-tail shortcut: inode id + logical length +
   file page count). Catalog pages are rewritten only when extents or
   names change (create, rename, delete, rotate’s new segment). Crash
   recovery takes the higher valid superblock checksum; unreachable pages
   join the freelist.

6. **Not in this change:** shrink/compaction, mmap as source of truth,
   OPFS, object-store VFS.

## Alternatives considered

- **Dedicated `bundle://` scheme** — rejected: `.cl` on the existing
  path/`file://` surface is enough, and ADR-0044 forbids advertising a
  scheme the engine does not open.
- **Treat any existing regular file as a bundle** — rejected: a stray
  non-magic file would fail opaquely; the suffix is the contract.
- **Dump `MemoryVfs` on every `sync`** — rejected: group commit would
  rewrite snapshots and closed WAL segments on every fsync.
- **Zip/tar/SQLite as the container** — rejected: zip cannot append a
  WAL cheaply; embedding SQLite adds a second durability story.

## Consequences

- `Database::open("./graph.cl")` is a durable single-file database.
- Native directory layout is unchanged for paths that do not end in `.cl`.
- Codecs still own `manifest.json` / shards / `graph.json` (ADR-0050).
- Loadtest baselines stay Native directories (ADR-0070).

## Later references

ADR-0098 proposes replacing the fixed virtual `{path}.tmp` name with an
unpredictable, exclusively created virtual temp inode. The temporary file
remains inside the single `.cl` host file; no sidecar is introduced.

ADR-0091 proposes quarantining **retired** catalog/inode pages reachable from
the selected old superblock until its replacement has passed a durability
barrier; the earlier statement that unreachable pages immediately join the
freelist does not license reuse of pages still needed by the old generation.
This does not protect a live partial WAL page from a torn in-place append;
the ADR-0074/0077 WAL-tail path remains a separate unresolved durability
boundary. The bundle remains one host file with the same on-disk format.

ADR-0093 proposes copying any selected partial WAL page before an append can
rewrite it, then publishing its v1 extent mapping with a new catalog and two
host barriers. Once implemented, that remap supersedes this ADR's catalog-free
WAL-tail shortcut; the one-host-file and format-v1 read contracts remain.
