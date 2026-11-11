# 0066. Retire the MemoryDB name in favor of cypherlite

- **Status**: Accepted
- **Date**: 2026-09-19
- **Deciders**: CypherLite / MemoryDB merge (post-merge cleanup)

## Context

The CypherLite/MemoryDB merge (ADR-0046) grafted CypherLite's dialect and UX
onto MemoryDB's WAL + CSR + planner/executor, and imported MemoryDB's name
along with its runtime. ADR-0052 then established the withdrawal pattern, and
ADR-0065 withdrew the last two unsupported surfaces from that merge. The
branding question was left open by both: ADR-0065 removed the WASM and Swift
surfaces without touching the names still embedded in client manifests,
baselines, environment variables, and diagnostics.

The MemoryDB project is archived and no longer exists as a product. The
maintainer's decision is explicit: "memorydb does not exist anymore, it's
archived and should be forgotten about." CypherLite is the only live product
name. Yet live surfaces still carry the old one:

- the npm package is published as `memorydb`
- the Go module path is `github.com/memorydb/memorydb/clients/go`
- the Java client uses groupId/package `com.memorydb`, including the
  `MemoryDbException` type
- the CIP-eval scoreboard is `baselines/cip-eval-memorydb.json`, a merge gate
  referenced by `AGENTS.md`, the `Makefile`, and CI
- CI/test environment variables are prefixed `CIP_EVAL_MEMORYDB_*`
- the `CYPHERLITE_PARSER` diagnostic parser value is `memorydb`

Every one of these misinforms a new reader: the archived project appears to
still be a live product surface. Retiring the name is the same discipline as
ADR-0052/ADR-0065 — the tree should advertise only what is actually supported
and shipped.

## Decision

Retire the MemoryDB name from every live surface. The product name is
**cypherlite** everywhere it is visible. The new coordinates are:

- **npm package**: `cypherlite` (was `memorydb`).
- **Go module**: `github.com/neo4j-labs/cypherlite/clients/go` (was
  `github.com/memorydb/memorydb/clients/go`).
- **Java**: groupId/package `com.cypherlite` (was `com.memorydb`); the
  exception type `MemoryDbException` becomes `CypherLiteException`.
- **CIP scoreboard**: `baselines/cip-eval.json` (was
  `baselines/cip-eval-memorydb.json`), with its references in `AGENTS.md`, the
  `Makefile`, and CI updated to match.
- **Environment variables**: `CIP_EVAL_*` (was `CIP_EVAL_MEMORYDB_*`).
- **Diagnostic parser value**: `CYPHERLITE_PARSER=planner-ir` (was
  `CYPHERLITE_PARSER=memorydb`); the historical parser path is unchanged, only
  its diagnostic label is renamed.

The following are **exclusions** and stay untouched, because rewriting them
would falsify history rather than retire a live surface:

- immutable Accepted ADRs under `docs/adr/` — their text records decisions as
  they were made and is not edited;
- released `CHANGELOG.md` sections — shipped history is not retroactively
  rewritten;
- frozen research notes under `testdata/cip-eval/**/NOTES.md` — these are
  point-in-time research artifacts, not live product surfaces.

This ADR records the naming decision only. The mechanical rename of each
coordinate to the values above is carried out by the accompanying cleanup.

## Alternatives considered

- **Keep memorydb branding in clients and internals** — rejected: the product
  name is retired; stale branding misinforms every new reader.
- **Rename only the public client packages and leave repo internals (the
  scoreboard, env vars, comments) alone** — rejected: a half-renamed repo
  repeats the seam problem this cleanup exists to eliminate; the CIP scoreboard
  is a merge gate referenced by `AGENTS.md`, the `Makefile`, and CI, so it must
  move together with its references.

## Consequences

- A new reader sees one live product name, `cypherlite`, on every surface that
  ships or gates the product. The archived MemoryDB name survives only in the
  exclusions, where it is understood as history.
- The npm package, Go module path, and Java groupId/package are breaking
  coordinate changes for existing consumers. That is intended: the old names
  are retired, and clients must move to the new coordinates.
- The CIP-eval merge gate moves from `baselines/cip-eval-memorydb.json` to
  `baselines/cip-eval.json`. Its references in `AGENTS.md`, the `Makefile`, and
  CI move in the same change, so the gate does not break. The frozen baseline
  content is unchanged — only its path.
- `CIP_EVAL_*` replaces `CIP_EVAL_MEMORYDB_*` for tests and tooling;
  `CYPHERLITE_PARSER=planner-ir` replaces `=memorydb` for diagnostics. Old
  values are no longer honored.
- ADR-0046's imported runtime is unaffected: this retires the name, not the
  WAL/CSR/planner work the merge brought in. ADR-0065's surface withdrawals are
  also unaffected; this ADR closes the branding question that ADR-0065 left
  open.
- Reinstating the MemoryDB name would require a new ADR; this one is the
  recorded decision that it is retired.
- This ADR retires the MemoryDB name only inside this repository.
