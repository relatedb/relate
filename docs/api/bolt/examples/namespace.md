# Example: select a namespace over Bolt

Every store has the fixed namespaces `neo4j`, `system` and
`system_ontologies` (ADR-0116). Official Neo4j drivers select one the way
they select a Neo4j database — `session(database=…)` or
`execute_query(database_=…)` — which Bolt 4.x sends as `db` in the `BEGIN`
or auto-commit `RUN` extra (ADR-0117). Absent, `null` or `""` means `neo4j`,
and the selection never carries over to the next request. The final
`PULL`/`DISCARD` SUCCESS reports the namespace the query ran in as `db`; an
unknown name fails with `Graph not found: <name>` (ADR-0146).
`system_ontologies` is visible only under the `RELATE_PREVIEW=ontology` opt-in (ADR-0593; without it the namespace is invisible — unlisted, and every route to the name fails as an unknown database, 42N00) and not
queryable (ADR-0229): only the
ADR-0116 §2 database-administration commands (`SHOW DATABASES`, `ALTER
DATABASE … SET DEFAULT LANGUAGE`) still run while routed to it — every
other statement, reads included, fails with the 42N17 text `This database
is not queryable. Use the db.ontology.* procedures on the user database
'neo4j' instead.`

In one terminal, run from the repository root:

```bash
cargo run -p relate-cli -- serve --host 127.0.0.1 --bolt 7687 --database :memory:
```

In another terminal with the official Neo4j Python driver installed
(`python3 -m pip install neo4j`), run the [driver example](namespace.py)
from the repository root:

```bash
python3 docs/api/bolt/examples/namespace.py
```

The example runs `SHOW DATABASES` on `system_ontologies` with `database_=`
(the one statement kind that still runs there), shows a data query routed
there fails with the 42N17 text, shows the selection does not carry over,
and surfaces the unknown-database failure — its CE text and Neo4j's
`Neo.ClientError.Database.DatabaseNotFound` code. CI runs this same script
in the `relate-bolt` job.
