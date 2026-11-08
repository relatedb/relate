# Agent safety: read-only use, limits, backups, destructive operations

## Read-only use

Open with `--read-only` when the work must not mutate the database. That
loads the snapshot and WAL once, rejects writes and `.checkpoint`, and does
not rewrite the files. On a native directory it can sit beside the process
that is writing. A `.rl` file can be opened `--read-only` only while no
writer has it open. `:memory:` with `--read-only` rejects writes and creates
nothing. `--read-only=live` (the former `--follow`) is native directories
only (refused for `.rl` and `:memory:`): it keeps applying new WAL frames
and reloads if a checkpoint leaves a gap. The value is attached-form only
(`--read-only live` is a usage error), and the flag is once-only.

`relate serve --read-only[=snapshot|live]` (ADR-0306, the ADR-0480
spelling) is the same open served over the network: the server takes no
writer lease, another process can write the store while it is served, and
a live serve keeps showing the writer's commits.

A query without writing clauses is read-only in effect on a writable open —
but a typo (`DELETE` vs `DETACH DELETE`) cannot be undone outside a
transaction. `--read-only` is the fence for that.

## Resource limits

- `LIMIT` every exploratory return; use `SKIP`/`LIMIT` windows to page.
- Preview Cypher features are opt-in only with
  `RELATE_PREVIEW=<feature>[,...]` (ADR-0371): unset means the pinned
  dialect floor, no preview syntax; an unknown feature name fails closed
  naming the known features instead of being silently ignored. Live gates:
  `hybrid-search` (CIP-278 `COMBINE`), `local-callables` (CIP-231
  `DEFINE PROCEDURE`/`DEFINE FUNCTION`), the CIP graph-type trio
  (`graph-type-opt-props`, `graph-type-default-values`,
  `graph-type-implied-labels`), `obfuscated-literals` (the `******` dummy literal, lexer-gated —
  ADR-0454), `mixed-label-syntax` (colon-conjunction, GQL label symbols and
  `IS` may mix within a clause's label expressions — ADR-0456),
  `float16-coordinate-types` (FLOAT16/BFLOAT16 vector coordinates —
  ADR-0464), `chronological-comparisons` (the four chronological operators —
  `IS [NOT] BEFORE`, `AFTER`, `SAME INSTANT`, `SAME REPRESENTATION` —
  ADR-0467),
  `complex-search-patterns` (wider SEARCH MATCH patterns — ADR-0467), and
  `ontology` — the four `db.ontology.*` procedures (ADR-0378); while it is
  disabled they fail with
  `preview feature 'ontology' is not enabled` and `SHOW PROCEDURES` omits
  them.
- The engine caps the live graph and indexes with
  `RELATE_LOGICAL_GRAPH_SIZE_LIMIT` / `--logical-graph-size-limit`
  (logical bytes; 0/unset = unlimited) — set it for untrusted
  queries. RSS is not that cap. A positive
  `RELATE_CONCURRENT_LOGICAL_WORKSPACE_LIMIT` /
  `--concurrent-logical-workspace-limit` turns on the shared
  logical-workspace pool for executing queries (ADR-0452); omit it and
  overlapping queries are not refused.
- Output is capped at 100 rows per statement when an agent harness is
  detected (stderr says so; every machine mode also carries the cut
  in-band — a terminal `{"rowCap":{...}}` record in `--jsonl` and a
  `"rowCap"` object in `--mode envelope` — so a capped result never reads
  as complete. Every machine mode also closes each statement's stream with
  its `"timings"` object (the phase split and the fine sub-split, in
  milliseconds) and the `"planCache"` outcome (`miss` / `hit-raw-fastpath`
  / `hit-template` / `replan-generation-mismatch` / `bypassed`) — read
  them to see where a slow statement spends its time and whether the plan
  cache served it
  as complete). Raise it
  with `--max-rows N`, lift it with `--max-rows 0`, or set
  `RELATE_MAX_ROWS`. The cap follows agent mode (ADR-0407's ladder):
  force it on with `--agent-mode`, off with `--no-agent-mode` or
  `RELATE_AGENT_MODE=0`. The cap bounds what is printed, not the engine's work.
- Statement scope: prefer several small queries over one giant `UNWIND`
  batch when a task is exploratory.

## Backups

- Directory backup: `relate clone ./mydata ./my-clone`. This opens the
  directory read-only, writes a checkpoint only into the new directory, and
  exits. It can run beside a writer. The clone is the LSN visible at open
  and does not follow later commits. Refused for `:memory:`.
- Move a directory to a single file and back: `relate clone ./mydata
  ./backup.rl`, later `relate clone ./backup.rl ./restored`
  (ADR-0282). Read-only source, destination must not exist, codec
  preserved.
- `.rl` backup: `.checkpoint`, wait for completion, then copy the file — or
  `relate clone ./db.rl ./backup.rl` while no writer holds the bundle
  (a writer-held `.rl` source fails locked). The WAL and snapshot are one
  unit — never copy a snapshot without its WAL.
- Snapshot before large destructive operations so a mistake is a file copy
  away from undo.

## Destructive operations

```text
MATCH (n:Legacy) RETURN count(n);          -- count first
.begin
MATCH (n:Legacy) DETACH DELETE n;
MATCH (n) RETURN count(n);                 -- verify inside the transaction
.commit                                    -- or .rollback
```

- `DROP INDEX` / `DROP CONSTRAINT` are immediate and silent on success;
  list first (`SHOW INDEXES`, `SHOW CONSTRAINTS`).
- `REMOVE n.label` / property removals are equally silent — verify by query.
- A batch write that applied nothing says so on stderr —
  `note: write statement changed nothing; the graph is unchanged`, and
  `note: .commit changed nothing …` for a transaction that persisted nothing
  (ADR-0402). The exit code stays 0, so grep stderr rather than trusting the
  code: a zero-row `MATCH` short-circuits its whole statement, and the
  `CREATE` you expected never ran. Grep the anchored prefix (`note: write
  statement`, `note: .commit`), not `changed nothing` alone — both lines carry
  that tail.
- The note is the engine's effect record: a catalog change (`ALTER DATABASE …`,
  `CREATE GRAPH TYPE`, `CALL db.ontology.set/drop`) applies without moving a
  mutation counter and prints nothing, and neither does a read the planner
  classifies as a write (`CALL { … }` with a read-only body, `LOAD CSV …
  RETURN`). A write inside a `CALL { … }` body or an untaken `WHEN` branch is
  an attempt even when the body never runs, so it reports; a change the engine
  rolled back (a failed batch under `IN TRANSACTIONS … ON ERROR CONTINUE`) is
  not applied, so it does not silence the note. `PROFILE` executes its
  statement, so its effect counts in the `.commit` summary — an applied write
  silences it, an attempted no-op makes it report — while the plan table
  replaces that statement's own note. In a batch, silence means the statement
  read only, applied a change, or printed a plan — never that a catalog write
  was skipped. An idempotent `MERGE` that matched and wrote nothing reports; an
  idempotent `SET` re-applies its property write and does not. A statement that
  fails takes no note, including one that executed and then failed to render:
  its change is already applied, so it still counts in the `.commit` summary —
  a persisted write is never called a no-op.

## Write conflicts and retries

- Relate allows one writer process per database. A second writer on the
  same file is unsupported, not retryable — serialize writers or move
  multi-client access to the Bolt server.
- Inside a process, transaction conflicts (e.g. reading a deleted element)
  surface as classified errors; retry only when the error class explicitly
  identifies a conflict. Never blind-retry other errors — inspect them.
- `DETACH DELETE` of a node another part of your transaction modified is a
  conflict, not a bug: re-run the statement in a fresh transaction.

## Network exposure

- The Bolt surface (`relate serve --bolt PORT`, ADR-0368 — the
  standalone `relate-bolt` binary is retired) ignores Bolt
  authentication in v1. Bind it to
  `127.0.0.1` (the default) and never expose it beyond loopback without an
  authenticating proxy in front.
- Bolt can be TLS-encrypted with `--tls-cert`/`--tls-key` and mTLS with
  `--tls-client-ca` — library-level only since ADR-0368 (embed
  `relate_bolt::serve`, or front `relate serve` with a
  TLS-terminating proxy). Encryption does not
  authenticate clients, so it does not lift the loopback rule.
- Plaintext on a non-loopback bind is refused unless the library-level
  `--allow-plaintext` is passed. With TLS unconfigured the server refuses
  to start; with TLS
  configured it refuses the plaintext path per connection.
