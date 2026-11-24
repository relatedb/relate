# Example: an agent-written knowledge graph, with provenance

Three simulated agents write facts into one store, each invocation
attaching its own `--tx-meta` transaction metadata — the "why" of a write
(ADR-0610). The metadata rides the commit's WAL record, and the facts can
be read back with their provenance beside the values through
`provenance()`, or retracted per agent through `CALL db.transactions()`.

The whole machinery is the `value-provenance` preview feature's, read once
at open — without the opt-in every `--tx-meta` run fails with the authored
preview error (exit 6) instead of silently dropping the map:

```bash
export RELATE_PREVIEW=value-provenance
```

Every agent is one CLI invocation against one store (`graph.db`, created
by the first write); the flag's merged map rides that invocation's next
committed transaction. The ingest agent bulk-loads a directory:

```bash
relate query --database graph.db --mode jsonl \
  --tx-meta agent=ingest --tx-meta source=directory \
  'CREATE (:Person {name: "Ada", role: "engineer"}),
           (:Person {name: "Grace", role: "mathematician"}),
           (:Company {name: "Acme"})'
```

A write with no `RETURN` prints nothing; exit 0 is the acknowledgement.
The curator agent links what ingest loaded, then checks its own work —
the value it just wrote beside the provenance of that value
(`commitTs` is the committing transaction's sequence, `lsn` its log
position, `metadata` the merged `--tx-meta` map):

```bash
relate query --database graph.db --mode jsonl --tx-meta agent=curator <<'cypher'
MATCH (p:Person {name: "Ada"}), (c:Company {name: "Acme"})
CREATE (p)-[:WORKS_FOR {since: 2026}]->(c);
MATCH (p:Person {name: "Ada"})-[r:WORKS_FOR]->(c)
RETURN p.name AS person, c.name AS employer, r.since AS since,
       provenance(r, "since") AS sinceOrigin;
cypher
```

```text
{"employer":"Acme","person":"Ada","since":2026,"sinceOrigin":{"commitTs":2,"lsn":2,"metadata":{"agent":"curator"},"value":2026}}
```

The scraper agent bulk-adds tags, then reads the whole graph back — every
fact with the transaction that wrote it beside the value:

```bash
relate query --database graph.db --mode jsonl \
  --tx-meta agent=scraper --tx-meta url=wiki <<'cypher'
CREATE (:Tag {name: "pioneer"}), (:Tag {name: "analyst"});
MATCH (n)
RETURN labels(n) AS labels, n.name AS name, provenance(n, "name") AS nameOrigin
ORDER BY name;
cypher
```

```text
{"labels":["Company"],"name":"Acme","nameOrigin":{"commitTs":1,"lsn":1,"metadata":{"agent":"ingest","source":"directory"},"value":"Acme"}}
{"labels":["Person"],"name":"Ada","nameOrigin":{"commitTs":1,"lsn":1,"metadata":{"agent":"ingest","source":"directory"},"value":"Ada"}}
{"labels":["Person"],"name":"Grace","nameOrigin":{"commitTs":1,"lsn":1,"metadata":{"agent":"ingest","source":"directory"},"value":"Grace"}}
{"labels":["Tag"],"name":"analyst","nameOrigin":{"commitTs":3,"lsn":3,"metadata":{"agent":"scraper","url":"wiki"},"value":"analyst"}}
{"labels":["Tag"],"name":"pioneer","nameOrigin":{"commitTs":3,"lsn":3,"metadata":{"agent":"scraper","url":"wiki"},"value":"pioneer"}}
```

One statement retracts everything the scraper wrote. `CALL
db.transactions()` lists the retained records ascending by `lsn`; the
`WHERE` keeps one agent's commits; their `changedKeys` name the elements
that agent touched, by the same element-id spelling `elementId(n)`
returns; the `DETACH DELETE` removes exactly those elements. The
supervisor's invocation carries its own `--tx-meta`, so the retraction is
itself part of the audit surface:

```bash
relate query --database graph.db --mode jsonl --tx-meta agent=supervisor <<'cypher'
CALL db.transactions() YIELD commitTs, metadata, changedKeys
WHERE metadata.agent = "scraper"
UNWIND changedKeys AS ck
WITH collect(DISTINCT ck.elementId) AS touched
MATCH (n) WHERE elementId(n) IN touched
DETACH DELETE n
RETURN count(n) AS retracted;
MATCH (n:Tag) RETURN count(n) AS tagsLeft;
MATCH (n) RETURN labels(n)[0] AS label, count(n) AS standing ORDER BY label;
MATCH ()-[r:WORKS_FOR]->() RETURN count(r) AS links;
CALL db.transactions() YIELD commitTs, metadata, changedKeys
WHERE metadata.agent = "supervisor"
RETURN commitTs, metadata, changedKeys;
cypher
```

```text
{"retracted":2}
{"tagsLeft":0}
{"label":"Company","standing":1}
{"label":"Person","standing":2}
{"links":1}
{"changedKeys":[{"elementId":"4:rl-id:n3","key":"name"},{"elementId":"4:rl-id:n4","key":"name"}],"commitTs":4,"metadata":{"agent":"supervisor"}}
```

The tags are gone, the curated graph stands, and the supervisor's own
record names the two elements it deleted — the retract answers "who
removed this and why", the same question `provenance()` answers for
writes.

Retained history is bounded by the last provenance-enabled open: a store
opened without the opt-in collapses it at that open's next checkpoint
(ADR-0610), and `CALL tx.setMetaData(map)` inside a session seeds the same
map the flag seeds. The [CLI integration
test](../../../../surfaces/relate-cli/tests/e2e_value_provenance.rs)
executes this session against the binary with these outcomes asserted,
in CI.
