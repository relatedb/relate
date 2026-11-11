# ADR 0053: CypherLite product names

## Status

Accepted

## Context

After GraphStore withdrawal (ADR-0052) the product runtime already lives in
`cypherlite/`, but crate names, binaries, C libraries, TCK packages, CLI
flags, and env vars still say MemoryDB. Two CLIs, two C ABIs, and two TCK
crates cannot all be renamed to `cypherlite-*` without colliding with existing
CypherLite packages.

Clients implementing embedded Relate are outside this rename.

## Decision

In the `cypherlite/` workspace only:

- The embeddable library crate is `cypherlite`; the type is `cypherlite::Database`.
- Runtime crates are `cypherlite-core`, `cypherlite-front`, `cypherlite-planner`,
  `cypherlite-executor`, `cypherlite-storage`, `cypherlite-catalog`.
- The planner AST crate is `cypherlite-ir`. It is not a second Cypher parser;
  `cypherlite-parser` remains the query lexer/AST; `cypherlite-front` lowers it
  to planner IR.
- One CLI: binary `cypherlite`. Flags `--memory-limit`, `--wal-dir`, and
  `--checkpoint-interval-secs` live here. Package `memorydb-cli` / binary
  `memorydb` is deleted.
- Product C ABI remains `cypherlite_*` (`cypherlite-ffi`) for Swift and the
  published header. Bolt language clients keep a distinct shared library
  (`cypherlite-bolt-ffi`, formerly `memorydb-ffi`, C symbols still `memorydb_*`).
  The two ABIs are not merged in this change.
- Strict TCK stays `cypherlite-tck` (3877 pass, 3 documented skips). The 3897/3897
  openCypher floor is `cypherlite-tck-opencypher` (formerly `memorydb-tck`).
  Feature corpora are not merged.
- Canonical env vars are `CYPHERLITE_SNAPSHOT_FORMAT`, `CYPHERLITE_MEMORY_LIMIT`,
  `CYPHERLITE_CHECKPOINT_INTERVAL_SECS`, `CYPHERLITE_COMMIT_TIMING`,
  `CYPHERLITE_SHARD_TARGET_BYTES`, `CYPHERLITE_PARSER`. If unset, the matching
  `MEMORYDB_*` name is still read.
- Clients implementing embedded Relate, and the `../memorydb` quarry,
  are out of scope.

## Alternatives considered

- Rename crates before collapsing dual stacks — rejected because `cypherlite-cli`,
  `cypherlite-ffi`, `cypherlite-tck`, and `cypherlite-parser` already exist.
- Keep `cypherlite::Database` with renamed directories — rejected; the product
  name is CypherLite.
- Merge Bolt `memorydb_*` C symbols into `cypherlite_*` now — rejected; the
  symbol sets and client tests differ; that is a later ABI project.
- Hard-cut env vars with no `MEMORYDB_*` fallback — rejected for this pass so
  existing directories and scripts keep working.

## Consequences

- Embedders of this tree use `cypherlite::Database`. Clients implementing
  embedded Relate are outside this rename.
- Docs and CI must use the new package names in the same change.
- On-disk WAL magic (`MDBWAL01`) is unchanged.

## Later references

- [ADR-0054](0054-one-c-abi-env-python.md) merges the C ABI, hard-cuts
  `MEMORYDB_*` env reads, vendors TCK only under `testdata/tck`, and makes
  PyO3 `cypherlite` the only Python package.
