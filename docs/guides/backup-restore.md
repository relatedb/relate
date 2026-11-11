# Backup and Restore

A Relate backup is a **copy of the storage artifact** — the database
directory, or the single-file `.rl` bundle. There is no separate dump/restore
command: the shell exposes only `.help`, `.quit`, `.begin`, `.commit`,
`.rollback`, and `.checkpoint` (see
[Dot Commands](../reference/dot-commands.md)).

## Back up

### Database directory

The store is a directory: the WAL plus one snapshot codec. **Copy the whole
directory, including `wal/`.** A checkpoint (timed, on last-handle close, or
`.checkpoint`) is what the snapshot files hold; commits since the last
checkpoint live only in the WAL.

```bash
relate -d ./mydata "CREATE (:Person {name: 'Alice'})"
cp -a ./mydata ./mydata-backup
```

`graph.json` (JSON codec) or the Parquet shards are the last checkpoint, not
live state. Restoring only `graph.json` without `wal/` loses commits made
since that checkpoint.

### Single-file bundle (`.rl`)

A `.rl` path keeps the WAL, snapshot objects, and catalog in one host file, so
the backup is that file alone:

```bash
relate -d ./mydata.rl "CREATE (:Person {name: 'Alice'})"
cp ./mydata.rl ./mydata-backup.rl
```

### In-memory

An in-memory database has no on-disk state; there is nothing to copy. To move
it to durable storage, re-run the load against a directory or `.rl` path.

### Quiescing before a copy

Copy is only consistent if no writer is mid-commit. Either stop the process
first, or force a checkpoint and copy the WAL alongside the snapshot (as
above) — the WAL plus the last checkpoint is a complete, replayable state.

## Restore

### From a directory or bundle copy

```bash
cp -a ./mydata-backup ./restored
relate -d ./restored

cp ./mydata-backup.rl ./restored.rl
relate -d ./restored.rl
```

### Legacy GraphStore JSON

The old single-file GraphStore `.db` format is withdrawn. To bring one onto
the JSON codec, copy it to `graph.json` in an empty directory and open with
`--snapshot-format json`:

```bash
mkdir -p ./imported
cp old-graph.db ./imported/graph.json
relate -d ./imported --snapshot-format json
```

## Exporting rows as Cypher text

`--mode cypher` renders result **values** as Cypher literals (useful for
copying data out of a query), but it is not a dump: a returned node column has
no Cypher literal form and is omitted with a comment. Export the properties
you want explicitly:

```bash
relate -d ./mydata --mode cypher \
  "MATCH (p:Person) RETURN p.name AS name, p.age AS age"
# CREATE ({name: 'Alice', age: 30})
```

For a full-graph text export, project the fields yourself (labels via
`labels(p)`, relationships via `type(r)`) and redirect stdout to a file.

## Tips

- **Before risky operations**: copy the directory or `.rl` file before bulk
  deletes or schema changes.
- **Checkpoint first** so the snapshot — not just the WAL — carries your data.
- **Version your backups**: include timestamps (`mydata-2024-01-15/`).
- **Test restores**: reopen a copy and run a `count(*)` to confirm it loads.
