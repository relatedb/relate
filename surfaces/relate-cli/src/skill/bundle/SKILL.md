---
name: relate
description: Relate is an embedded openCypher graph database with a single-binary CLI. Use this skill when the user wants to run Cypher queries, manage a local graph database, or embed it from Python; its focused references cover vector search, geospatial, operations, and agent safety.
version: {{VERSION}}
---

# Relate

Relate is an embedded, single-process openCypher graph database (openCypher
TCK coverage is measured by the project's spec harness, reporting-only) plus
a large slice of Neo4j's Cypher 25 / GQL-aligned extensions. One binary, no
server, no configuration. Databases are a local directory, a single `.rl`
file, or `:memory:`.

## Selection guidance

Everything an agent normally needs — installation, CLI, Cypher, Python — is
in **this** file. Reach for a focused reference only when the task calls for
it:

| Need | Read |
|------|------|
| Vector search: indexes, `SEARCH`, similarity functions | [vector](references/vector.md) |
| Geospatial: points, distance, bounding boxes, point indexes | [spatial](references/spatial.md) |
| Operations: storage forms, concurrency, ontology + schema discovery, import | [operations](references/operations.md) |
| Safe operation: read-only use, limits, backups, destructive queries | [agent-safety](references/agent-safety.md) |
| Pull from Neo4j / Aura into a local database | [pull](references/pull.md) |
| Push a local database (or a scoped subgraph) into Neo4j / Aura | [push](references/push.md) |
| Git history: coupling, ownership, churn, reviewers from a checkout | [run-git-to-graph](references/run-git-to-graph.md) |
| Command-kind extensions: author, install, or run `relate run <name>` packages | [client-extensions](references/client-extensions.md) |
| Node.js client: `@relate/relate` install, native-lib resolution | [client-javascript](references/client-javascript.md) |
| Java client: `com.relate:relate` dependency, bundled natives, classifier jars | [client-java](references/client-java.md) |
| Go client: cgo link, typed values, numeric + element ids, typed errors | [client-go](references/client-go.md) |
| Browser/WASM client: `@relate/relate-wasm` init, OPFS, traps | [client-wasm](references/client-wasm.md) |
| iOS/macOS Swift client: SwiftPM `Relate`, XCFramework, concurrency traps | [client-swift](references/client-swift.md) |
| Python client: `relate-db` errors, bookmarks, `Duration`/`Vector`, element ids | [client-python](references/client-python.md) |
| C ABI: `librelate_ffi` v2 build/link, walker, params builder, error channel | [client-c](references/client-c.md) |

## Installation

```bash
curl -sSfL https://relatedb.ai/install.sh | bash
relate --version        # always verify the version before relying on behavior
```

From source: `git clone https://github.com/relatedb/relate && cargo build --release`.
The Python package (`relate-db`) is pending its first tagged PyPI
publish; until then use `make install-python` from a source checkout.

## CLI

```
relate query [STATEMENT] [--database PATH]
relate [OPTIONS] [--database PATH] [QUERY]
```

Prefer the explicit form:

```
relate query "RETURN 2" --database ./mydata
```

Before writing Cypher from a natural-language request, read the ontology
first (opt in with `RELATE_PREVIEW=ontology`, ADR-0378) and take labels,
relationship types, and property names from it:

```
RELATE_PREVIEW=ontology relate query "CALL db.ontology.get()" --database ./mydata
```

`null` means no ontology is set. Without the opt-in the call fails with
`preview feature 'ontology' is not enabled` and `SHOW PROCEDURES` omits the
four `db.ontology.*` rows; if you cannot set it, fall back to schema
discovery (Agent rule 4).

- `-d` / `--database` sets the store (directory, `.rl` bundle, or `:memory:`;
  default `:memory:`). The same flag is the store on `query`, the path-first
  form, `pull`, and `info`. `-d` is the short form.
- `STATEMENT` or positional `QUERY` runs one-shot (one statement or a
  `;`-separated list). With no statement and no `query` verb the process
  opens the interactive REPL on a terminal; piped or redirected, it prints
  help (ADR-0231). `query` with no `STATEMENT` is the interactive REPL on a
  terminal, or a script on stdin
  (`relate query --database ./db < script.cypher`).
- Flags accept both `-json` and `--json` forms. Key flags:
  `--json` / `--csv` / `--markdown` / `--toon` / `--mode` (`--format` is an
  alias; `--mode jsonl` for one JSON
  object per line; `--mode envelope` for one
  `{columns, columnTypes, rows, truncated}` object per result set, which keeps
  the columns of an empty result and the engine-stamped per-column type names
  in `columnTypes`, ADR-0224;
  `--mode toon` for the compact TOON tabular form),
  `--param N=V` (repeatable),
  `--snapshot-format json` (empty dir only; `parquet` — whose checkpoints
  are ADR-0553's wide-column parts — too in builds with that codec, as
  `--help` lists),
  `--logical-graph-size-limit SIZE`, `--concurrent-logical-workspace-limit SIZE`
  (a positive value turns query admission on; omit leaves it off, ADR-0452),
  `--max-rows N` (rows printed per statement; under an
  agent harness the default is 100, with the cut named in-band in every
  machine mode — a terminal `{"rowCap":{"kept":N,"total":M}}` line in
  `--jsonl`, `"rowCap"` in `--mode envelope` — plus a stderr warning;
  `--max-rows 0` for all), `-c/--command`, `--version`. Piped or redirected
  stdout defaults to `toon` when agent mode is in effect (ADR-0407's ladder
  detects your harness; ADR-0426) and JSONL otherwise (ADR-0107);
  `--mode jsonl` (one object per line), `--mode table`, `--json`, `--csv`, or
  `--markdown` override it. `--json` prints one
  compact line when stdout is piped; `--pretty` indents it.
- Read query notifications before rewriting a query: `EXPLAIN <statement>`
  plans without executing and prints a `Notifications:` block after the
  plan table — one line per hint, each with the diagnosis and a `Remedy:`
  naming the action (give the patterns a join anchor, bound an expansion,
  create the named index, fix an unknown label/type/property spelling,
  replace a deprecated function). `--mode envelope` / `--json` carry the
  same list as machine fields (`code`, `gql_status`, `severity`,
  `category`, `title`, `description`, `remedy`, plus `operator_id` on
  operator-scoped entries). The plan table's `Warnings` column and footer
  already cover the three plan-warning kinds, so do not expect them in the
  block.
- Namespaces: every store has three fixed namespaces — `neo4j` (user data,
  the default), `system` (SHOW/administration commands) and
  `system_ontologies` (listed but not queryable: every non-administration
  statement routed there fails with 42N17 `This database is not queryable.
  Use the db.ontology.* procedures on the user database 'neo4j' instead.`
  — read the stored ontology through `CALL db.ontology.get()` on `neo4j`,
  with `RELATE_PREVIEW=ontology` set).
  `--namespace NAME` (case-insensitive) sets the session namespace for the
  whole run; `USE <name>` inside a statement switches it for that
  statement. Example:
  `relate query --database ./db --namespace system -c 'SHOW DATABASES'`.
- REPL dot-commands: `.help .quit .exit .begin .commit .rollback
  .checkpoint .info .timer on|off`. `.info` prints store metadata (persistent
  databases), exact live node/rel counts, and the index/constraint catalog —
  read-only, safe to run anytime. REPL editing is readline-style with
  in-session
  history; there is no tab completion or history file. The CLI prints no
  mutation summaries — verify writes with a query.
- Commands (first argument only): `relate query …`, `relate skill install
  [--agent NAME]` / `relate skill remove` / `relate skill list` /
  `relate skill check` (install state and drift detection), `relate
  upgrade [--yes] [--check]` (alias `update`). `relate agent-context
  [COMMAND]` prints the compact command/flag/exit-code briefing.
- `--timeout DURATION` (e.g. `10s`, `500ms`, `2m`) bounds each statement; a
  timed-out statement exits 7 (`code: "timeout"`). `--pretty` forces indented
  JSON when piped.
- Exit codes: 0 success, 1 runtime failure, 2 usage error, 5 conflict (write
  conflict or store locked), 6 rejected statement (syntax/semantic/type
  error), 7 query timeout (`--timeout`).
  With `--json`/`--jsonl` a failure also prints
  `{"error":{"code","exit_code","kind","message","retryable"}}` on stdout (a
  syntax error adds `column` and `source`); branch on `code`. When several
  statements stop on a failure the error adds `statement`, `statements` and
  `committed` (`0` inside an open `.begin`), so you know whether earlier
  statements already changed the store.
  `relate <command> --help` prints one command's help.
  In pipe mode the first failing statement stops the batch.

## Cypher

Reading:

```cypher
MATCH (n:Person {name: 'Alice'}) RETURN n;
MATCH (a)-[:KNOWS*1..3]->(b) RETURN a, b;              -- variable length
MATCH p = (a)-[:KNOWS]->(b) RETURN nodes(p), length(p); -- named path
OPTIONAL MATCH (p)-[:LIVES_IN]->(c)                    -- null on no match
```

Writing:

```cypher
CREATE (a:Person {name: 'Alice', age: 30})
MERGE (p:Person {name: $name})                          -- find-or-create
SET p.age = 31, p += {city: 'Berlin'}                   -- update / merge-props
REMOVE p.city                                           -- drop a property
MATCH (n:Legacy) DETACH DELETE n                        -- delete + rels
```

Composing: `WITH` (pipe, `GROUP BY`, `ORDER BY`, `SKIP`/`LIMIT`, `WHERE`),
`UNWIND`, `UNION [ALL]`, `FOREACH`, `LOAD CSV`, `CALL { ... }` subqueries,
`CALL (imports) { ... }` scope clause, `EXISTS`/`COUNT`/`COLLECT` subquery
expressions, `LET`, `FILTER`, `FINISH`, `NEXT` statement chains.

Schema:

```cypher
CREATE INDEX idx FOR (n:Person) ON (n.name);
CREATE VECTOR INDEX vidx FOR (n:Doc) ON (n.embedding) WITH [n.topic];
CREATE FULLTEXT INDEX ft FOR (n:Doc) ON EACH [n.title, n.body];
CREATE CONSTRAINT c FOR (p:Person) REQUIRE p.id IS UNIQUE;
SHOW INDEXES; SHOW CONSTRAINTS; SHOW FUNCTIONS;
CALL db.schema.nodeTypeProperties(); CALL db.schema.relTypeProperties();
CALL db.schema.visualization();
CALL db.ontology.get();             -- needs RELATE_PREVIEW=ontology
DROP INDEX idx;
```

Extensions beyond openCypher: label expressions (`n:A|B&!C`), quantified
path patterns and selectors (`SHORTEST 1`, `ANY`, `TRAIL`), `SEARCH`
(vector + fulltext indexes), `vector()`/UUID types, string interpolation
(`s'Hello {name}'`), map comprehensions (`{i IN list | k: v}`), explicit
`GROUP BY`, `CYPHER 5` / `CYPHER 25` preambles. Parameters are `$name`
(passed with `--param` or from Python dict keys). Every referenced
parameter must be supplied: a missing one fails the statement with
`ParameterMissing: Expected parameter(s): <names>`, never binds `null`.

## Python

```python
import relate

db = relate.connect("./mydata")      # or ":memory:" / "./task.rl"
db.execute("CREATE (p:Person {name: $name})", {"name": "Alice"})
for row in db.execute("MATCH (p:Person) RETURN p.name"):
    print(row["p.name"])

db.execute("BEGIN")
try:
    db.execute("MATCH (n:Legacy) DELETE n")
    db.execute("COMMIT")
except Exception:
    db.execute("ROLLBACK")
db.close()
```

Parameters (ADR-0219): `uuid.UUID` crosses as its canonical string;
`bytes`/`bytearray` as a list of byte values 0–255; tuples as lists; dicts
are always maps — a `{"srid": …, "x": …, "y": …}` dict is NOT turned into a
point, use `relate.Point(srid, x, y)` or `point($coords)`; returned maps
keep key order. Non-dict params or non-string keys raise
`relate.ProgrammingError`.

Rows are driver-style `Record` dicts: `row["p.name"]`, `row.p_name`-style
attribute access for valid identifiers, plus `.data()`, `.keys()`, and
`.get()`. The DB-API cursor also offers Neo4j-driver conveniences —
`cursor.single()` (exactly one row or `ResultNotSingleError`), `peek()`,
`fetch(n)`, `to_df()` for a pandas DataFrame (optional
`relate-db[pandas]` extra), and `to_arrow()` for a `pyarrow.Table`
(optional `relate-db[arrow]` extra — `.to_pandas()` and
`pl.from_arrow()` both work). `cursor.consume()` returns a
`ResultSummary` (counters, resolved namespace, query type,
`bookmark`, engine-reported `elapsed_ms`). Errors carry the engine's
`neo4j_code`, `gql_status`, `stage` and `retryable`; durations arrive as
`relate.Duration`, vectors as `relate.Vector`, and `element_id` is
Cypher's `elementId()` — see [client-python](references/client-python.md). `cursor.description` type codes come from
the engine-reported column types (ADR-0224): `STRING`/`BINARY` map to
themselves, `INTEGER`/`FLOAT`/`BOOLEAN` fold into `NUMBER`,
`DATETIME`/`DATE`/`TIME` fold into `DATETIME`, and graph, spatial, list,
map and duration columns — plus `ANY` (empty or `Null` first row) — report
`None`. `cursor.graph()` returns the result's
graph entities (nodes, relationships, paths — nested values walked),
de-duplicated by id, as a `ResultGraph`.

Native `Database` API adds `execute_arrow()` for zero-copy Arrow IPC into
Pandas/Polars. One writer process per database; `":memory:"` is ephemeral.
For interactive graphs in Jupyter, the optional `relate.viz` helper
builds a `neo4j-viz` `VisualizationGraph` from a query result's graph
elements (nodes, relationships, paths) — `render_widget()` in Jupyter
(`pip install neo4j-viz`; see
docs/api/python/spec.md#visualization-relateviz-optional).

## Agent rules (always follow)

1. Run `relate --version` first and trust it over remembered behavior.
2. Use `:memory:` only for disposable work — it vanishes on exit.
3. Prefer a **new `.rl` database per experimental task**
   (`relate ./task-<slug>.rl`); copy an existing one instead of
   experimenting in place.
4. Load the ontology before generating Cypher from a natural-language
   request — `CALL db.ontology.get()` returns the stored ontology document
   as YAML (labels, relationship types, property names with types); `null`
   means none is set. It requires `RELATE_PREVIEW=ontology`; without it
   the call fails with `preview feature 'ontology' is not enabled`. Steer
   labels, relationship types, and property names from the document. If it
   returns `null`, errors, or you cannot set the opt-in, fall back to schema
   discovery — `SHOW INDEXES`, `SHOW CONSTRAINTS`,
   `CALL db.schema.nodeTypeProperties()`, `MATCH (n) UNWIND labels(n) AS l
   RETURN DISTINCT l` (see [operations](references/operations.md)).
5. Use `$parameters` for all values, never string-interpolated literals.
6. For machine parsing, remember piped or redirected stdout defaults to
   `toon` when agent mode is in effect (ADR-0407's ladder — a harness like
   yours is likely detected; ADR-0426): one `rows[N]{cols}:` header plus one
   line per row. `--mode jsonl` gives one JSON object per line for
   line-based parsing, `--json` (`-json`) a JSON array, and `--mode table`
   the human-readable table.
7. Snapshot (`.checkpoint`) before large destructive operations, and wrap
   them in `.begin` … `.commit`/`.rollback` so they are undoable.
8. Limit returned rows (`LIMIT`, or a `SKIP`/`LIMIT` window) — never dump a
   graph unbounded.
9. Retry only classified write conflicts; anything else fails loudly (see
   [agent-safety](references/agent-safety.md)).
10. Never expose unauthenticated Bolt beyond loopback — bind the Bolt
    surface (`relate serve --bolt PORT`, ADR-0368) to `127.0.0.1`
    unless an authenticating proxy is in front of it.

## Tips & gotchas

- **Vector similarity scores are rebounded** — `vector.similarity.cosine`
  returns a value in `[0, 1]` (1.0 = identical direction, 0.5 = orthogonal);
  not the raw cosine.
- **One writer at a time** — single-process engine; use the Bolt server for
  multi-client access, or `relate serve --read-only[=snapshot|live]` to serve
  a store beside the process writing it (ADR-0306).
- **Destructive DDL is silent** — `DROP INDEX`/`DROP CONSTRAINT` print
  nothing on success; list first.
