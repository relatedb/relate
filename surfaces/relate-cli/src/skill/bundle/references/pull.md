# Pull from Neo4j / Aura

```bash
relate pull [--from FILE] --query Q [--param N=V] --database PATH
```

- `--from FILE` — `KEY=VALUE` env file (`NEO4J_URI`, `NEO4J_USERNAME`,
  `NEO4J_PASSWORD`, optional `NEO4J_DATABASE`). Same format as Aura Created
  credentials and `neo4j-cli --env-out-file`. Omit `--from` to use those
  process environment variables (`NEO4J_USER` aliases `NEO4J_USERNAME`).
- `--query Q` — required shape query (file path or inline). Must `RETURN`
  nodes, relationships, or paths — not scalars or aggregates.
- `--param N=V` — source query parameter (repeatable).
- `--database PATH` — destination directory or `.rl` bundle. `:memory:` is rejected.

`NEO4J_URI` is an http(s) Query API origin (e.g. `https://host:7473`); a
Bolt `neo4j://host:7687` tries `http://host:7474` then `http://host`, a
TLS Bolt `neo4j+s://host:7687` tries `https://host:7473` then
`https://host` (ADR-0633 — each candidate until one answers), an Aura
`neo4j+s://` URI resolves to `https://<host>/`. The hosted demo's Query
API is `https://demo.neo4jlabs.com:7473` (database `companies`), reached
from its TLS Bolt connection URI `neo4j+s://demo.neo4jlabs.com:7687`.

The command talks HTTP Query API v2, upserts the subgraph (source-scoped
union on remote `elementId`; overlapping pulls reuse existing local
elements), then carries the source schema (ADR-0201): `SHOW CONSTRAINTS` /
`SHOW INDEXES` rows are replayed as standalone `CREATE … IF NOT EXISTS`
statements (uniqueness / existence / key / property-type constraints and
RANGE / TEXT / POINT indexes; constraint-backing, offline, LOOKUP, FULLTEXT
and VECTOR rows are filtered or skipped with a stderr note — schema replay
never fails a pull). It checkpoints, prints
`pulled N new / M reused nodes, P new / Q reused rels (+ C constraints, I indexes; S schema skipped) to PATH`, and exits.

Pulled values keep their Cypher type (ADR-0181): `pull` asks the source for
the typed Query API v2 JSON media type, so `POINT`, `DATE`, `TIME`,
`DATETIME`, `ZONED DATETIME`, `DURATION`, `UUID`, `VECTOR`, and every other
source type are stored as those types. A property the engine cannot
represent — a byte array (`Base64`), an `Unsupported` value, an unparseable
payload, or an untagged cell — fails the pull with exit 1 and a
`typed wire decode failed: …` error naming the element id and property key.
The full source-type → stored-type table is in `docs/api/cli/spec.md`.
