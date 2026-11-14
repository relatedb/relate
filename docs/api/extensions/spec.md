# Extensions — spec

One extension is one package directory that mounts as a CLI command and
speaks MCP stdio to a host-owned store. This document is the contract for
package authors and host-side callers: the manifest schema, the discovery
walk, the mount's wire and caps, the host execution lifecycle, the error
classes, and the verbs.

Decisions: [ADR-0429](../../adr/0429-cli-extension-registry.md) (the
registry and the `run` mount), [ADR-0431](../../adr/0431-cli-extension-distribution.md)
(distribution — the verbs and the channel, shipped with this CLI; see
[Verbs and lifecycle](#verbs-and-lifecycle)), [ADR-0449](../../adr/0449-github-install-trust-pins.md)
(the `--github` commit-pin tier and its per-user trust store),
[ADR-0442](../../adr/0442-extension-mcp-stdio-wire.md)
(the MCP stdio mount and its five tools), [ADR-0447](../../adr/0447-extension-single-wire.md)
(the mount speaks MCP stdio only; it supersedes ADR-0430's dual wire). The
verb's user-facing CLI contract (synopsis, help pages, did-you-mean, exit
codes) is [the `run` command](../cli/spec.md#run-command) in the CLI spec.

Ships in the `relate` binary through the `extensions` Cargo feature
(`full` — the default — and `agent`; not `minimal`). The
`run` word is reserved in every build: without the feature the command
keeps the shared not-included error (exit 2), and a store named `run`
stays reachable as `relate --database run`.

## The mount

```
relate run <name> [flags]
```

`<name>` is an installed action's mount name — the manifest's `name`
field. Installed names never enter the CLI's command-word vocabulary, so
the top-level grammar and `--help` stay byte-stable across installs
(ADR-0429 §1). The dynamic installed-actions listing lives on
`run --help` and the bare-`run` usage error, never the top-level page.

Only an installed extension mounts under `run`: with the `lab` surface
retired (ADR-0431 §12), every name the registry does not resolve fails
closed with the pinned discovery error (below).

## The manifest (`extension.json`)

One package directory is `extension.json` plus the executable. The
manifest is JSON, decoded strict and fail-closed: an unknown field, an
unknown vocabulary value, a mistyped default, or malformed JSON fails the
load loudly, naming the offending value — no entry is skipped, nothing
flows by default.

`kind` is the extension stratum, a closed vocabulary from day one. The
only stratum is `command`; the later strata (`callables`, `starter`,
`engine-plugin`) arrive as new values, and a manifest naming one today is
a refusal naming the value and the vocabulary — never a future-proofed
install (ADR-0429 §3).

Command-stratum fields:

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `kind` | `"command"` | required | The stratum; the only v1 value |
| `name` | string | required | The mount name — the `<name>` of `run <name>`. Collision-checked at load |
| `version` | string | required | The package's own version (shown in listings and the child's `clientInfo`) |
| `protocol` | string | required | The MCP revision at the `command` mount point. Must equal the revision this host serves (`2025-06-18`); another value is a usage error naming both (exit 2, before anything runs) |
| `flags` | array | required (may be empty) | The flag table the host parses argv from — one entry per flag, below |
| `positional` | object \| absent | absent | The optional positional, `{"name": "DIR"}`; at most one positional is accepted |
| `requires-fresh-store` | boolean | `false` | Run the ADR-0394 fresh-store check: refuse a target that already holds nodes, before the child spawns |
| `commit` | `"whole-import"` \| `"per-batch"` | required | The commit policy that frames the session's writing calls (below) |
| `hidden` | boolean | `false` | Still parses and runs, appears in no listing, its flags never reach did-you-mean |
| `executable` | string | required | The executable, a path relative to the package directory. An absolute, rooted, or parent-escaping path is a load refusal |

The flag table — one entry per flag, tagged by `kind`. The default rides
the tag's own field, so a default that does not parse as its flag's kind
fails the decode:

| Flag kind | Entry | Value grammar |
| --- | --- | --- |
| `string` | `{"kind": "string", "name": "…", "default": "…"}` | `--name VALUE` |
| `integer` | `{"kind": "integer", "name": "…", "default": 0}` | `--name N` — a value that does not parse as an integer is a usage error naming the flag and the value |
| `boolean` | `{"kind": "boolean", "name": "…", "default": false}` | `--name` — bare; sets the value `true` |
| `repeatable` | `{"kind": "repeatable", "name": "…", "default": []}` | `--name VALUE` — repeatable, one value per occurrence, appended to the default-initialized list |

Two grammar refusals the host's parse adds over the decode (both exit 2):
a flag table that declares a flag twice (the first would silently shadow
the second), and a flag named `database` or `db`, which collides with
the host's own store flag — the host resolves `-d`/`--database` itself
(ADR-0383), so no manifest flag may spell it.

A well-formed manifest exercising every field:

```json
{
  "kind": "command",
  "name": "greet",
  "version": "1.2.3",
  "protocol": "2025-06-18",
  "flags": [
    {"kind": "string", "name": "message", "default": "hello"},
    {"kind": "integer", "name": "count", "default": 3},
    {"kind": "boolean", "name": "shout", "default": false},
    {"kind": "repeatable", "name": "tag", "default": []}
  ],
  "positional": {"name": "DIR"},
  "requires-fresh-store": true,
  "commit": "whole-import",
  "executable": "greet.sh",
  "hidden": false
}
```

## Discovery

The registry walks `$XDG_CONFIG_HOME/relate/extensions/` (falling
back to `~/.config/relate/extensions/` when `$XDG_CONFIG_HOME` is
unset, empty, or relative — a relative value is treated as unset, so the
walk is never resolved against the working directory) and nothing else —
never the working directory, never a project-local directory. That is the
drive-by-execution guard: the grammar a `relate` invocation accepts is
a property of the user's machine, never of the directory the command
happens to run in, so cloning a checkout can never add executables to the
grammar (ADR-0429 §6).

A *package* is a directory directly under the extensions dir. The walk is
sorted, so the enumeration and every refusal are deterministic. A missing
extensions directory is the ship-with-zero state — an empty registry, not
an error.

Every load-time violation refuses the load loudly, with a pinned text
naming the offender — never skipped, never first-wins, never silent. A
refusal is an exit-2 usage error (ADR-0430 §5's manifest class):

| Refusal | Text |
| --- | --- |
| No manifest in the package dir | `extension '<pkg>': no extension.json in the package directory` |
| Manifest unreadable | `extension '<pkg>': cannot read extension.json: <io>` |
| Manifest malformed (bad JSON, unknown kind/field, mistyped default) | `extension '<pkg>': invalid manifest: <decode detail>` |
| Name collides with a reserved command word | `extension name '<name>' collides with the reserved command word '<word>'` |
| Name collides with an installed extension | `extension name '<name>' collides with the already-installed extension (package '<dir>')` |
| Executable escapes the package dir | `extension '<pkg>': executable path '<path>' escapes the package directory` |

Every `run` invocation resolves the registry first, and a load refusal
fails the verb closed — exit 2 with the refusal, the `run` synopsis, and
the help hint — help pages included, never a partial grammar or a partial
listing.

## The mount's wire: MCP stdio

The host spawns the executable over std pipes — no shell, no argv at all.
The child receives exactly one request on stdin: its resolved flag values
(every flag's typed default when absent, its given value when present) and
its positional when one was given, keyed by the names the manifest
declares — one compact JSON line. It never sees raw argv, the
`--database` value, a store path, or any other host flag (ADR-0430 §6):
all store handling is host property. The child inherits the user's working
directory.

From then on the traffic is MCP's: the child writes JSON-RPC 2.0 requests
on stdout and reads the host's responses from stdin. The child is the MCP
client (it drives the import); the host is the MCP server (it alone owns
the `Database`) — `initialize`, `notifications/initialized`, `tools/list`
and `tools/call`, the subset the five tools need, so a child written
against an MCP SDK mounts unchanged. Stderr is the child's own; the host
drains it capped (64 KiB retained, past that truncated with a marker) and
rides it into the exit-failure errors, sanitized.

A session whose first request is not `initialize`, or which ends without
one, fails closed (exit 1).

### The Transport contract

Everything a child has to implement, in language-neutral terms — the
section the generated glue (`relate extension init`, [Verbs and
lifecycle](#verbs-and-lifecycle)) is a copy of. Every generated glue file
names this section as its source of truth: when the contract changes here,
a package re-scaffolds or copies this section again. There is no client
package to version, no lockfile to pin, and nothing to publish.

1. **Read the envelope.** One line on stdin, before anything else: a compact
   JSON object keyed by the manifest's flag names, each resolved to its
   declared default or to the value given on the command line. A declared
   positional rides under its own name (`{"dir": "."}`). Nothing else
   arrives — no argv, no `--database` value, no store path.
2. **Speak JSON-RPC 2.0 over the same two pipes.** The child writes one
   request per line on stdout and reads the host's answers on stdin; ids
   correlate requests and answers, so a reader that skips a message it did
   not ask about stays correct. `initialize` is the first request, the
   id-less `notifications/initialized` follows it, and `tools/call` carries
   every unit of work.
3. **Call the five tools by name** — `write_cypher`, `read_cypher`,
   `update_schema`, `batch_cypher`, `report_output` — with the arguments of
   [The five tools](#the-five-tools). A result is the tool's JSON object
   carried as MCP `content[0].text`; a refusal is an answered JSON-RPC error
   (`-32602`), never a closed pipe.
4. **Stay inside the caps** of [The caps](#the-caps): 8 MiB per statement and
   per response line, 1000 rows per writing call, 4096 statements per batch
   call, and — under `whole-import` framing only — 128 MiB across the
   session. The `initialize` result republishes
   them (`maxStatementBytes`, `maxRowsPerStatement`, `maxStatements`,
   `maxStreamBytes`) together with the store's capacity, so a child sizes its
   chunks from the handshake instead of hard-coding numbers; a `per-batch`
   session has no whole-run cap (ADR-0598) — its bounds are the per-call
   caps and the store's live-graph limit.
5. **End quietly.** The last call is `report_output` (the run's summary);
   the child then ends its stdout and exits 0. A non-zero exit, a killed
   child, or a stream that never initializes fails the run — and under
   `whole-import` rolls the session's transaction back (exit 1).

The reference implementations of this contract ship with the repository:
`examples/mcp-client.sh` (bash, the kit a bash package inlines),
`examples/so-to-graph.py` and `examples/so-to-graph.ts` (the same walk on the
official MCP SDKs' `Client` with a transport over the pipes the child was
handed), and `extensions/relate-wire`'s `mcp::serve`/`McpChild` (Rust, the
kit both sides of the mount share).

#### Language tiers

| Tier | Languages | What the author gets |
| --- | --- | --- |
| **First-class** — `extension init --lang` | `sh`, `py`, `ts`, `rust` | The generated glue above: the envelope read, the JSON-RPC transport, a minimal client (or, for Rust, the wire kit's usage as source), a TODO-marked `walk()`, and a README quickstart — with no published client dependency to install or manage |
| **Supported by construction** | `go`, `java`, `csharp`, `swift`, `lua`, `elixir`, `ruby` | Any MCP client works: an official SDK's `Client` over a `Transport` implemented on the two pipes the child was handed (the SDKs' stock stdio transports spawn a server; here the host spawned *us*), or the raw JSON-RPC path of the five moves above. This one is the SDK's own custom-transport seam, not a Relate package |

Both tiers are the same wire; the first-class four are the ones this CLI can
generate a package for.

### The caps

The host caps what an untrusted child can make it hold, ADR-0377's
semantics — each bound a named constant, every refusal a typed error with
pinned text, each per-call refusal *before* the store write it would have
fed:

| Cap | Value |
| --- | --- |
| Statement bytes (and the same bound per response line) | 8 MiB |
| Rows per `write_cypher` call | 1000 |
| Statements per `batch_cypher` call | 4096 |
| Stream bytes across the session — `whole-import` framing only | 128 MiB |

The numbers are the wire kit's (`relate_wire`), the handshake publishes
them as `maxStatementBytes`, `maxRowsPerStatement`, `maxStatements` and
`maxStreamBytes`, and the child kit chunks its rows against them so a
first-party import refuses loudly instead of being killed.

The stream cap is scoped by the commit policy (ADR-0598): it guards the
one framing that retains work across calls — a `whole-import` session's
single transaction, whose overlay holds every write until its one commit.
A `per-batch` session retains nothing between calls, so it has no
whole-run cap at all: every call is bounded by the per-call caps, and
the retained work is bounded by the store's live-graph limit, which the
write engine enforces with a typed capacity refusal. That is what lets a
per-batch import (git-to-graph) stream a history whose wire bytes exceed
128 MiB — and lets re-running the same command complete a partially
imported store, the contract per-batch framing exists to keep.

### Handshake

The handshake is MCP's: the child's `initialize` names the revision it
speaks (`protocolVersion`) and its identity; the host answers with the
revision it serves, its capabilities and its identity, plus the mount's
own store facts; the child then confirms with the id-less
`notifications/initialized`.

Child → host (the envelope reached it first, one line on stdin):

```json
{"jsonrpc":"2.0","id":1,"method":"initialize",
 "params":{"protocolVersion":"2025-06-18","capabilities":{},
           "clientInfo":{"name":"my-importer","version":"0.1.0"}}}
```

Host → child (the `initialize` result):

```json
{
  "protocolVersion": "2025-06-18",
  "capabilities": {"tools": {"listChanged": false}},
  "serverInfo": {"name": "relate", "version": "0.13.0"},
  "freshStoreRequired": false,
  "commitPolicy": "per-batch",
  "caps": {
    "maxStatementBytes": 8388608,
    "maxRowsPerStatement": 1000,
    "maxStatements": 4096,
    "maxStreamBytes": 134217728,
    "maxRequestBytes": 50332672
  },
  "capacity": {
    "storeLimitBytes": 1073741824,
    "storeUsedBytes": 258,
    "storeRemainingBytes": 1073741566
  }
}
```

Child → host (no id: a notification, never answered):

```json
{"jsonrpc":"2.0","method":"notifications/initialized"}
```

`protocolVersion` is the MCP revision the mount serves (`2025-06-18`); a
client that names another revision is answered with this one — MCP's rule
for a version the server does not speak — and a client that cannot speak
it fails the handshake on the decode. `initialize` params without a
string `protocolVersion` are the `-32602` refusal
`invalid tools/call params: initialize requires a string
"protocolVersion"`, and the session stays uninitialized.

`maxRequestBytes` is the per-request framing cap: the statement cap plus
the JSON envelope allowance, derived by the wire kit's one function
(`request_bytes_cap`), which the host's capped line read uses too. A
statement of exactly `maxStatementBytes` therefore always fits one
request line.

### Store capacity

The handshake publishes the host-owned store's capacity — the engine's
live-graph accounting (ADR-0042), read through `Database::memory_usage`
— so an importer can size its work against the budget it is filling
before it writes.

| Field | Meaning |
| --- | --- |
| `storeLimitBytes` | The configured live-graph + index cap in bytes. `18446744073709551615` (`u64::MAX`) means unlimited — the engine's zero-cap spelling, published as a bound so one derivation covers both cases |
| `storeUsedBytes` | The store's live logical bytes at the handshake |
| `storeRemainingBytes` | `storeLimitBytes - storeUsedBytes`, saturated at zero |

All three are raw-byte `u64`s, and `storeUsedBytes == storeLimitBytes -
storeRemainingBytes` holds whether the store is capped or not. The
numbers are the store's committed state: the host reads them from the
`Database` it owns after each writing call, never from the child's
requests.

Every `write_cypher` and `batch_cypher` result repeats the remaining
side after its write, so a child needs no second call to learn what it
has left. Subtracting two results gives exactly the bytes its own calls
cost:

```text
usage between calls = storeRemainingBytes(before) - storeRemainingBytes(after)
```

Under `per-batch` every writing call commits, so the number moves per
call. Under `whole-import` the store holds the session's writes in one
transaction until the session's commit, so the numbers stay at the
handshake's reading for the whole session; a child that needs per-call
feedback declares `per-batch`.

After the handshake the host serves `tools/list` and `tools/call`. A
message without an id is a notification and is read and ignored;
`notifications/*` are the only methods that may omit the id — a `tools/call`
without one is fatal (`the MCP request 'tools/call' carries no id; only
notifications may omit it`), because a child waiting for an answer it can
never get would hang the session. The tool names, the `tools/list` result,
the MCP revision, and the JSON-RPC record shapes are the wire kit's single
home (`relate_wire::mcp`); the child-side client kit is `McpChild` in
that module.

### The five tools

Result content is the tool's JSON object carried as MCP
`content[0].text` (`{"content": [{"type": "text", "text": "<json>"}],
"isError": false}`). `tools/list` marks `read_cypher` and
`report_output` with `readOnlyHint: true`; `write_cypher` and
`batch_cypher` carry `destructiveHint: true`, `read_cypher`,
`update_schema` and `report_output` carry `destructiveHint: false`. The
child reads each response under the same 8 MiB record
cap it writes its requests under, so a result must fit one line.

| Tool | Params | Result content | Notes |
| --- | --- | --- | --- |
| `write_cypher` | `{statement: string, parameters: object}` | `{counts: {...}, storeRemainingBytes: u64}` | `parameters` feeds `query_with_params`; a `rows` array feeds `$rows` — the importer shape, one call per chunk. Counters are the shared wire vocabulary (`nodes_created`, …). `storeRemainingBytes` is the store's budget after the write (see [Store capacity](#store-capacity)) |
| `read_cypher` | `{statement, parameters}` | `{columns: string[], rows: array}` | Classified through `EXPLAIN`'s plan-derived type, so an `OPTIONAL MATCH` read passes (the pattern's optionality never changes the class); a non-read statement is refused with `Only MATCH queries are allowed for read-query` |
| `update_schema` | `{statement: string}` | `{created: string[]}` | DDL (indexes, constraints), guarded by the plan's own type through `EXPLAIN`: a data write or a read is refused with `Only DDL queries are allowed for update-schema` before it touches the graph. `created` is the set difference of the `SHOW INDEXES` / `SHOW CONSTRAINTS` names across the call |
| `batch_cypher` | `{statements: [{statement, parameters}]}` | `{results: [counts…], storeRemainingBytes: u64}` | All statements in ONE transaction (one per call under `per-batch`; the session's one transaction under `whole-import`). A failing statement is atomic per call: `per-batch` rolls the call back and answers `-32602`; `whole-import` ends the run (the session's one transaction cannot roll a single call back) with `the extension MCP batch failed inside the whole-import transaction: <detail>`, so nothing commits. `storeRemainingBytes` is the store's budget after the call |
| `report_output` | `{text: string, starters?: string[]}` | `{}` | The run's summary: the host stores text + starters and prints/hands them off through `print_summary` and the REPL handoff |

### Traps and framing

The host owns the store lifecycle: the `requires-fresh-store` check runs
before the child spawns (and its fact is published in the initialize
result); the manifest's commit policy
frames the session through the shared write layer (`per-batch` commits
each writing call, `whole-import` opens one transaction around the whole
session and closes it at the end); the run ends with the same summary
rendering, interactive handoff and final checkpoint. There is no
begin/end transaction tool — external transaction control is not
exposed.

The child's exit is classified at the request stream's end, inside that
framing: a child that completes its tool calls and then exits non-zero
fails the run and — under `whole-import` — rolls the session back, so a
failed run never leaves a committed store.

The caps bound every call: the statement byte cap, the rows-per-statement
cap, the statements-per-call cap, the whole-import stream cap (scoped by
the commit policy — a `per-batch` session runs uncapped across the
session, ADR-0598), the per-request
framing cap (derived from the statement cap plus its JSON envelope
allowance), and — in the response direction — the statement cap one
response line may carry (the same 8 MiB the child's capped response read
runs under, so a result the child could not read is refused instead of
written). Every payload is checked before it reaches the engine. A
per-call cap, an unknown tool, a bad payload, a read or DDL refusal, and
an engine failure are JSON-RPC error responses:

| Situation | JSON-RPC code | Message |
| --- | --- | --- |
| A line the strict decode refuses (not JSON, a missing required member such as `jsonrpc` or `method`, an unknown member) | `-32700` | `malformed JSON-RPC message: <detail>` |
| A request that is not JSON-RPC 2.0 | `-32600` | `unsupported JSON-RPC version '<version>'` |
| An unknown (non-notification) method | `-32601` | `unknown method '<method>'` |
| An unknown tool | `-32602` | `unknown tool '<name>'` |
| A malformed `tools/call` payload | `-32602` | `invalid tools/call params: <detail>` |
| The statement byte cap | `-32602` | `the statement exceeds the <n>-byte statement cap` |
| The rows cap | `-32602` | `the call carries <n> rows, over the <n>-row cap` |
| The statements cap | `-32602` | `the call carries <n> statements, over the <n>-statement cap` |
| A read of a writing/schema statement | `-32602` | `Only MATCH queries are allowed for read-query` |
| A data write or read through `update_schema` | `-32602` | `Only DDL queries are allowed for update-schema` |
| A tool result over the response line cap | `-32602` | `the tools/call result would exceed the <n>-byte response cap` |
| An engine refusal | `-32602` | `engine error: <engine text>` |

The per-request framing cap (`maxRequestBytes`), the whole-import stream
cap (a `per-batch` session is never refused on it — its retained work is
bounded by the store's live-graph limit, ADR-0598), a
request that needs an answer but carries no id, a failed `batch_cypher`
under `whole-import`, a session that never initializes, and a child that
writes no first request within ten seconds are fatal: the host kills and
reaps the child (`the extension MCP request exceeds the <n>-byte record
cap`, `the extension MCP request stream exceeds the <n>-byte
whole-import stream cap; window the import or move the package to
per-batch framing`,
`the MCP request '<method>' carries no id; only notifications may omit
it`, `the extension MCP batch failed inside the whole-import transaction:
<detail>`, `the extension child ended its MCP session without an
initialize request`, `the extension wrote no MCP request within 10s`).

The read deadline bounds the handshake window only: the host waits at most
ten seconds for the child's first request line, so a child that holds its
pipes and writes nothing fails the run with its own refusal instead of
hanging it. Once that first request has arrived the session is established
and later reads block without a bound — a child may do arbitrarily
expensive work (a whole tree walk, a full `git log`) before its first tool
call without being killed. Streaming rows as you collect them remains the
recommended pattern, but it is not required to survive the deadline. The
`check` verb differs deliberately: as a diagnostic it applies the same
ten-second bound to every stage, while the mount applies it only to the
handshake. A child waiting for an answer the host will never write — an
id-less non-notification — is likewise turned into a failure instead of a
hang.

## Host execution

The `run` verb dispatches into the host, which runs the same import
lifecycle the CLI's compiled-in importers used (ADR-0430 §7):

1. **Open the host-owned store** — the `:memory:` default, or the target
   an explicit `-d`/`--database` named. The child is never told either.
2. **Fresh-store check** — a `requires-fresh-store` action refuses a
   target that already holds nodes *before* the child spawns, so a
   refusal costs no child: `the extension '<name>' requires a fresh
   store, but the target holds N nodes` (exit 1).
3. **Spawn and admit** — spawn the executable (an unreadable or
   non-executable path is the spawn-time refusal, exit 1), write the
   envelope, then drive the JSON-RPC session: the child's requests are
   answered until its stdout ends, every writing call runs through the
   shared write layer under the manifest's commit policy, and
   `report_output` supplies the summary.
4. **The summary** — the child's one-line result, then each starter
   indented two spaces, printed through the host's sanitization.
5. **Handoff and exit** — an explicit `--database` exits after the
   summary (a persistent target takes its final checkpoint); an omitted
   one opens the interactive shell on a TTY with the graph and the
   starters in it (ADR-0286), or exits on a non-TTY.

The commit policy is the manifest's, and the whole session runs inside it:
`whole-import` frames everything in one explicit transaction, so any
failure rolls the entire import back (ADR-0157); `per-batch` commits each
writing call as it lands, so every call before a failure stays committed
(ADR-0392). A `write_cypher` call carrying zero rows writes nothing — no
chunk, no transaction.

## Errors and exit codes

ADR-0430 §5's split, the CLI's closed map:

| Exit | Class | When |
| --- | --- | --- |
| `0` | success | The session ended clean (with or without a report) |
| `2` | usage | The invocation was malformed before anything ran: a manifest problem (unknown kind/field, a collision, an escaping executable, a protocol this host does not speak, a duplicate or store-spelling flag), a flag problem (unknown flag, a value that does not parse as its kind), an uninstalled name, a missing name, a registry load refusal |
| `1` | runtime | The work was invited and failed: a protocol violation (a request before `initialize`, an id-less request that needs an answer, a fatal framing or whole-import stream cap), a non-zero child exit, a killed child, a failed `batch_cypher` under `whole-import`, a fresh-store refusal. Per-call refusals (a malformed line, an unknown method, an unknown tool, a cap on one payload, a read or DDL guard, an engine failure) are answered as JSON-RPC errors and do not fail the run |

A runtime failure renders as one `Error:` line and exits 1; a usage
error renders the refusal, the relevant synopsis (`run`'s own, or the
action's — manifest data, sanitized), an optional `Did you mean` line
drawn from the action's own flag table, and the help hint, and exits 2.
The action-scope synopsis is rendered from the manifest (the action's
`Usage: relate run <name> [DIR] [--flag VALUE] …` line), and every
manifest- or child-authored fragment prints through the host's
sanitization, so a package author can never forge a second line, ANSI, or
control characters into the host's output.

The uninstalled-name refusal is pinned byte-for-byte (ADR-0429 §7,
LABS-552 §4) — discovery lists what is installed, it never fetches:

```text
Error: no installed action 'git-to-graph' — first-party extension available: relate extension install git-to-graph
Usage: relate run <name> [flags]
Run 'relate run --help' for more information.
```

## Verbs and lifecycle

Two verbs are the extension program's reserved words (ADR-0429 §1):
`run` — the mount, this surface — and `extension` — the lifecycle verb
(ADR-0431, LABS-469):

```text
relate extension init <name> [--lang sh|py|ts|rust]
relate extension install <name> [--github REPO@TAG|@SHA] [--url URL]
                               [--from DIR] [--version V] [--force]
                               [--re-approve | --approve SHA]
relate extension list [--available | --trust]
relate extension remove <name> [--forget-pin]
relate extension check <dir>
```

Both ship with this CLI. `extension` is a recognized command word in
every build — without the `extensions` feature it answers with the
not-included error (ADR-0104). `init`, `list`, `remove` and `check` ride
`extensions` (dependency-free); `install` and `list --available` ride
`extension-install` (`full` and `agent`, never `minimal` — the network
dependencies gate the install verbs only).

`init` scaffolds a package in the working directory: `<name>/` with the
manifest, the executable of `--lang` (`sh`, `py` — the default — `ts` or
`rust`) and a README quickstart. The executable is the language's minimal MCP
client generated inline — the [Transport contract](#the-transport-contract)'s
glue, a TODO-marked `walk()`, and no published client dependency — so the
directory is installable and runnable as it is:

```text
relate extension init my-ext            # ./my-ext/{extension.json,my-ext.py,README.md}
relate extension install my-ext --from ./my-ext
relate run my-ext -d ./store
```

The generated manifest declares the command stratum, the name, version
`0.1.0`, this host's MCP revision as `protocol`, one sample flag (`--name`,
default `there`) and the `per-batch` commit policy; the author extends it
as the walk grows. The sample `walk()` binds that flag end to end — the
envelope value becomes a Cypher parameter and the read's result is the
summary — so the contract an author must get right is demonstrated, not
just described. The
directory is created fresh — an existing one is a refusal (exit 1), never a
merge — and a failed scaffold leaves nothing behind. The name is held to the
installer's one package-name rule — a single directory segment — and never a
reserved command word; a bad name, an unknown `--lang` or a missing name is a
usage error (exit 2).
The [author guide](authoring.md) walks the full loop. The host still ships with
zero installed extensions, and a hand-placed directory under the XDG
extensions dir works. The contract (ADR-0431, ADR-0449): first-party is
the release channel and its one signing root (`extensions.json` beside
`releases.json`, SSHSIG like `upgrade`); `--github REPO@TAG|@SHA` installs
a package from a GitHub release pinned to a commit (below); `--from DIR` installs an
unpacked package directory offline — the CI and air-gap path, the
executable resolving inside the directory it names (an
intermediate-directory symlink is a refusal); `--url URL` is the
explicit third-party trust act, https only — a non-https URL is a usage
error before any fetch; every install runs the same manifest, protocol
and collision checks and records provenance; nothing pre-seeds, nothing
auto-installs.

### The GitHub pin tier (`--github`)

`--github OWNER/REPO@TAG` and `--github OWNER/REPO@<FULL-COMMIT-SHA>`
install a package from a GitHub release. There is no signature root for
a third-party repository, so the pin is the trust: the first install
records exactly which commit a tag resolved to and which bytes the
platform asset had, and every later install is compared against that
record.

- **An untagged `OWNER/REPO` is a usage error** — a repository without a
  pin is a moving target — as is anything that is not `owner/repo@ref`.
  A 40-hex value after `@` is read as the commit pin.
- **Resolution** (before the package itself is fetched): the REST API's
  commit lookup resolves the tag to its commit (`GET
  /repos/OWNER/REPO/commits/TAG`), and the release-tag lookup yields the
  release's assets (`GET /repos/OWNER/REPO/releases/tags/TAG`). The
  package asset is the one whose name matches the first-party
  convention `<name>-<version>-<os>-<arch>.tar.gz` for the host platform
  and the requested package name; zero or several candidates refuse. Its
  SHA256 is GitHub's own recorded `digest` (`sha256:<hex>`); an asset
  without one refuses. A `@commit` pin scans the newest 100 releases for
  the tag that targets that commit; none or several refuse.
- **The trust store** is the per-user file
  `$XDG_CONFIG_HOME/relate/extensions-trust.json` (falling back to
  `~/.config/relate/extensions-trust.json` through the
  config-dir rule), a flat object keyed by extension name, each entry
  `{repo, tag, commit, assetSha256}`. The first install records its pin;
  the invocation is the approval. The store is read *before* any
  package fetch and it outlives `remove`, so removing the package and
  reinstalling cannot bypass the pin. A pin is recorded only once its
  package has landed: a failed install leaves the store byte-identical
  to before the attempt.
- **A moved pin refuses before any fetch**: when the same tag resolves
  to a different commit, or the asset's SHA256 changed, the install
  exits 1 naming the recorded pin beside the resolved one. Nothing is
  downloaded and nothing lands.
- **Re-approval** is explicit: `--re-approve` accepts whatever the pin
  resolves to now, and `--approve <FULL-COMMIT-SHA>` accepts only a
  resolution whose commit is exactly that SHA. Either way the new asset
  must verify against its resolved SHA256 first; only then is the
  recorded pin updated. A reinstall with the identical pin succeeds
  without either flag.
- **Inspecting and clearing** are offline, dependency-free operations:
  `relate extension list --trust` prints every recorded pin (name,
  repo, tag, commit, asset SHA256), and `relate extension remove
  <name> --forget-pin` clears one. A plain `remove` keeps it.
- **The download** rides the same probe-then-fallback route as the
  first-party channel: the direct
  `https://github.com/OWNER/REPO/releases/download/<tag>/<asset>` URL,
  falling back to the authenticated API asset route on a typed 404 with
  `GITHUB_TOKEN` set (`RELATE_RELEASE_DOWNLOAD_URL` and
  `RELATE_GITHUB_API_URL` are the documented mirror and test
  seams). The downloaded bytes must match the resolved SHA256 before
  anything unpacks.
- **Provenance**: the marker records the tier `github-pin`, the source
  `OWNER/REPO@TAG`, and the pin (`repo`, `tag`, `commit`, `assetSha256`)
  itself. `extension list` shows the tier; the loud install line names
  the repository, the tag and the commit.

The result is the tier ladder's fourth rung: first-party (the signed
channel), github-pin (a commit-pinned GitHub release), third-party
(`--url`, unsigned — the invocation is the act), local (`--from`, the
directory is the act).

The importer extraction has landed (ADR-0431 §11): `fs-to-graph` and
`git-to-graph` are first-party extension crates speaking MCP stdio — the
channel's first real content. The crates live in the workspace's
`extensions/` group
(the placement ADR-0431 §11 records), each with its committed
`package/extension.json` beside its source — the manifest the release
pipeline will pack beside the built executable, and the one manifest the
CLI e2e installs and drives (`surfaces/relate-cli/tests/e2e_fs_to_graph.rs`
and `surfaces/relate-cli/tests/e2e_git_to_graph.rs`, the per-subject
importer suites).
Two summary deltas the wire forces on the importers, recorded here because
the e2e pins them: the child's summary names its target `the store` (the
child never sees the store path, ADR-0430 §6), and the closing pointer to
the question catalog rides as the summary's last starter (the summary
record's shape: text, then starters).

### The check verb

`relate extension check <dir>` runs a package directory through the
stages the mount itself needs, in the mount's order, printing one line per
stage and stopping at the first failure:

| Stage | Passes when | Failure class (exit) |
| --- | --- | --- |
| `manifest` | `extension.json` is present, decodes strictly, the executable stays inside the package directory, the mount name is not a reserved command word, and the manifest is a grammar the mount accepts (the `protocol` this host serves, no duplicate or store-shadowing flag names) | 2 |
| `executable` | the declared file exists, is a regular file, and carries the exec bit | 2 |
| `spawn` | the child starts over the mount's std pipes and takes its request envelope (the flag table's defaults) | 1 |
| `initialize` | the child's first request is `initialize`, names the MCP revision this host serves, and is answered with the mount's own `initialize` result | 1 |
| `notifications/initialized` | the child confirms the handshake with MCP's id-less notification | 1 |
| `tools/list` | the check serves the shared [tool table](#the-five-tools); a `tools/list` request is answered with it and a `tools/call` must name one of its tools | 1 |

A clean run then closes with one status line about the package's recorded
GitHub pin ([ADR-0449](../../adr/0449-github-install-trust-pins.md)):

| Line | Meaning |
| --- | --- |
| `pin: approved (repo@tag commit …)` | the provenance marker's pin is exactly the [trust store](#the-github-pin-tier---github)'s recorded approval for this extension |
| `pin: drifted (…)` | the marker's pin is not the approval — a moved tag, a changed asset, or no entry at all; the line names both sides and the `--re-approve` command |
| `pin: not from a GitHub source (first-party \| third-party \| local \| unmarked)` | the package did not arrive by the `--github` tier, so there is no pin to compare |
| `pin: unknown (…)` | the marker or the trust store could not be read (or a `github-pin` marker carries no pin); the refusal is named, never worn as one of the states |

The pin line is a report, never a stage: **it does not change the exit
code**. A drifted pin still mounts — the installed bytes are the ones that
were approved and the mount never consults the marker — so the check keeps
answering "would this package mount". Drift is refused where it matters,
before any fetch, by `install --github`'s pin comparison (the remedy the
line names).

The verb opens no store, writes nothing and runs no tool: the handshake it
answers publishes an empty, unlimited capacity, the session ends once the
tool table is served, and the child's own walk never runs. Its two local
reads — the package's provenance marker and the per-user trust store — are
files, never a graph store, and nothing is fetched. So a check is safe
against any store, needs no network, and proves the mount contract rather
than the import. No `--database` is accepted — the directory is the whole
invocation.

Exit codes are the split of [Errors and exit codes](#errors-and-exit-codes): **2** for the package's
declaration and contents (the two stages above it), **1** for what it does
when run, **0** when it would mount. Each expected request waits at most
ten seconds — the same value the `run` mount's handshake window uses, but
applied here to every stage because the check is a diagnostic — so a child
that writes nothing fails its stage instead of hanging the verb. A clean
package prints:

```text
$ relate extension check ./greet
manifest: ok (greet 0.1.0, protocol 2025-06-18)
executable: ok (run.sh)
spawn: ok
initialize: ok (MCP 2025-06-18, server relate 0.13.0)
notifications/initialized: ok
tools/list: ok (the shared tool table: 5 tools; the extension calls 'report_output')
pin: not from a GitHub source (unmarked)
```

## The channel and its publisher

The first-party channel is this repository's release channel
(ADR-0431 §1): "first-party" means *published here*, nothing else —
one signing root (the release key that also signs `SHA256SUMS`),
one route machinery (`upgrade`'s probe-then-fallback), one os/arch
vocabulary shared with the platform binaries the upgrade picks. The
index and the packages are release artifacts:

- **The index** — `extensions.json` and its SSHSIG signature
  `extensions.json.sig` at the website root beside `releases.json`,
  written only by the release pipeline
  (`.github/scripts/publish-extensions-json.sh`, run by
  `publish-extensions-json.yml`), never hand-edited. It is a full
  snapshot regenerated from the live release list on every publish and
  backfill (the ADR-0250 §6 publisher contract), and it is signed or
  not published at all.
- **The packages** — one GitHub release per package version, tagged
  `relate-extensions-<name>-<X.Y.Z>`, carrying the per-platform
  tarballs `<name>-<X.Y.Z>-<os>-<arch>.tar.gz` as assets (`os`/`arch`
  in the channel scope: `darwin`/`linux`, `amd64`/`arm64`). A package
  release bumps only its index row; the CLI release need not move
  (ADR-0431 §8). The publisher refuses anything that breaks the
  convention — an unparseable tag, an off-convention asset name, a
  tarball whose manifest names another package — never a silently
  partial index.
- **The release path** — the manual dispatch
  `.github/workflows/release-extensions.yml` builds the listed
  package (one row of `distribution/extension-packages.json`: the
  package name, the crate directory that builds its executable, and
  its `extension.json`) for the four channel platforms, signs and
  notarizes the darwin executables (the CLI binary's own machinery),
  packs the ADR-0431 §2 tarballs, tags after green builds, cuts the
  release, then chains the index publisher and the website deploy.

The package table lists the first-party importers
(`distribution/extension-packages.json`: `fs-to-graph` and
`git-to-graph`, ADR-0431 §11) — the rows the release workflow's
dispatch resolves. The channel itself opens with no published index at
all: until the first publisher run the website file does not exist and
`extension list --available` fails closed with the missing-index
refusal; once the pipeline has published (a release or the backfill
dispatch), the same command answers the signed catalog — an empty
`packages` list until the first package release, then one row per
released package — and `extension install <name>` answers the
unknown-package refusal naming the channel until that package's first
release.

Compatibility: the manifest's `protocol` — the MCP revision — is the only
coupling between a host and a package, not the CLI version nor the release
tag. The host and a package rev freely while both name the same revision; a
revision bump is a new string, and old hosts fail closed on the mismatch,
naming both (ADR-0431 §8, ADR-0447 §2).

## Telemetry

A `run` reports command `run`, action `extension` — never the installed
action's name (ADR-0429 §9, ADR-0079's anonymity rule). Builds without the
`telemetry` feature (the `agent` and `minimal` bundles) record and send
nothing.

## The `ideas` package

A first-party, `hidden` command-kind package (crate `extensions/relate-ideas`):
a small linked idea store inside the host's store. It parses and runs like any
extension but appears in no `run` listing and no did-you-mean. The child links
no `Database` and no surface; every verb is `read_cypher` / `batch_cypher`
through the wire kit, and the summary goes out through `report_output`.

```bash
relate extension install ideas --from extensions/relate-ideas/package
relate run ideas -d ideas.rl --command seed
```

Flags (the manifest's table; `commit` is `per-batch`):

| Flag / positional | Kind | Default | Meaning |
| --- | --- | --- | --- |
| `--command` | string | `list` | The verb: `add`, `link`, `related`, `list`, `show`, `seed` |
| `idea` (positional) | string | absent | The idea title: the new title for `add`, the source for `link`, the subject of `related` / `show` |
| `--tag` | repeatable | none | `add`: tags to attach (at most 8; lowercase `a-z 0-9 - _`, at most 32 characters). `list`: filter by one tag |
| `--to` | string | empty | `link`: the target idea (required for `link`) |
| `--as` | string | `RELATED_TO` | `link`: the link type, one of `RELATED_TO`, `INSPIRED_BY`, `COMPLEMENTS`, `VARIANT_OF`, `SAME_DATA` (case-insensitive; `-` or space read as `_`) |
| `-d`, `--database` | host flag | `:memory:` | The store; see [Host execution](#host-execution) |

Verbs:

| Verb | Contract |
| --- | --- |
| `add <title> [--tag t]...` | Creates `(:Idea {name})` and `(:Tag)` / `[:TAGGED]` edges in one atomic batch. Titles are trimmed, 1-120 characters, no control characters, unique case-insensitively; adding an existing title only adds tags |
| `link <a> --to <b> [--as T]` | `MERGE`s `(a)-[:T]->(b)`; both ideas must exist (case-insensitive lookup), no self-links; idempotent |
| `related <idea>` | One line per link, `-> TYPE other` outgoing and `<- TYPE other` incoming |
| `show <idea>` | Summary, category, tags and links |
| `list [--tag t]` | Titles with categories, ordered by title, at most 200 rows |
| `seed` | One atomic batch of `MERGE`s loading the embedded seed set (34 ideas, 46 links: title, one-line summary, category and the links between them). Re-running refreshes and never duplicates |

Output: the first line is the report's summary; row lines ride the report's
`starters`, which the host prints one per line, indented, through its
sanitizer (no ANSI or control characters reach the terminal). A user mistake
(unknown idea, bad tag, off-vocabulary link type, unknown verb) fails the run
with the message on stderr and exit 1. The seed's idea nodes carry
`source: 'seed'`; user-added ones `source: 'user'`.

## Examples

[authoring.md](authoring.md) — the author guide: the `init → edit → check →
install --from → run` loop, the language tiers, the worked scaffold, a
worked failing check with its fix, publishing, and the manifest fields to
grow; the check verb's six stages and exit-code split are
[above](#the-check-verb). [install.md](install.md) — the install guide: the
trust tiers, what the loud install line and the provenance marker mean, and
`list`/`remove`.

[quickstart.md](quickstart.md) — scaffold a package or copy a shipped
template, `extension install --from`, `relate run`, and read the graph
back: the shipped example in bash, Python and TypeScript
(`examples/so-to-graph.sh|.py|.ts`). All three walk the live Stack Exchange
API (a live Stack Overflow corpus), so they need network access
and a fresh store, and CI gate them for syntax only — [`scripts/check-extension-examples.sh`](../../../scripts/check-extension-examples.sh)
(`bash -n` both shell files and the inlined-kit pin, `python3 -m
py_compile` plus the example's parse path when the `mcp` SDK is installed,
`node --check` after a type strip). The offline, executed coverage is the
fixture suites below: the shipped children are not run in CI.

[examples/run.md](examples/run.md) — author and install a fixture
package, run it into a store, and query the result. Its runnable form is
the end-to-end suite
[`surfaces/relate-cli/tests/e2e_run.rs`](../../../surfaces/relate-cli/tests/e2e_run.rs),
which runs the same fixture child through the real binary in CI (the
wire-driven tests are unix-gated, like the host and protocol suites).

[examples/ideas.md](examples/ideas.md) — the `ideas` package end to end. Its
runnable form is the crate's unit suite
(`cargo test -p relate-ideas`: the verbs through a recording host, the seed
asset's parse, the manifest's agreement with the child).

[examples/mcp-mount.md](examples/mcp-mount.md) — the MCP stdio mount's
line protocol: the envelope, the `initialize` handshake and its
confirmation, and the five tools. Its runnable form is
[`surfaces/relate-cli/tests/e2e_extension_mcp.rs`](../../../surfaces/relate-cli/tests/e2e_extension_mcp.rs)
plus the compiled fixture child the suite installs.
