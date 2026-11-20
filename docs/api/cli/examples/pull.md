# Example: pull a Neo4j subgraph

```bash
relate pull --from neo4j.env --query "MATCH (n) RETURN n" --database ./mirror
```

`neo4j.env` is a `KEY=VALUE` file (`NEO4J_URI`, `NEO4J_USERNAME`,
`NEO4J_PASSWORD`, optional `NEO4J_DATABASE`) — the same format Aura's
Created credentials file and `neo4j-cli --env-out-file` emit. `--query`
must `RETURN` nodes, relationships, or paths. On success the process prints
`pulled N new / M reused nodes, P new / Q reused rels to ./mirror` and
exits 0. The target is a Relate directory (WAL + snapshot), not
`:memory:`.

`NEO4J_URI` is an http(s) Query API origin (e.g. `https://host:7473`); a
Bolt `neo4j://host:7687` tries `http://host:7474` then `http://host`, a
TLS Bolt `neo4j+s://host:7687` tries `https://host:7473` then
`https://host` (ADR-0633 — each candidate until one answers), an Aura
`neo4j+s://` URI resolves to `https://<host>/`. The hosted demo's Query
API is `https://demo.neo4jlabs.com:7473` (database `companies`), reached
from its TLS Bolt connection URI `neo4j+s://demo.neo4jlabs.com:7687`.

Repeat the same query to reuse identities (no duplicate nodes):

```bash
relate pull --from neo4j.env --query "MATCH (n) RETURN n" --database ./mirror
# pulled 0 new / N reused nodes, 0 new / 0 reused rels to ./mirror
```

A different overlapping query adds only remote ids that are not already
mapped for this source:

```bash
relate pull --from neo4j.env --query "MATCH (a:Account {id: 'acc-745'})-[t:TRANSFER]-(b) RETURN a, t, b" --database ./mirror
```
