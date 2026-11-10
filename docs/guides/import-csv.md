# Import and Export Data

## Import CSV with LOAD CSV

The `LOAD CSV` clause reads a CSV file and makes each row available as a variable in your Cypher query.

### With headers

If your CSV has a header row:

```csv
name,age,city
Alice,30,NYC
Bob,28,LA
Charlie,35,Chicago
```

```cypher
LOAD CSV WITH HEADERS FROM '/path/to/people.csv' AS row
CREATE (:Person {name: row.name, age: row.age, city: row.city});
```

Each `row` is a map where keys are the column headers.

### Without headers

If your CSV has no header row:

```csv
Alice,30,NYC
Bob,28,LA
```

```cypher
LOAD CSV FROM '/path/to/people.csv' AS row
CREATE (:Person {name: row[0], age: row[1], city: row[2]});
```

Each `row` is a list of string values accessed by index.

### Custom field separator

Use `FIELDTERMINATOR` for files that use a separator other than commas:

```cypher
LOAD CSV WITH HEADERS FROM '/path/to/data.tsv' AS row FIELDTERMINATOR '\t'
CREATE (:Record {id: row.id, value: row.value});
```

The terminator must be exactly one character — a multi-character (or empty)
`FIELDTERMINATOR` string fails with a type error instead of being truncated
to its first character.

## Import CSV with dot-commands (withdrawn)

The `.import` dot-command (csv / json / graphml / cypher) was withdrawn with
the interactive shell surface reduction (ADR-0058) — entering it errors
`Error: unknown command .import`. Use `LOAD CSV` (above) for CSV files, or
pipe a Cypher script into the shell:

```bash
./relate ./mydata < setup.cypher
```

## Bulk import: durability and batch size

Every write that autocommits pays one WAL durability barrier under the
default `full` mode — the WAL is fsync'd before the statement is
acknowledged (ADR-0077). Group commit shares that barrier between
**concurrent** commits only (ADR-0019); a single sequential connection
autocommitting statement-by-statement pays one barrier per statement. On
a volume with real device barriers (5–15 ms per fsync, typical for cloud
disks) that floor is 60–200 statements/s no matter how fast the engine
is — the engine itself writes a node in ~0.06 ms.

Measured on one volume, 30k single-node creates, one autocommit per
statement:

| Configuration | Nodes/s |
| --- | --- |
| `full`, fsync-lenient volume (~0.2 ms per barrier) | ~3,400 |
| `full`, volume with ~12 ms barriers | ~80 |
| `everysecond` (engine floor, disk-independent) | ~18,000 |

Two levers fix this: fewer, larger commits — or a relaxed durability
mode while loading.

### Keep `full`: batch the writes

One statement = one barrier, so put many rows in one statement:

- A `LOAD CSV` over a whole file is **one statement** — one barrier for
  the entire import, however many rows the file has. This is the
  simplest fast path for CSV data.
- For statement scripts and driver clients, batch rows into one
  parameterized statement every few thousand rows:

  ```cypher
  UNWIND $rows AS row
  CREATE (:Person {name: row.name, age: toInteger(row.age)})
  ```

  or wrap a batch in an explicit transaction — over Bolt, `BEGIN` …
  `COMMIT` commits once per `COMMIT` (ADR-0028), so a driver's
  transaction-per-batch has the same one-barrier economics.

A few thousand rows per batch is the sweet spot: the barrier cost
amortizes over the batch, while the commit's workspace — every row's
redo, held until the commit lands — stays small. A single
million-row statement pays the same one barrier but holds the whole
million rows' redo in memory first.

### Relax durability while loading

```bash
./relate --durability-mode everysecond ./mydata < import.cypher
# or: RELATE_DURABILITY_MODE=everysecond ./relate ./mydata < import.cypher
```

- `everysecond` acknowledges after apply and barriers about once per
  second — a crash can lose ~1 s of acknowledged writes. Write rate is
  bounded by the engine floor, not the disk (~5× on fsync-lenient
  volumes, ~200× on real-barrier volumes).
- `snapshot` acknowledges after apply; only completed checkpoints are
  durable. For a fresh store loaded from data you can re-import, this
  is the fastest choice.

The mode is an open-time setting (flag `--durability-mode`, environment
`RELATE_DURABILITY_MODE`), not a per-statement one. After the load, take
a checkpoint (`.checkpoint` in the interactive shell) and run or serve
with the default `full` again.

## Export query results

Use the output-format flags with shell redirection. CSV:

```bash
./relate -csv ./mydata "MATCH (p:Person) RETURN p.name, p.age" > people.csv
```

```
p.name,p.age
Alice,30
Bob,28
```

JSON (`--json`, or one object per line with `--mode jsonl`):

```bash
./relate --json ./mydata "MATCH (p:Person) RETURN p.name, p.age" > people.json
```

The `.export` dot-command (csv / json / graphml) and the `.mode` / `.output`
REPL commands were withdrawn with the shell surface reduction (ADR-0058).
To export a whole database directory, copy the directory after a
`.checkpoint` — see [Backup and Restore](./backup-restore.md).

## Tips

- All values from CSV files are strings. Use `toInteger()` or `toFloat()` if you need numeric types:

  ```cypher
  LOAD CSV WITH HEADERS FROM 'data.csv' AS row
  CREATE (:Person {name: row.name, age: toInteger(row.age)});
  ```

- For large imports, few large statements beat many small autocommits — see [Bulk import](#bulk-import-durability-and-batch-size).

- File paths in `LOAD CSV` are relative to where you launched Relate, or can be absolute paths.

- Use **CSV** for tabular node data or interoperability with spreadsheets, and the `--json` / `--mode jsonl` output flags for programmatic interchange.
