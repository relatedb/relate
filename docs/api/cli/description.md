# CLI — description

`relate` is the product shell. With no statement and no `query` verb it
opens the interactive shell on a terminal (ADR-0231) — piped or redirected,
it prints help and exits instead, so scripts never block on stdin. The
explicit statement form is
`relate query [STATEMENT] [--database PATH]` (ADR-0200): a positional
statement or `-c`/`--command` (one statement or a `;`-separated list). With
no statement, `query` reads a script on stdin or opens the interactive shell
on a terminal. `--database` defaults to `:memory:`. The path-first form uses
the same flag and the same one-shot statement:
`relate [OPTIONS] [--database PATH] [QUERY]`.

It opens `relate::Database`: a directory with WAL + snapshot codecs,
a `.rl` single-file bundle (ADR-0074), or `:memory:`. An `http(s)`
`--database` value names a remote `.rl` bundle: the CLI fetches it to a
local copy, opened writable by default (`--read-only` as usual; only the
remote origin is never written back) — and `--safe` refuses the fetch. See the
[run-from-URL example](examples/run_from_url.md) (ADR-0496). Output-mode flags,
`--param`, `--fetch-params`, `--snapshot-format`, `--wal-dir`, `--logical-graph-size-limit`, `--load-csv-dir`,
`--checkpoint-interval-secs`, and `--durability-mode` apply. GraphStore flags
are withdrawn (ADR-0052).
Crate names are Relate (ADR-0053).

Durability defaults to `full`: an acknowledged write is covered by the WAL
barrier. `everysecond` acknowledges before a background barrier and accepts
loss of the latest approximately one second of writes (possibly longer if a
barrier is delayed). `snapshot` accepts loss of all writes since the latest
completed checkpoint. Its normal loss window follows
`--checkpoint-interval-secs` (300 seconds by default); `0` disables only the
timer, not threshold, explicit, or clean-close checkpoints.

`--param` binds JSON objects as ordinary maps even if they contain a
`__type` key; queries create typed values explicitly with `point($coords)`
or `date($iso)`. See the [parameter example](examples/params.md).

`--tx-meta K=V` attaches transaction metadata to the session's next
committed transaction — the "why" of a write, queryable beside the values
(ADR-0610, behind the `RELATE_PREVIEW=value-provenance` opt-in):
`provenance(element, key)` returns the committing transaction's
`{commitTs, lsn, metadata, value}`, and `CALL db.transactions()` lists the
retained records, so one statement can retract everything a given agent
wrote. See the [value-provenance example](examples/value_provenance.md).

`--fetch-params N=SRC` fetches an `http(s)` URL or reads a file, parses it
as JSON, and binds the document as the parameter `N` — a CLI-process fetch
(the same trust as piping through `--param`), so the engine's `LOAD CSV`
policy is untouched. The flag parses in every build (ADR-0270); the fetch
itself needs the `fetch-params` feature (`full` and `agent` bundles) and
fails closed without it. Sources are capped at 64 MiB with a 60 s timeout.
See the [fetch-params example](examples/fetch_params.md).

`--load SOURCE` loads a store from a source — a local `.rl` bundle, a
store directory, or an `http(s)` URL — into `--database`, the target
(LABS-872, LABS-903, ADR-0500). With the default `:memory:` target it
turns the source into the in-memory store: a read-only open that loads it
and then releases the source — nothing is held open or locked, the source
can be deleted or opened writable underneath, and a remote source's
download leaves nothing on disk at all. With a durable `-d PATH` target it
creates the store there (the path must not exist), so a URL can be
materialized into a store at a path you chose. `--database` itself never
takes a URL.

The shell denies local `LOAD CSV` unless given `--load-csv-dir PATH` or the
`RELATE_LOAD_CSV_DIR` fallback (the flag wins). This grants only bounded
reads of regular CSV files within that directory, including with an in-memory
database; invalid paths and oversized files get generic errors. See the
[CLI example](examples/load_csv.md).

Exit-after lifecycle verbs are positional commands (ADR-0069): `relate
skill install|remove|list|check [--agent NAME] [--force]` manages the
agent-skill bundle (`list` reports install state, `check` drift-detects
against the binary's bundle, ADR-0242),
`relate upgrade [--yes] [--check]` (alias `update`) self-updates the
binary from the release list published on the website (ADR-0250); the asset
download uses the direct release route and, while the repository is internal
(where that route answers 404), falls back to the authenticated GitHub API
asset route with `GITHUB_TOKEN` (ADR-0358),
`relate telemetry on|off|status` controls
anonymous usage metrics, and `relate pull [--from FILE] --query Q
--database PATH` copies a Neo4j/Aura subgraph into a local store (ADR-0080,
ADR-0200). `relate run <name> [flags]` dispatches an installed
command-kind extension (ADR-0429): the host parses the action's argv from
its manifest flag table, drives the child's wire batches into a store the
host alone owns, and fails an uninstalled name closed with the first-party
install hint — the installed-actions listing lives on `run --help`, never
the top-level page. `relate info --database PATH [--json]` prints cold store
metadata (ADR-0165). `relate serve --database PATH [--http PORT]
[--mcp PORT] [--read-only[=snapshot|live]]` hosts the network surfaces
in-process — the HTTP Query API v2 endpoint and the MCP streamable-HTTP
transport, sharing one basic-auth credential — until SIGINT/SIGTERM
(ADR-0232); `--read-only[=snapshot|live]` (ADR-0306, the ADR-0480
spelling) serves an existing store
beside the process writing it, a follower showing the writer's commits.
`relate agent-context
[COMMAND]` prints the compact agent briefing — commands, flags, exit codes —
rendered from the same table as `--help` (ADR-0242).

`relate --help` prints the query-run options plus a `Commands:` index of
the commands compiled into the binary; each command has its own help at
`relate <command> --help` (e.g. `relate clone --help`, `relate
skill install -h`), which prints that command's synopsis, actions, and flags
to stdout and exits 0. `relate update --help` shows the `upgrade` help; a
command compiled out of the build keeps its not-included error and exits 2. A
usage error prints the failing command's usage line plus a `--help` hint, and a `Did you mean` line when a flag is a close typo. See
the [help example](examples/help.md) and [help and usage
errors](spec.md#help-and-usage-errors). When a first positional is meant as a
command but misspelled (`relate qu`), the statement fails to parse and a
`did you mean "query"?` line with the command's synopsis follows the parse
error — a fallback after the failed parse only, so valid Cypher is never
rerouted; the same line also follows a usage error raised after the
misspelled word (`relate service --bolt 7687`), which exits before the
statement reaches the engine (ADR-0518); see [did-you-mean command
suggestions](spec.md#did-you-mean-command-suggestions).

For agents and scripts, the CLI prints machine-readable results and failures:
`--mode envelope` prints `{columns, columnTypes, rows, truncated}` per result set
(`columnTypes` are the engine-stamped per-column type names, ADR-0224); in the
`json`, `jsonl` and `envelope` modes a failed statement also prints an error
object on stdout, and the exit code classifies it (`6` rejected statement, `5`
conflict or locked store, `1` other runtime failure, `2` usage). A batch write
that applied nothing is visible on stderr — `note: write statement changed
nothing; the graph is unchanged`, plus a `.commit` summary for a transaction
that persisted nothing (ADR-0402); only a plan-producing `PROFILE` run reports
through its plan table instead. A no-op script therefore cannot pass unnoticed
for a successful one; the exit code and stdout are unchanged. Under a
detected agent harness a statement prints at most 100 rows (`--max-rows`,
`RELATE_MAX_ROWS`), and `--version` states the binary's license
(ADR-0432): the `License: Apache 2.0 (binary)` line under the version line,
the `license` member in the `--version --json` object. See the
[agent example](examples/agents.md), [result envelope](spec.md#result-envelope),
[query-run errors](spec.md#query-run-errors) and [row cap](spec.md#row-cap).

`skill install` stamps an ownership marker (`.relate-installed`) into
`<skills dir>/relate`. Without `--force`, `skill install` replaces a
destination that is missing, marker-owned, or a recognizable relate bundle
(`SKILL.md` frontmatter declaring `name: relate` with a `version:` line),
and refuses anything else; `skill remove` is stricter and removes only a
marker-owned directory. Either command exits `1` and leaves an unrecognized
directory untouched unless `--force` is passed, so user-authored skill
directories are never overwritten or deleted by accident.

When stdout is a terminal the default output is a table; when stdout is piped
or redirected it is JSONL, unless an output flag (`--json`, `--csv`,
`--jsonl`, `--markdown`, or `--mode`) is given (ADR-0107) — or agent mode is in
effect (a detected harness, or `--agent-mode`/`RELATE_AGENT_MODE=1`,
ADR-0407), which defaults the piped run to the compact `toon` form (ADR-0426). Piped and scripted
consumers therefore get machine-readable output by default — `relate db -c
'RETURN 1 AS n' | jq` works with no flag — while interactive runs stay human
readable.

Human-facing output is hardened by default: `table`/`markdown` escape terminal
control characters in cells and headers, `csv` neutralizes spreadsheet
formulas (and therefore prefixes negative numbers with `'`), and `--mode
cypher` emits single-line, re-parseable literals. JSON/JSONL stay byte-exact.
See the [output-safety example](examples/output_safety.md).

On a TTY the interactive prompt uses rustyline for line editing and
in-session history (ADR-0073). Piped stdin and one-shot `QUERY` /
`--command` do not.

For installation, see the install section of the [README](../../../README.md).
