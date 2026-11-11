# Dot-Commands Reference

The interactive shell implements exactly **eight** dot-commands. Dot-commands
are entered at the `cypher>` prompt and are **line-based**: they submit on
Enter and do not require a trailing `;` (a `;` is tolerated). Any other
command starting with `.` fails with `Error: unknown command <name>` (exit 1
in non-interactive mode).

On a TTY the prompt is a rustyline editor (ADR-0073): arrow keys, Home/End,
and emacs bindings edit the current line, and up/down walk in-session
history. There is no tab completion or persistent history file. Piped
stdin stays a plain reader. Cypher statements submit on a terminating
top-level `;`; until then the continuation prompt `   ...> ` keeps reading.
A blank line does not submit a pending statement. Ctrl-C clears the
pending buffer and returns to `cypher>`.

## Quick reference

| Command | Description |
|---------|-------------|
| `.help` | Show the command summary |
| `.quit` / `.exit` | Exit Relate |
| `.begin` | Begin an explicit transaction |
| `.commit` | Commit the open transaction (persists durably) |
| `.rollback` | Roll back the open transaction (discards changes) |
| `.checkpoint` | Force an immediate checkpoint (snapshot + WAL recycle) |
| `.info` | Store metadata, live counts, and the current catalog |
| `.timer on\|off` | Toggle per-statement wall-clock timing (default off) |

All eight commands work identically in the interactive shell and in piped
stdin. In piped stdin the batch is fail-fast: the first failing statement
stops the run and the process exits with that statement's code: `6` for a
rejected statement (syntax, semantic or type error), `5` for a conflict, `1`
otherwise ([exit codes](../api/cli/spec.md#query-run-errors)).

## Shell commands

### `.help`

Prints the command summary:

```
cypher> .help
  Cypher statement ending with ;
  .begin / .commit / .rollback / .checkpoint
  .info
  .timer on|off
  .quit
```

### `.quit` / `.exit`

Exits the process with status 0 (bare `exit` / `EXIT` work too).
If an explicit transaction is still open (`.begin` with no matching
`.commit`), its uncommitted writes are discarded — they were never
published to the graph or the WAL. That discard is never silent
(ADR-0164): the CLI prints

```
Warning: exiting with an open transaction; rolling back uncommitted changes. Use .commit before exiting to persist them.
```

to stderr before the final checkpoint. Interactively (`.quit` / EOF) the
exit status stays **0** — implicit rollback on connection drop is the
documented behavior, not an error. In a one-shot or piped-stdin run an open
transaction at exit forces exit status **1** instead, so scripts can detect
a forgotten `.commit` from the exit code alone (a higher code already
earned by a failed statement is kept). Run `.commit` before exiting to
persist the writes. On a persistent database, `.quit` then runs a final
checkpoint; if that fails it prints `Error: final checkpoint failed:
<reason>` and exits 1.

## Transactions

By default Relate runs in **autocommit** mode: each statement that
mutates the graph commits durably on success (it is flushed to disk before
the result returns). The `.begin` / `.commit` / `.rollback` commands group
several statements into one durable unit instead. See
[Storage Engine](../guides/storage-engine.md) for the durability model
(WAL + snapshot codec + checkpoint).

These commands succeed silently — no confirmation is printed. Errors are
reported on stderr.

### `.begin`

Begins an explicit transaction. Subsequent statements accumulate in the
transaction and are **not** written to disk until you `.commit`. A second
`.begin` while a transaction is already open is an error
(`semantic error: already in a transaction`).

```
cypher> .begin
cypher> CREATE (:Person {name: 'Alice'});
cypher> CREATE (:Person {name: 'Bob'});
```

### `.commit`

Commits the open transaction, durably persisting all of its statements as a
single unit. With no open transaction it errors (`semantic error: no
transaction`). A statement that fails **inside** a transaction rolls back
only its own partial writes, but it also **poisons** the transaction: every
later statement and `.commit` then error `semantic error: transaction
failed; rollback required` until you `.rollback`.

### `.rollback`

Rolls back the open transaction, discarding every statement executed since
`.begin`. With no open transaction it errors (`semantic error: no
transaction`).

```
cypher> .begin
cypher> CREATE (:Scratch {tmp: true});
cypher> .rollback
cypher> MATCH (n:Scratch) RETURN count(n) AS c;
+---+
| c |
+---+
| 0 |
+---+
```

## Storage commands

### `.checkpoint`

Forces an immediate checkpoint: the snapshot codec rewrites its snapshot
(parquet shards or `graph.json`) up to the current state and the WAL
recycles the segments covered by it. Succeeds silently. On a `:memory:`
database it succeeds without writing anything (there is no durable store).

```
cypher> .checkpoint
cypher>
```

Periodic automatic checkpoints are configured with
[`--checkpoint-interval-secs`](./cli-flags.md); see
[Storage Engine](../guides/storage-engine.md) for what a checkpoint
writes.

### `.info`

Prints a read-only diagnostic readout for the open database (ADR-0165). It
takes no arguments and never modifies the store.

For a persistent database the first block is store metadata read cold from
disk — the same block, with the same wording, that
[`relate info`](../api/cli/spec.md) prints without a session: the layout
(native directory or `.rl` bundle, with the bundle superblock version), the
snapshot codec and manifest format version, the checkpoint LSN and commit
timestamp, the total size on disk in bytes, and the number of WAL segments
past the checkpoint (a staleness hint for the manifest figures). A store
that has never been checkpointed reports `snapshot: none yet` instead. On a
`:memory:` database there is no on-disk store, so the store metadata is
marked not applicable.

The remaining lines always come from the live session: exact node and
relationship counts via Cypher (`MATCH (n) RETURN count(n)` and the
relationship equivalent — the graph is already materialized, so the counts
are exact, including un-checkpointed writes), then the current catalog via
`SHOW INDEXES` and `SHOW CONSTRAINTS` rendered in the session's output mode
(so they include DDL replayed from the WAL that the on-disk manifest does
not reflect yet).

```
cypher> .info
store:                        /data/shop
layout:                       native directory
codec:                        parquet
format version:               3
checkpoint LSN:               1
commit timestamp:             1759651200
total bytes:                  4163
wal segments past checkpoint: 1
nodes:                        2
relationships:                1
indexes:
(SHOW INDEXES result, in the session's output mode)
constraints:
(SHOW CONSTRAINTS result)
```

On `:memory:`:

```
cypher> .info
store:                        :memory: (store metadata not applicable)
nodes:                        0
relationships:                0
indexes:
constraints:
```

With a JSON-codec store (`--snapshot-format json`) the manifest is a codec
envelope only, so the store block reports `format version: unavailable` plus
a `note:` line explaining why (ADR-0165 Decision 5); the live counts and
catalog are unaffected.

## Display commands

### `.timer on|off`

Toggles per-statement wall-clock timing. **Default is off** (SQLite-style).
When on, every successfully executed Cypher statement prints its wall-clock
run time after the result, as `Time: N.NNNms`; the time covers execution
plus result formatting. Entering `.timer` with anything but `on` or `off`
prints `Usage: .timer on|off` and leaves the state unchanged.

```
cypher> .timer on
Timer: on
cypher> RETURN 1 AS x;
+---+
| x |
+---+
| 1 |
+---+
Time: 0.042ms
cypher> .timer off
Timer: off
```

## Withdrawn commands (ADR-0058)

The following legacy dot-commands were **withdrawn** with the interactive
shell surface reduction (ADR-0058). Entering any of them fails with
`Error: unknown command <name>`:

- Output/formatting: `.mode`, `.headers`, `.width`, `.output`,
  `.once`, `.bail` — use the CLI's `--mode` / `--json` / `--csv` /
  `--markdown` / `--noheader` flags and shell redirection instead.
- Introspection: `.labels`, `.reltypes`, `.schema`, `.indexes`,
  `.constraints`, `.count`, `.databases`, `.version` — use Cypher
  (`UNWIND labels(n) AS l RETURN DISTINCT l`, `type(r)`, `count(...)`)
  or `relate --version`.
- Data management: `.dump`, `.read`, `.backup`, `.restore`, `.import`,
  `.export` — use piped stdin (`relate -d ./db < script.cypher`),
  `LOAD CSV`, and output flags with redirection. For durable backup, copy
  the database directory after a `.checkpoint` (see
  [Backup and Restore](../guides/backup-restore.md)).
- Multi-database: `.open`, `.close`, `.attach`, `.detach`, `.use` — the
  database is chosen once at startup (`relate --database PATH`); one process
  operates on one graph (see [Limitations](../limitations.md)).
- Query analysis: `.explain`, `.profile` — not currently exposed in the
  shell.
- Versioned storage: `.compact`, `.versions`, `.checkout` — Lance
  versioning was withdrawn with GraphStore (ADR-0052).
