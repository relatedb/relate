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

## Large writes in one go: the two memory knobs

A bulk load can hit two different memory bounds, and they are set by two
independent knobs — size both when you want one big commit:

- **How large each commit may be** — the per-commit WAL record cap
  (ADR-0650). Default **256 MiB**. One commit whose serialized record
  crosses it is refused with the message that names the remedy:

  ```sh
  RELATE_WAL_PAYLOAD_LIMIT=512mb relate --database ./mydata
  ```

  The alternative to raising it is the statement-level form the refusal
  names: `LOAD CSV … CALL (r) { … } IN TRANSACTIONS OF n ROWS`, which
  commits every n rows as its own record — usually the better choice for
  very large loads.
- **How many large commits may run at once** — the concurrent
  logical-workspace pool (ADR-0452). Off by default; a big single commit
  needs headroom in it once enabled:

  ```sh
  RELATE_CONCURRENT_LOGICAL_WORKSPACE_LIMIT=2g relate --database ./mydata
  ```

Both read the same `SIZE` grammar (bytes, or `mb`/`mib`/`gb`/`gib`); a
zero or unparseable value fails the open naming the variable, never a
silent default. Embedded callers set the same knobs as
`OpenOptions::wal_payload_limit` and
`OpenOptions::concurrent_logical_workspace_limit`.

## Opening a large store: the load thread count

Opening a checkpointed store decodes the snapshot and rebuilds the
adjacency structures on a shared pool of load threads, and the temporary
memory of that load scales with the thread count. `RELATE_LOAD_THREADS`
caps the threads (default: the machine's available parallelism):

```sh
RELATE_LOAD_THREADS=2 relate --database ./mydata
```

A zero, non-integer, or non-UTF-8 value fails the open naming the variable,
before the store is leased; a still-set pre-rename spelling of the variable
fails the open naming `RELATE_LOAD_THREADS` (ADR-0517, ADR-0664). Embedded callers set
`OpenOptions::load_threads` and read what the open actually used from
`Database::open_peak_transient_bytes()` — the peak temporary bytes of the
latest load (`0` for an empty `:memory:` store; a follower's gap reload
refreshes it). It is a measurement, not a limit.

## Memo budget (advanced)

The executor memoises repeated pure checks within one statement under a shared
budget, default **16 MiB** (ADR-0657). Lower it on a tight host, or set `0` for a
no-memo control run; results never change, only speed and memory:

```sh
RELATE_EXPRESSION_MEMO_BUDGET=4 relate --database ./mydata
```

A plain whole-MiB integer; a negative or unparsable value fails the open naming
the variable. Embedded callers set `OpenOptions::expression_memo_budget`. No CLI
flag.

## Ontology as a query-steering source

Before generating Cypher from a natural-language request, load the ontology
first: `RELATE_PREVIEW=ontology relate query "CALL db.ontology.get()"
--database ./mydata`. The four `db.ontology.*` procedures are gated
(ADR-0378): without the `ontology` opt-in every call fails with
`semantic error: SemanticError: preview feature 'ontology' is not enabled:
set RELATE_PREVIEW=ontology`, and `SHOW PROCEDURES` does not list them.
With the opt-in, `CALL db.ontology.get()` returns the stored ontology
document as YAML (labels, relationship types, property names with types);
`get('json')` returns it as JSON. Steer the generated Cypher's labels,
relationship types, and property names from it. The procedures' contracts
are revised (ADR-0667); the accepted document
is the published Ontology Graph Spec 1.1.0 shape, and `null` from `get`
means none is set — a document exists only after `CALL
db.ontology.set($ontology)` stored one.
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
