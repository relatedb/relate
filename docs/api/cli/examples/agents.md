# Example: output for agents and scripts

Machine-readable output, failures you can branch on, and a row cap that keeps
a harness's context small. See the spec for the full contract:
[result envelope](../spec.md#result-envelope),
[query-run errors](../spec.md#query-run-errors), [row cap](../spec.md#row-cap).

## The result envelope

`--mode envelope` prints one object per result set. An empty result still lists
its columns, so "no rows" is distinguishable from "no output":

```bash
relate --mode envelope :memory: "RETURN 1 AS a, [1, 2] AS l"
relate --mode envelope :memory: "MATCH (n:Nope) RETURN n.p AS p"
```

```text
{"columns":["a","l"],"rows":[{"a":1,"l":[1,2]}],"truncated":false}
{"columns":["p"],"rows":[],"truncated":false}
```

## Failures you can branch on

In `json`, `jsonl` and `envelope` modes a failed statement also prints an error
object on stdout. The stderr line is unchanged, and the exit code carries the
class: `6` for a rejected statement, `5` for a conflict or a locked store, `1`
for other runtime failures.

```bash
relate --json :memory: "RETRN 1"
echo "exit=$?"
```

```text
syntax error: parse error: unexpected token "RETRN"
  RETRN 1
  ^
{"error":{"code":"validation_error","column":1,"exit_code":6,"kind":"syntax","message":"syntax error: parse error: unexpected token \"RETRN\"   RETRN 1   ^","retryable":false,"source":"RETRN 1"}}
exit=6
```

(The first three lines are stderr, the fourth is stdout.) A syntax error also
carries `column` and `source`, so a caller can point at the failing token without
parsing the message.

## The row cap

Under a detected agent harness (for example `CLAUDECODE=1`) with stdout not a
terminal, a statement prints at most 100 rows, warns on stderr and exits `0`.
`--max-rows` overrides it everywhere; `0` is unlimited.

```bash
CLAUDECODE=1 relate --jsonl "UNWIND range(1, 250) AS i RETURN i" | wc -l
CLAUDECODE=1 relate --jsonl --max-rows 0 "UNWIND range(1, 250) AS i RETURN i" | wc -l
relate --mode envelope --max-rows 2 "UNWIND range(1, 5) AS i RETURN i"
```

```text
warning: result truncated to 100 rows (the statement returned 250); pass --max-rows 0 for all rows
101
250
{"columnTypes":["INTEGER"],"columns":["i"],"notifications":[],"planCache":"miss","rowCap":{"kept":2,"total":5},"rows":[{"i":1},{"i":2}],"timings":{...},"truncated":true}
```

The first count is the 100 row lines plus the keyed `rowCap` record that
keeps the cut from reading as complete; the second is the 250 rows with no
cut and no record. The terminal timing line is opt-in: `--timings` appends
one keyed `{"timings": ..., "planCache": ...}` line per statement after any
plan and `rowCap` lines (the two counts above become 102 and 251); without
it, the default jsonl stream is pure rows.

## The version

```bash
relate --version
```

```text
Relate v1.0.0
License: Apache 2.0 (binary)
```

```bash
relate --version --json
```

```text
{"arch":"aarch64","license":"Apache 2.0 (binary)","name":"relate","os":"macos","version":"1.0.0"}
```

`arch` and `os` report the machine the binary runs on; `version` is the
release; `license` is the binary's license, stated by name only, never the
license text (ADR-0432).
