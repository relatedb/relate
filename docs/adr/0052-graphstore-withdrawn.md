# ADR 0052: GraphStore withdrawn

## Status

Accepted

## Context

CypherLite’s product runtime is `cypherlite::Database` (directory + WAL + snapshot
codec). GraphStore (`cypherlite-engine` + `cypherlite-storage` + Lance + Neo4j
`--sync`) remained as CLI flags (`--legacy`, `--server`, `--set-access`,
`--sync`) and as the live Engine CIP oracle.

MemoryDB already matched Engine CIP scenario identities. Keeping two runtimes
split language coverage and blocked deleting unused crates.

## Decision

GraphStore is withdrawn from the product:

- The CLI is MemoryDB-only. `--legacy`, `CYPHERLITE_RUNTIME`, `--server`,
  `--sync` / `--sync-shape`, and `--set-access` are removed.
- Crates `cypherlite-engine`, `cypherlite-storage`, `cypherlite-lance`, and
  `cypherlite-sync` are deleted.
- Constraint types, semantic validation, graph-type compilation, SHOW catalogs,
  and vector similarity live in `cypherlite-model` / `cypherlite-front` /
  `cypherlite-executor`.
- CIP is gated on MemoryDB results versus `baselines/cip-eval-memorydb.json`
  **and** the frozen Engine-passing identity set in
  `testdata/cip-eval/ENGINE_PASSING.json` (captured while Engine still existed).
  The live Engine CIP oracle is gone.

TCK remains full coverage: `cypherlite-tck-opencypher` 3897/3897; `cypherlite-tck` 3877 pass
with the three documented skips.

## Consequences

- Sync and JSONL server can return later on `Database` if needed; they are not
  GraphStore revival.
- Engine CIP totals must not be ratcheted downward to hide a coverage drop.

## Later references

- [ADR-0053](0053-cypherlite-product-names.md) names the remaining runtime crates
  and CLI as CypherLite.
- Correction to the Decision crate bullet: `cypherlite-storage` still exists.
  Its GraphStore-era implementation was deleted and replaced by the current
  storage stack (Parquet + object_store + WAL + checkpoint; see the crate
  layout in `AGENTS.md`). Read the bullet as "`cypherlite-engine`,
  `cypherlite-lance`, and `cypherlite-sync` are deleted; `cypherlite-storage`
  is deleted and replaced by the new WAL/checkpoint implementation under the
  same crate name."
- [ADR-0080](0080-cli-pull-command.md) returns Neo4j/Aura subgraph pull on
  `cypherlite::Database` as the positional `cypherlite pull` command (HTTP
  Query API v2, CREATE + checkpoint). It is not GraphStore revival.
- This ADR was accepted without an "Alternatives considered" section. Recorded
  here per the immutability rule:
  - **Keep GraphStore as a second product runtime** — rejected: two runtimes
    split language coverage and block deleting unused crates; MemoryDB already
    matched Engine CIP scenario identities.
  - **Keep Engine only as the live CIP oracle, not a product runtime** —
    rejected: a live oracle keeps the engine/storage/sync crates buildable and
    maintained; the frozen `testdata/cip-eval/ENGINE_PASSING.json` identity
    set preserves the gate without the code.
  - **Delete the `cypherlite-storage` crate outright** — rejected: the new
    WAL + Parquet-checkpoint storage stack needs a home; only the old
    implementation was gutted, and the crate name was kept.
- [ADR-0548](0548-query-engine-seam.md) is not a second product runtime.
  The language front end stays shared and one engine implements it.
