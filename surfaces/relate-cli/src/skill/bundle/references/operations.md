# Operations: storage forms, safety, schema, and APIs

## Storage forms

| Form | Open as | Notes |
|------|---------|-------|
| Directory | `relate --database ./mydata` | WAL + snapshot (Parquet by default in builds with that codec, else JSON; `--help` lists what is compiled in). `--snapshot-format` only on an **empty** directory (values as `--help` lists); never mix codecs in one database. Parquet checkpoints are wide-column parts (ADR-0553). |
| Single file | `relate --database ./mydata.rl` | `.rl` bundle: WAL + snapshot inside one host file (ADR-0074). Same semantics as the directory form — copy the one file to back it up. |
| Ephemeral | `relate` | Gone when the process exits. Default when no path is given. |

A `.db` file is **not** a Relate database; legacy `graph.json` data is
imported by copying it to `<dir>/graph.json` before opening.

## Backup and snapshot safety

- Durability model: `full` (default) barriers the WAL before acknowledgement;
  `everysecond` barriers in the background; `snapshot` relies on completed
  checkpoints. A checkpoint rewrites the snapshot (Parquet shards or
  `graph.json`).
- Directory backup: `relate clone ./db ./db-clone`. The source stays
  read-only, including beside a writer, and the clone is a new directory at
  the LSN visible on open. It does not follow later commits. Refused for
  `:memory:`.
- Directory ↔ bundle moves: `relate clone ./db ./backup.rl` exports a
  directory to a single-file bundle; `relate clone ./backup.rl
  ./restored` imports a bundle into a fresh directory (ADR-0282). Each
  side's format is inferred from the `.rl` suffix. The source opens
  read-only (a writer-held `.rl` fails locked), the destination must not
  exist, and the codec is preserved in every direction unless
  `--to-snapshot-format json|parquet` transcodes the destination checkpoint
  (ADR-0163) — e.g. `relate clone --to-snapshot-format parquet ./db
  ./backup.rl` for a compact bundle from a JSON working directory.
- `.rl` backup: run `.checkpoint` (or one-shot `relate -d ./db.rl ".checkpoint"`),
  wait for it to finish, then copy the file. A bundle cannot be read beside
  its writer. Never copy mid-write: the WAL plus snapshot are one unit.
- Restoring = copy the backup back and open it. Treat the WAL as part of the
  backup set — do not copy a snapshot without its WAL.

## Concurrency and write conflicts

- One writer process per database at a time. Relate is a single-process
  embedded engine: there is no multi-writer coordination between processes
  opening the same directory/`.rl` file.
- If a second process must access the same data, serve the Bolt surface
  (`relate serve --bolt PORT --database PATH`, ADR-0368) and connect
  clients to it — it serializes access.
- To serve a store while another process writes it, run
  `relate serve --read-only` (or `--read-only=live` to keep showing the
  writer's commits) on the same directory (ADR-0306): the serve takes no writer lease,
  so the writer keeps writing, and every write through the served surfaces
  fails.
- Inside one process, use transactions (`.begin` / `.commit` /
  `.rollback`) to make a group of writes atomic; `.rollback` discards them.

## Ontology as a query-steering source

Before generating Cypher from a natural-language request, load the ontology
first: `RELATE_PREVIEW=ontology relate query "CALL db.ontology.get()"
--database ./mydata`. The four `db.ontology.*` procedures are gated
(ADR-0378): without the `ontology` opt-in every call fails with
`semantic error: SemanticError: preview feature 'ontology' is not enabled:
set RELATE_PREVIEW=ontology`, and `SHOW PROCEDURES` does not list them.
With the opt-in, `CALL db.ontology.get()` returns the stored ontology
document as YAML (labels, relationship types, property names with types);
`get('json')` converts to JSON. Steer the generated Cypher's labels,
relationship types, and property names from it. The document is main's
graph-spec shape (ADR-0364): `version` is a string,
node/relationship/property ids are map keys, property existence is
`must_exist`, and constraints are id-keyed maps
with `type: EXISTS | KEY | UNIQUE | PROPERTY_TYPE`. A document exists only
after `CALL db.ontology.set($ontology)` stored one — `set` accepts only the
exact `schema` URI
`https://neo4j.com/docs/ontology-graph-spec/1.1.0/schema.json`, so a
pre-bump document fails and must be resubmitted in the new shape — and
`null` means none is set.
The procedures act on the session database and only `neo4j` holds the
ontology, so stay on the default session. If `get` returns `null`, the call
errors, or you cannot set the opt-in, fall back to the schema discovery
recipes below. The full surface (`set`, `get`, `drop`, `apply`) is in
`docs/reference/ontology-procedures.md`.

## Schema discovery recipes

```cypher
SHOW INDEXES;             -- name, type, entityType, labelsOrTypes, properties
SHOW CONSTRAINTS;         -- property/type constraints incl. owning index
SHOW FUNCTIONS;           -- every built-in the binder accepts
CALL db.schema.nodeTypeProperties();  -- inferred node property types + mandatory
CALL db.schema.relTypeProperties();   -- inferred rel property types + mandatory
CALL db.schema.visualization();       -- virtual schema graph (nodes per label, rels per triple)
-- labels and relationship types in the data:
MATCH (n) UNWIND labels(n) AS l RETURN DISTINCT l ORDER BY l;
MATCH ()-[r]->() RETURN DISTINCT type(r) AS relType ORDER BY relType;
-- property keys of a label:
MATCH (n:Person) UNWIND keys(n) AS k RETURN DISTINCT k ORDER BY k;
```

## Python and embedding APIs

The same engine embeds directly — no subprocess, no server:

```python
import relate

db = relate.connect("./mydata")          # or ":memory:"
db.execute("CREATE (p:Person {name: $name})", {"name": "Alice"})
for row in db.execute("MATCH (p:Person) RETURN p.name"):
    print(row["p.name"])
db.close()
```

Rust uses `relate::Database::open(...)`, and other languages wrap the C
ABI (`relate.h`). See the docs: Python library, Rust API, and C API
reference pages.
