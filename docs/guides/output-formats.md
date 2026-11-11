# Output Formats

Relate renders results in six formats, selected with CLI flags (there is no
in-session `.mode` dot-command — see [Dot Commands](../reference/dot-commands.md)).

| Format | Flag | Default |
|--------|------|---------|
| Table | *(none)* | when stdout is a terminal (ADR-0107) |
| CSV | `-csv` | |
| JSON | `-json` | |
| JSON Lines | `--jsonl` | when stdout is piped or redirected (ADR-0107) |
| Result envelope | `--mode envelope` | |
| Markdown | `-markdown` | |
| Cypher | `--mode cypher` | |

The default depends on stdout: Table on a terminal, JSONL when stdout is piped
or redirected (ADR-0107) — or TOON for the same piped run when agent mode is in
effect (ADR-0407's ladder: a detected harness, or the explicit flag/env; ADR-0426).
An explicit output flag always wins.

Plus `-noheader` (any format) and the general `--mode table\|csv\|json\|jsonl\|envelope\|markdown\|cypher`.

Flags work in one-shot query mode, pipe mode, and the interactive shell.

## Table

```bash
relate -d ./mydata "MATCH (p:Person) RETURN p.name, p.age"
```

```
+--------+-------+
| p.name | p.age |
+--------+-------+
| Alice  | 30    |
| Bob    | 28    |
+--------+-------+
```

## CSV

```bash
relate -csv ./mydata "MATCH (p:Person) RETURN p.name, p.age"
```

```
p.name,p.age
Alice,30
Bob,28
```

## JSON

```bash
relate -json ./mydata "MATCH (p:Person) RETURN p.name, p.age"
```

```json
[
  {"p.name": "Alice", "p.age": 30},
  {"p.name": "Bob", "p.age": 28}
]
```

## JSON Lines (JSONL)

One JSON object per line, suitable for streaming. Use `--jsonl` (or
`--mode jsonl`):

```bash
relate --jsonl ./mydata "MATCH (p:Person) RETURN p.name, p.age"
```

```
{"p.name":"Alice","p.age":30}
{"p.name":"Bob","p.age":28}
```

## Result envelope

One compact object per result set, with the column list and a truncation flag.
Use it when a program (or an agent) reads the output: an empty result still
lists its columns, so "no rows" is distinguishable from "no output".

```bash
relate --mode envelope ./mydata "MATCH (p:Person) RETURN p.name, p.age"
```

```
{"columns":["p.name","p.age"],"rows":[{"p.age":30,"p.name":"Alice"},{"p.age":28,"p.name":"Bob"}],"truncated":false}
```

`columns` is the query's column order. Keys inside each row object are sorted
alphabetically, as they are in `--json`, so read `columns` when order matters.
`truncated` is `true` when the [row cap](../api/cli/spec.md#row-cap) cut the
rows. `--json` and `--jsonl` are unchanged.

## Errors in the JSON modes

In `json`, `jsonl` and `envelope` modes a failed statement also prints one error
object on stdout, after any rows already written, and the exit code says what
kind of failure it was (`6` rejected statement, `5` conflict or locked store, `1`
other runtime failure). The message on stderr is unchanged. See
[query-run errors](../api/cli/spec.md#query-run-errors).

```
{"error":{"code":"validation_error","column":1,"exit_code":6,"kind":"syntax","message":"syntax error: …","retryable":false,"source":"RETRN 1"}}
```

A syntax error also carries `column` (the character the error points at) and
`source` (that line of the statement), and stderr shows the line with a caret
under the token.

## Row cap for agents

Under a detected agent harness (Claude Code, Codex, Cursor, …) with stdout not a
terminal, a statement prints at most 100 rows and warns on stderr; pass
`--max-rows N` (`0` for all) or set `RELATE_MAX_ROWS`. Terminals and scripts
outside a harness are not capped. See the [row cap](../api/cli/spec.md#row-cap).

## Markdown

```bash
relate -markdown ./mydata "MATCH (p:Person) RETURN p.name, p.age"
```

```
| p.name | p.age |
|--------|-------|
| Alice  | 30    |
| Bob    | 28    |
```

## Cypher

Renders result **values** as Cypher literals. Like JSONL, this needs `--mode`:

```bash
relate --mode cypher ./mydata "MATCH (p:Person) RETURN p.name AS name, p.age AS age"
```

```
CREATE ({name: 'Alice', age: 30})
CREATE ({name: 'Bob', age: 28})
```

Note this is a value formatter, not a dump: a returned **node** column has no
Cypher literal form and is omitted (with a `// column … omitted` comment). For
backup, copy the storage directory or `.rl` file — see
[Backup and Restore](backup-restore.md).

## Suppress headers

```bash
relate -csv -noheader ./mydata "MATCH (p:Person) RETURN p.name"
```

```
Alice
Bob
```

## Redirect output to a file

There is no `.output` dot-command. Use the shell's own redirection:

```bash
relate -csv ./mydata "MATCH (p:Person) RETURN p.name, p.age" > people.csv
echo "MATCH (n:Person) RETURN n.name" | relate -json ./mydata | jq .
```
