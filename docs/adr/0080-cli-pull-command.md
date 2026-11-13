# 0080. CLI `pull` command: Query API v2 into `Database`

- **Status**: Proposed
- **Date**: 2026-09-22
- **Deciders**: CypherLite maintainers

## Context

GraphStore `--sync` pulled a Neo4j/Aura subgraph over HTTP Query API v2 from
an Aura/`neo4j-cli` env file and hydrated a read-only `MemoryStore`
(ADR-0052 withdrew that runtime). A client implementing embedded Relate
later seeded a live `cypherlite::Database` from a Cypher slice: extract
graph elements, `CREATE` them, checkpoint. Operators want that pull on
the laptop CLI without
reviving GraphStore, putting passwords on argv, or taking `neo4rs` in a
product crate.

Lifecycle verbs are positional commands (ADR-0069). `--sync` as a flag is
the same mistake as the removed `--skill` flag.

The first `pull` implementation always `CREATE`d. Extract interned Neo4j
`elementId` strings to integers for wiring start/end *inside one HTTP
response*, then discarded the strings. A second `pull` of the same query
therefore doubled the graph (for example, 41/42 nodes/rels, then 82/84).
Incremental pulls of a *different* overlapping query must add only the
new remote elements.

## Decision

1. **`cypherlite pull` is a first-positional lifecycle command**, not a
   flag. Grammar:

   ```text
   cypherlite pull [--from FILE] --query Q [--param N=V] DATABASE
   ```

   `./pull` and `cypherlite -- pull` remain database paths. The command
   exits after pull; it does not enter the REPL or run a follow-up query.

2. **Transport is HTTP Query API v2 via workspace `ureq`.** Product crates
   do not depend on `neo4rs`. Aura hosts use
   `https://{host}/db/{database}/query/v2` (Bolt port stripped; truncated
   `.databases.neo4j` completed to `.io`). `neo4j://host:7687` maps to
   `http://host:7474`. `http://` / `https://` URIs are Query API origins
   (tests and explicit HTTP endpoints).

3. **Credentials.** `--from FILE` is a `KEY=VALUE` env file
   (`NEO4J_URI`, `NEO4J_USERNAME`, `NEO4J_PASSWORD`, optional
   `NEO4J_DATABASE`, default `neo4j`) — the Aura “Created” / `neo4j-cli
   --env-out-file` format. The password is never an argv flag. When
   `--from` is omitted, the process environment supplies the same keys
   (`NEO4J_USER` is an alias of `NEO4J_USERNAME`). Values that look like
   `NEO4J_PASSWORD=secret` are stripped to `secret`.

4. **Shape query.** `--query` is required (file path or inline). There is
   no Movies default. The parser rejects a non-graph `RETURN`
   (scalars, aggregates, `*`, projections) before any network call.

5. **Materialize is a source-scoped union/upsert**, not always-`CREATE`
   and not a full replace of `DATABASE`.

   - Extract keeps the remote `elementId` string on each node and
     relationship. Integers are interned only to wire start/end *within
     one response*.
   - Identity key: `(source, kind, remote elementId)` where `source` is
     the normalized Query API origin plus `NEO4J_DATABASE` (the v2 URL).
     Username and password are not part of the key. Two Aura databases
     may both use `4:uuid:1`.
   - Hit and local element still exists: **reuse**. Nodes: `SET n =
     $props` (source properties win) and **union labels**. Relationships:
     `SET r = $props`.
   - Hit but the local element was deleted: treat as new and remap.
   - Miss: `CREATE` and register the mapping.
   - Elements not in this query are left alone (union, not
     replace-the-database).
   - Local edits on a pulled entity are overwritten the next time that
     entity is pulled.
   - Neo4j `elementId` is not stable across delete+recreate at the
     source; pull does not invent business keys such as `Account.id`.
   - The mapping is **not** stored as user-visible properties. It lives
     on `Graph` (`DashMap` plus reverse index so `DeleteNode` /
     `DeleteRel` drop stale entries), in WAL as additive `RedoOp::
     PullIdentityPut { source, kind, remote_id, local_id }` in the **same
     commit** as the `CREATE`/`SET` ops (one explicit transaction for the
     whole pull), and in snapshot as `pull-identity.json` at checkpoint
     (same durability boundary as `manifest.json`). A missing file or
     `#[serde(default)]` is an empty map; `FORMAT_VERSION` stays 3.
     Snapshot restore loads the map before WAL replay; replay applies
     further `PullIdentityPut`s and deletes. Identity ops are not graph
     CDC events. `:memory:` is rejected — pull must persist.
   - HTTP/Neo4j stay out of the `cypherlite` crate. `cypherlite-load`
     uses a narrow `Connection` upsert API.

6. **Library boundary.** Fetch, extract, and upsert live in
   `cypherlite-load`. The public `cypherlite` crate does not grow HTTP or
   Neo4j types. Seeding, authentication, named secrets, and any Bolt
   fallback stay with clients implementing embedded Relate.

## Alternatives considered

- **Revive `--sync` and GraphStore JSON persist.** Rejected: ADR-0052
  withdrew GraphStore; ADR-0069 forbids lifecycle flags.
- **Bolt / `neo4rs` in the CLI.** Rejected: `neo4rs` stays off
  product crates; Query API v2 covers Aura and default local HTTP.
- **Password flag / inline URI userinfo.** Rejected: argv leaks; the env
  file is how Aura credentials are already delivered.
- **Default Movies shape when `--query` is omitted.** Rejected: a demo
  query silently loading the wrong graph.
- **Allow `:memory:` and keep a REPL.** Rejected: pull is exit-after; an
  in-memory target cannot be the persisted artifact.
- **Name the command `load`.** Rejected: `pull` matches fetching a remote
  subgraph; `load` collides with Cypher `LOAD CSV` and dataset loadtests.
- **Always `CREATE` (status quo).** Rejected: repeat and overlapping
  pulls duplicate nodes and relationships.
- **Reserved user-visible properties** (for example `__cl_*`). Rejected:
  they leak into Cypher, `MATCH`, and exports.
- **Sidecar-only identity file without WAL.** Rejected: a crash between
  `CREATE` and writing the sidecar loses the mapping or leaves a map
  that does not match the graph. WAL + snapshot is crash-safe and
  invisible to Cypher.

## Consequences

- `--help`, `docs/api/cli/spec.md`, and the skill-bundle CLI reference
  list `pull` in the same change as the parser. `--help` stays a
  one-liner; union/upsert details live in docs.
- Success prints
  `pulled N new / M reused nodes, P new / Q reused rels to PATH`.
- Directory stores and `.cl` bundles include `pull-identity.json` after
  a checkpoint that has mappings.
- Telemetry emits `CYPHERLITE-CLI_COMMAND` with `command=pull` and no
  query text, paths, or parameter values (ADR-0079).
- Default CI uses a loopback Query API fixture; live Aura is not a merge
  gate.
- Local property/label edits on a pulled entity are overwritten on the
  next pull of that remote id. Source `elementId` reuse after
  delete+recreate at Neo4j maps to a new local element only after the
  previous local element is gone.
- A client implementing embedded Relate may later call `cypherlite-load`
  for extract and upsert; that wrap is a separate change.

## Later references

ADR-0092 proposes replacing the mutable `pull-identity.json`
manifest-then-sidecar checkpoint rule with a manifest-selected immutable
virtual map written before manifest publication. WAL identity ops and
source-scoped upsert remain as decided here; v3 manifests without a selector
retain the legacy `pull-identity.json` fallback.

ADR-0122 adds credential hardening on top of the HTTP Query API transport
decided here: `pull` refuses to send Basic auth over a plaintext `http://`
origin to a non-loopback host unless `--allow-plaintext` is passed (matching
`cypherlite-bolt`, ADR-0109), and warns when a `--from` env file is group- or
world-readable. The loopback `neo4j://`/`bolt://` → `http://host:7474` mapping
is unchanged.

- ADR-0158 (streaming single-process pull) supersedes the manual `SKIP`/`LIMIT` paging workaround for the response cap and the persisted re-pull identity: one process per pull, the identity map is load-scoped memory, and a non-empty destination is refused.

- ADR-0200 moves the pull destination from a positional `DATABASE` to
  required `--database PATH`. `:memory:` stays rejected.

- ADR-0181 fixes the Query API wire format for pull: the pull path requests typed JSON (`application/vnd.neo4j.query…`) and decodes every `$type`-tagged value into an engine value, failing loudly on `Base64`, `Unsupported`, unknown tags, unparseable payloads, and untagged cells. `run_query`/`run_query_streaming` keep plain `application/json`, and request bodies stay `Content-Type: application/json`.
