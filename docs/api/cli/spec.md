# CLI — spec

Binary: `relate` (package `relate-cli`).

```
cargo run -p relate-cli -- [OPTIONS] [--database PATH] [QUERY]
cargo run -p relate-cli -- --logical-graph-size-limit 1gib ./graph
cargo run -p relate-cli -- --help
```

## Synopsis

```
relate [OPTIONS] [--database PATH] [QUERY]
relate <COMMAND> [ARGS]
relate query [STATEMENT] [--database PATH]
relate clone [--wal-dir DIR] [--to-snapshot-format F] SOURCE DEST
relate info --database PATH [--json]
relate skill install|remove|list|check [--agent NAME] [--force]
relate upgrade [--yes] [--check]
relate update  [--yes] [--check]  (alias of upgrade)
relate telemetry on|off|status
relate pull [--from FILE] --query Q [--param N=V] [--allow-plaintext] --database PATH  # Q: statement, `;`-list, file, or `-`
relate push --to CREDS --database PATH [--query Q | --labels L1,L2 | --types T1,T2] [--batch-size N] [--nodes-update POLICY] [--rels-update POLICY] [--updated-at NAME[,NAME]] [--rel-mode MODE] [--merge-key LABEL:PROP[,PROP]] [--rel-key TYPE:PROP[,PROP]] [--unkeyed-labels ACTION] [--dry-run] [--json] [--allow-plaintext]
relate run <name> [flags]
relate extension init <name> [--lang sh|py|ts|rust]
relate extension install <name> [--github REPO@TAG|@SHA] [--url URL | --from DIR] [--version V] [--force] [--re-approve | --approve SHA]
relate extension list [--available | --trust]
relate extension remove <name> [--forget-pin]
relate extension check <dir>
relate agent-context [COMMAND]
```

This is the synopsis of the default (`full`) build. Top-level `--help` lists
only the commands compiled into the binary; see [Build bundles](#build-bundles)
and [Help and usage errors](#help-and-usage-errors).

## `query` command

`relate query [STATEMENT] [--database PATH]` is the explicit statement
session (ADR-0200). It is built into every bundle. It is not an exit-after
lifecycle command: after parse it runs the same oneshot / stdin / shell
session as the path-first form.

| Input | When |
| --- | --- |
| Positional `STATEMENT`, or `-c` / `--command` | One Cypher statement, or a `;`-separated list. The two spellings cannot be combined |
| `-d`, `--database PATH` | The store **target**: a store directory, `.rl` bundle (legacy `.cl` bundles still open, ADR-0517 §5), or `:memory:` (default `:memory:`) — never a URL (a remote store is a [`--load SOURCE`](#loading-a-store-from-a-source----load-adr-0500) source). Same flag on the path-first form, `pull`, and `info` — those verbs keep local paths. `-d` is the short form |
| Stdin | No `STATEMENT`, and stdin is not a terminal |
| Interactive shell | No `STATEMENT`, and stdin is a terminal |

```bash
relate query "RETURN 2" --database /tmp/triha/my-db
relate query --database ./friends "MATCH (p:Person) RETURN p.name"
relate query "RETURN 1 AS n"
relate --database ./friends "MATCH (p:Person) RETURN p.name"
```

A directory named `query` is `--database query` or `--database ./query`. The
word `query` as the first positional is the command.

The bare form is the same session (ADR-0231): `relate` with no statement
and no command word opens the interactive shell when stdin is a terminal —
`--database PATH` still selects the store — and prints the top-level help
with exit code 0 when stdin is piped or redirected.

### Loading a store from a source — `--load` (ADR-0500)

`--database` names the store **target** — a local path or `:memory:`
(the default) — and never a URL. `--load SOURCE` names the store
**source** — a local `.rl` bundle, a native directory store, or an
`http(s)` URL to a `.rl` single-file bundle (LABS-872, LABS-903) — loaded
into the target. A `.cl` bundle from v0.16 and earlier is accepted on both
paths as the legacy extension (ADR-0517 §5). The two flags are the two axes:
`--database` decides where the store lives, `--load` decides where its
content comes from.

```bash
relate --load https://github.com/relatedb/relate/blob/main/test.rl \
  "MATCH (n) RETURN count(n)"          # into :memory: — nothing kept on disk
relate -d ./friends.rl --load https://…/test.rl \
  "MATCH (n) RETURN count(n)"          # into a durable store you named
```

- **A `:memory:` target (the default) is the ephemeral open** (LABS-903,
  ADR-0500): the source — a `.rl` bundle or a native directory — is loaded
  and then **released**. Nothing is held open or locked, the source can be
  deleted or opened writable underneath, and writes refuse exactly like
  `--read-only`'s (`WriteOnReadOnlyAccessDatabase`, exit **6**). A remote
  source's scratch copy is removed right after the load — nothing is left
  on disk at all — and the run reports `memory` storage.
- **A durable target (`-d PATH --load SOURCE`) creates the store**: the
  target path must not exist yet (exit **2** if it does), the source is
  cloned into it (the `clone` verb's own machinery, the full
  dir/`.rl` matrix), and the run then opens the created store with the
  flags' normal mode — writable by default. `--read-only` cannot govern a
  load that creates a store (exit **2**); `--read-only=live` with any
  `--load` is a parse-time usage error (exit **2**) — the source is loaded
  once and released, there is no WAL left to follow.
- **A URL in `--database` is a usage error** (exit **2**): the target is a
  local path or `:memory:`; a remote store is a `--load` source.
- A **GitHub file link** is rewritten to its `raw.githubusercontent.com`
  form — both the shareable **blob** page URL
  (`github.com/<owner>/<repo>/blob/<ref>/<path>`) and GitHub's own **raw**
  verb (`github.com/<owner>/<repo>/raw/<ref>/<path>`, which 302s to the
  bytes), so one request fetches the bytes instead of a redirect hop. A raw
  URL, a loopback test server, and any other host are fetched unchanged.
- **Redirects are followed** (ureq's default chain, up to its limit), so a
  GitHub release-asset URL or any other 302/301 that lands on the bundle
  bytes works; an `https://` fetch refuses a hop that would downgrade to
  plain `http://` (the pin below).
- **`https://` for any host; plain `http://` only for a loopback host**
  (`localhost`, `127.0.0.0/8`, `::1`) — the e2e fixtures and a local dev
  server. Any other `http://` exits **2** with `plain http:// is only
  allowed for a loopback host; use https://…`; there is no opt-in flag,
  since a database fetched over plaintext arrives unverified on a transport
  anyone on the path can rewrite. Every `https://` fetch also pins
  `https_only` on its client (the LABS-470 pin), so a redirect cannot
  downgrade the transport to plaintext either.
- The URL must name a **`.rl` file** (or a legacy `.cl` bundle, ADR-0517
  §5). A repository, `tree`, or other-verb GitHub URL, a blob URL with no
  file path, and any URL whose last path segment is not a bundle file exit
  **2** with `Error: invalid --load: …` naming the form that works. A remote
  **native directory** store is not supported: a `.rl` bundle is the one
  self-contained artifact.
- The fetch is capped at **1 GiB** and **300 s** (its own knobs, independent
  of `--fetch-params`'s 64 MiB/60 s) and **streams the body straight to the
  staged file**, so a bundle at the cap never lives in RAM; the download is
  announced on stderr (`--load <url>: downloaded to <path>, turning into the
  in-memory store (the copy is released after the load)` for a memory
  target, `--load <source>: loaded into <path>` for a durable one), and a
  non-2xx status is a runtime error (exit **1**) reporting
  `HTTP <status>`.
- **Every fetched copy is scratch.** A memory-target load's copy is removed
  right after the load (the source was released); a durable-target load's
  copy is removed the moment the clone into the target finishes; a failed
  fetch removes its own partial file. No download is ever kept on disk.
- **`--safe` refuses a remote `--load` source** (exit **2**, before any
  network I/O; ADR-0477): a downloaded store is untrusted input, so safe
  mode introduces none. (The refusal is the store's: `--fetch-params`
  sources bind a parameter, not a store, and are unaffected.)
- Without the `remote-database` feature (`full` and `agent` carry it), the
  URL still parses and the fetch fails closed (exit **1**) with
  `<url>: the remote bundle fetch is not compiled into this build (enable
  the "remote-database" feature)`.
- The fetched database has the same trust as a downloaded file: no
  signature check, no host allowlist (the ADR-0267 posture for
  `--fetch-params`), and the bundle's own open path validates it.

### Zero-effect write notes (ADR-0402)

A **batch** statement (the one-shot `STATEMENT` / `-c` form, or piped stdin —
never the interactive shell) that runs a mutation operator but applies no
change says so on **stderr**, once per statement, after the statement's own
output. The wording is pinned byte-for-byte:

```text
note: write statement changed nothing; the graph is unchanged
```

In a batch holding more than one statement the line names its position with
the same numbering as the [failed-run report](#progress-of-a-multi-statement-run)
below — a `.`-command counts as typed; empty fragments and a trailing `;` do
not:

```text
note: write statement 2 of 3 changed nothing; the graph is unchanged
```

A `.commit` that closes a transaction whose statements attempted a write and
applied nothing adds one summary line:

```text
note: .commit changed nothing; the transaction applied no changes
```

Every statement the transaction executed joins the summary's merged effect, a
plan-producing `PROFILE` included: it executed, so its applied write silences
the summary even though its own plan table replaced its note.

The criterion is the engine's own effect record (`QueryResult::effect`,
ADR-0402 Decision 2): a mutation operator reached execution **and** the engine
applied no change. It is not the plan's query type and not the mutation
counters — both are wrong for real statements:

- a **catalog change** persists while every mutation counter stays zero, so
  `ALTER DATABASE … SET DEFAULT LANGUAGE …`, `CREATE GRAPH TYPE …` and the
  preview-gated `CALL db.ontology.set(…)` / `CALL db.ontology.drop()` are
  writes that applied a change and print nothing;
- a **read-only statement the planner conservatively classifies `w` / `rw`** —
  a top-level `CALL { … }` with a read-only body, or a bare `LOAD CSV …
  RETURN` — ran no mutation operator, so it prints nothing too (and a
  transaction holding only such statements prints no summary).

The positive case is the row pipeline feeding a write nothing: `MATCH
(m:NoSuchLabel) DELETE m CREATE (n:Label {id: 1})` attempts and applies
nothing, so it reports. Rows cannot tell a no-op from a `CREATE` either — a
write without `RETURN` returns no rows whatever it wrote. The attempt fact
reaches nested bodies: a write inside a `CALL { … }` body or an untaken `WHEN`
branch counts even when that body never runs. The applied fact counts only
changes the engine kept — a failed statement restores its state and a
rolled-back batch un-counts what it truncated, so neither is reported as
applied. The rerun cases follow from the same fact: an idempotent `MERGE` that
matched and wrote nothing reports, while an idempotent `SET` that re-writes the
same value applied a property write and does not.

An empty or read-only transaction, a failed statement and the interactive
shell print nothing. A failed statement takes no note even when it executed
first: a render failure reports the failure (exit 1) and the change the engine
already applied still joins the `.commit` summary, so a persisted write is
never described as a no-op. `.rollback` prints no **summary** (the discard was
asked for), but the per-statement notes of a rolled-back transaction's
zero-effect writes have already been printed and stay on stderr. A plan
artifact takes the **note** out of scope, not the effect: `EXPLAIN` does not
execute and carries no effect, while `PROFILE` executes — its effect counts in
the transaction's commit summary, so an applied write silences it and an
attempted no-op makes it report — and prints its plan table instead of a note. Silence after a
successful batch statement therefore means it read only, it applied a change,
or it was a plan-producing run. A note is a report, never a failure: the run
keeps the exit code it earned (0 for a successful zero-effect write) and
stdout bytes are unchanged in every output mode, so an existing pipeline sees
no difference. The wording is an observable contract — grep stderr for the
**anchored prefix**, `note: write statement` for the per-statement line or
`note: .commit` for the summary; `changed nothing` alone is common to both.
See [ADR-0402](../../adr/0402-zero-effect-write-notes.md).

## Build bundles

The CLI's optional capabilities are Cargo features on `relate-cli`, grouped
into named bundles (ADR-0104). Every feature only adds capability. The
query-engine run — every flag under [Query-engine flags](#query-engine-flags),
one-shot, piped stdin, and interactive input, in-memory and persistent — is
in every bundle, and every flag parses in every build. A flag value can still
require a compiled-in capability: `--snapshot-format parquet` needs the
`parquet` feature and fails at open without it (ADR-0105).

| Feature | Provides | Optional dependencies | Without it |
| --- | --- | --- | --- |
| `repl` | rustyline line editor for the interactive shell (history, editing) | `rustyline` | Interactive mode reads plain stdin lines (same prompts, no cursor-key editing or history); `--help` omits the readline note |
| `pull` | `relate pull` | `relate-load` | `pull` is a not-included command |
| `push` | `relate push` (ADR-0401): pull's mirror — push a scoped subgraph of `--database` into a Neo4j/Aura remote over the same Query API v2 client | `relate-load` | `push` is a not-included command |
| `skill` | `relate skill install\|remove\|list\|check` and the embedded skill bundle | none | `skill` is a not-included command |
| `upgrade` | `relate upgrade` and its `update` alias | `ureq`, `sha2`, `ssh-key` | `upgrade` / `update` are not-included commands |
| `telemetry` | `relate telemetry on\|off\|status` and all usage-metrics emission | `ureq`, `rand` | `telemetry` is a not-included command; no events are recorded or sent, no first-run notice, no config/state files, no network |
| `fetch-params` | the `--fetch-params NAME=SOURCE` fetch (http(s) URL or file, parse JSON, bind as the parameter `NAME`; ADR-0267) | `ureq` | The flag still parses in every build (ADR-0270); a `--fetch-params` value fails closed at resolution (exit 1) with `--fetch-params is not compiled into this build (enable the "fetch-params" feature)`; `--help` omits the flag |
| `remote-database` | the remote `--load SOURCE` fetch (LABS-872): an `http(s)` URL naming a `.rl` bundle is downloaded to a scratch copy and loaded into `--database` (ADR-0500) | `ureq` | A URL source still parses in every build (ADR-0104) and every policy refusal still applies (safe mode, `--read-only=live`, unsupported URL shapes); the fetch fails closed (exit 1) with `<url>: the remote bundle fetch is not compiled into this build (enable the "remote-database" feature)`, and `--help` omits the URL lines |
| `parquet` | Parquet snapshot codec (`relate/parquet`, ADR-0105) | `arrow`, `parquet` (through `relate`) | New directories and `.rl` bundles default to JSON; `--snapshot-format parquet`, `RELATE_SNAPSHOT_FORMAT=parquet`, or opening an existing Parquet store fails at open (exit 1) with `parquet snapshot codec is not compiled into this build (enable the "parquet" feature)`; `--help` lists `(json, markdown)` only |
| `info` | `relate info` (ADR-0165) | none | `info` is a not-included command |
| `serve` | `relate serve` with no surface compiled in (ADR-0232) | `relate-auth`, `ctrlc` | implied by `http`, `mcp`, and `bolt`; enabled alone, `serve` parses but every surface flag is an unknown flag, so the run is a usage error |
| `http` | `relate serve --http PORT` (ADR-0232) | `relate-http`, `relate-auth`, `ctrlc` | `--http` is an unknown flag; with `mcp` also absent, `serve` is a not-included command |
| `mcp` | `relate serve --mcp PORT|stdio` (ADR-0232, ADR-0375) | `relate-mcp`, `relate-auth`, `ctrlc` | `--mcp` is an unknown flag; with `http` also absent, `serve` is a not-included command |
| `bolt` | `relate serve --bolt PORT` (ADR-0232; LABS-298) | `relate-bolt`, `relate-auth`, `ctrlc` | `--bolt` is an unknown flag; with `http`/`mcp` also absent, `serve` is a not-included command |
| `agent-context` | `relate agent-context [COMMAND]` (ADR-0242) | none | `agent-context` is a not-included command |
| `eggs` | Hidden client-side REPL dot commands, the Archie first-run banner and the opt-in `RELATE_PROMPT=archie` prompt glyph ([Hidden commands](#hidden-repl-commands)) | `relate-eggs` | The hidden command names are unknown commands; no banner art, and `RELATE_BANNER` / `RELATE_PROMPT` have no effect |
| `extensions` | `relate run <name> [flags]` (ADR-0429): dispatch an installed command-kind extension — the wire host (ADR-0430), the XDG registry, the `run` mount — and the dependency-free lifecycle half, `relate extension init` (the language scaffolds), `relate extension list`, `relate extension remove` and `relate extension check <dir>`, with the provenance marker (ADR-0431). The first-party importers `fs-to-graph` and `git-to-graph` are extension packages now (ADR-0431 §11); the `lab` command retired with them (ADR-0431 §12: no stub, nothing recognizes `lab` after) | `relate-wire` (the extension group's shared kit, ADR-0431 §11) | `run` and `extension` are not-included commands |
| `extension-install` | `relate extension install <name> [--github REPO@TAG\|@SHA] [--url URL] [--from DIR] [--version V] [--force] [--re-approve \| --approve SHA]` and `extension list --available` — the signed channel index fetch and SSHSIG verification (the one release signing root `upgrade` verifies against), the GitHub pin resolution (ADR-0449), the tarball SHA256 check, the probe-then-fallback download route (ADR-0358), and the closed-contract tarball unpacker (ADR-0431). Implies `extensions` | `ureq`, `sha2`, `ssh-key`, `flate2` | `extension install` and `extension list --available` parse in every build (ADR-0137) and answer with the not-included error, exit 2, in this one; `init`, `list`, `remove` and `check` keep working (the network dependencies gate the install verbs only) |

| Bundle | Features | Intended use |
| --- | --- | --- |
| `full` (**default**) | `repl`, `pull`, `push`, `skill`, `upgrade`, `telemetry`, `fetch-params`, `remote-database`, `parquet`, `extensions`, `extension-install`, `info`, `http`, `mcp`, `bolt`, `agent-context`, `eggs` | Release binary (GitHub releases, install script, Homebrew); developer workstations |
| `agent` | `skill`, `pull`, `push`, `fetch-params`, `remote-database`, `info`, `agent-context`, `extensions`, `extension-install` | Sandbox/Docker images for AI agents: one-shot/stdin queries, JSON output, remote/file JSON sources via `--fetch-params`, a database loaded from a URL via `--load` (ADR-0500), the `run` extension mount, no self-update, no telemetry; JSON snapshots only; the extension channel and install verbs ride along (ADR-0431 §3: `full` and `agent` carry both) |
| `minimal` | *(none)* | Smallest query-only shell: one-shot, stdin, and plain interactive input; JSON snapshots only |
| `serve` | `serve` | The bare compile gate (ADR-0232): `serve` with every surface flag an unknown flag — a usage error at run, the smallest serve-shaped build; the empty-`SURFACE_FLAGS` shape is a CI-tested lane (LABS-628) |

Release artifacts are `full` only. Build another bundle from source by
disabling default features:

```bash
cargo build -p relate-cli --no-default-features --features agent
cargo build -p relate-cli --no-default-features --features minimal
cargo build -p relate-cli --no-default-features --features serve
```

Add single features on top of a bundle, e.g. `--features minimal,parquet`
for a query-only shell that keeps the Parquet codec.

**Base dependencies.** `relate-catalog` is non-optional in every bundle:
the ungated `.info` store block renders the manifest's index/constraint
catalog types through the same store-info projection as `relate info`
(ADR-0165), so the `info` feature gates no optional crates.

**Moving databases between builds.** A database created by a build with
`parquet` (every release binary) uses the Parquet codec by default; once it
has a Parquet checkpoint, a build without `parquet` cannot open it. The open
fails closed (exit 1) with the error above instead of converting the store. To share a database across builds, create it
with `--snapshot-format json`, which every build reads and writes.

**Measured size.** Unique crates in
`cargo tree -p relate-cli --no-default-features --features <F> -e normal --prefix none`
(`(*)` markers stripped, deduplicated on name + version) and the size of
`target/release/relate` from `cargo build --release -p relate-cli`.
Measured on Darwin arm64 (macOS, Rust 1.88.0) with the workspace
`[profile.release]` recipe (`opt-level = 3`, `lto = "thin"`,
`codegen-units = 1`, `strip = "debuginfo"`). The table's bytes were
measured under `strip = "symbols"`, which additionally drops the symbol
table; current binaries keep it (symbol-level stripping broke macOS dylibs
until the toolchain carries the LLVM 23 fix) and run slightly larger than
this table:

| Features | Normal crates | Release binary (bytes) |
| --- | --- | --- |
| `full` (default) | 204 | 23,375,312 |
| `agent,parquet` (`agent` before ADR-0106) | 176 | 21,791,552 |
| `agent` | 176 | 21,791,552 |
| `minimal,parquet` (`minimal` before ADR-0105) | 125 | 18,597,264 |
| `minimal` | 77 | 12,294,064 |

`agent` and `agent,parquet` resolve identically: the `agent` bundle's
`pull` command links the engine with its default `parquet` feature through
the load surface, so the explicit `parquet` feature adds nothing. Only
`minimal` truly compiles without Parquet/Arrow (LABS-864 tracks gating
that weight in the other bundles and the FFI cdylib).

**Compiled-out commands.** Every command word (`query`, `skill`, `upgrade`,
`update`, `telemetry`, `pull`, `push`, `run`, `info`, `serve`,
`agent-context`, `extension`) is recognized
as a command in
every build, under the same first-positional rule as
[Lifecycle commands](#lifecycle-commands). `query` is built into every bundle
(ADR-0200). When a feature-gated command's feature is not compiled in, the
command ignores its arguments, prints

```
Error: the '<command>' command is not included in this build of relate
```

to stderr (`<command>` is the word as typed, e.g. `update`), and exits **2**
without opening a database. So `relate pull ...` never silently opens a
database named `pull` in a smaller build. A store of that name is
`relate --database pull` or `relate --database ./pull`. The one
action-level case: a build with `extensions` but without `extension-install`
answers `relate extension install ...` with

```
Error: the 'extension install' action is not included in this build of relate (enable the "extension-install" feature)
```

and exit 2, while `extension init`, `extension list`, `extension remove` and
`extension check` keep working (ADR-0431 §3).

## Lifecycle commands

Exit-after lifecycle verbs are **positional commands**, recognized only as the
first argument (ADR-0069). `clone` is built into every bundle.
The others are provided by the Cargo feature in the **Feature** column
([Build bundles](#build-bundles)). The statement-session verb `query` is
documented under [`query` command](#query-command); it is listed in
`Commands:` beside these verbs but runs the engine instead of exiting.

| Command | Feature | Default | Meaning |
| --- | --- | --- | --- |
| `query [STATEMENT] [--database PATH]` | built-in | `:memory:` | Run a statement session (ADR-0200). Present in every bundle |
| `clone [--wal-dir DIR] [--to-snapshot-format F] SOURCE DEST` | built-in | source codec | Copy a database into a new store and exit (ADR-0156, ADR-0282). Each side is a native directory, or a `.rl` single-file bundle when its path ends in `.rl` — the full dir/`.rl` matrix. The clone uses the source snapshot codec unless `--to-snapshot-format` transcodes it (ADR-0163). Present in every bundle, including `minimal` |
| `skill install [--agent NAME] [--force]` | `skill` | all detected agents | Install the agent-skill bundle |
| `skill remove [--agent NAME] [--force]` | `skill` | all detected agents | Remove the agent-skill bundle |
| `skill list [--agent NAME]` | `skill` | all detected agents | Print each target agent's install state and path (ADR-0242) |
| `skill check [--agent NAME]` | `skill` | all detected agents | Verify installed bundles match this binary's bundle; exit 1 on drift, 0 when clean (ADR-0242) |
| `agent-context [COMMAND]` | `agent-context` | | Print the compact agent briefing: command index, query-run flags, exit codes; per-command detail with a word (ADR-0242). `update` resolves to `upgrade` |
| `upgrade [--yes] [--check]` | `upgrade` | | Self-update to the latest release listed by the website's `releases.json` (ADR-0250); the download probes the direct release route and falls back to the authenticated GitHub API asset route on a 404 with `GITHUB_TOKEN` set (ADR-0358) |
| `update [--yes] [--check]` | `upgrade` | | Alias of `upgrade` |
| `telemetry on\|off\|status` | `telemetry` | enabled | Manage anonymous usage metrics (ADR-0079) |
| `pull [--from FILE] --query Q [--param N=V] [--allow-plaintext] --database PATH` | `pull` | | Pull a Neo4j/Aura subgraph over HTTP Query API v2 into `--database` (ADR-0080, ADR-0200). `Q` is a Cypher statement, a `;`-separated statement list, a file path, or `-` (stdin); all statements run streamed in one process with one checkpoint (ADR-0158) |
| `push --to CREDS --database PATH [--query Q \| --labels L1,L2 \| --types T1,T2] [--batch-size N] [--nodes-update POLICY] [--rels-update POLICY] [--updated-at NAME[,NAME]] [--rel-mode MODE] [--merge-key LABEL:PROP[,PROP]] [--rel-key TYPE:PROP[,PROP]] [--unkeyed-labels ACTION] [--dry-run] [--json] [--allow-plaintext]` | `push` | `5000`, `add`, `add`, none, `keyed`, none, none, `skip` | Push a scoped subgraph of `--database` into a Neo4j/Aura remote over HTTP Query API v2 (ADR-0401, ADR-0200) — `pull`'s mirror, over the same client and the same credentials file. Merge keys come from the store's uniqueness/node-key constraints; see the flag table below |
| `run <name> [flags]` | `extensions` | `:memory:` | Dispatch an installed command-kind extension (ADR-0429, LABS-552): the host parses the action's argv from its manifest flag table, drives the child's wire batches (ADR-0430) into a host-owned store through the `run_import` lifecycle, prints the child's summary, and REPL-handoffs on a TTY `:memory:` run. See [the `run` command](#run-command) |
| `extension init <name> [--lang sh|py|ts|rust]` | `extensions` | `py` | Scaffold a command-kind extension package into a new directory `<name>/` (ADR-0431 §3): the manifest (mount name, this host's MCP revision, a one-flag table — the sample's `--name` — and the `per-batch` commit policy), the language's minimal MCP client generated inline (the [Transport contract](../extensions/spec.md#the-transport-contract); no published client dependency), a TODO-marked `walk()`, and a README quickstart — installable as it is with `extension install <name> --from <name>/`. An existing directory is a refusal (exit 1); a bad name, a reserved command word, an unknown `--lang` or a missing name is a usage error (exit 2). No store, no network. See the [author guide](../extensions/authoring.md) |
| `info --database PATH [--json]` | `info` | | Print store metadata gathered by a cold read (no open, no lock, no WAL replay; ADR-0165, ADR-0200) and exit |
| `serve --database PATH [--load SOURCE] [--host HOST] [--http PORT] [--mcp PORT|stdio] [--bolt PORT [--bolt-identity STRING]] [--read-only[=FRESHNESS]] [--auth USER:PASSWORD] [--concurrent-logical-workspace-limit SIZE] [--max-traversal-depth N] [--expression-memory-limit SIZE] [--safe | --no-safe] [--logical-graph-size-limit SIZE] [--durability-mode MODE] [--snapshot-format F]` | `http`, `mcp`, `bolt` | loopback `127.0.0.1`, no default ports | Host the network surfaces on one open store until SIGINT/SIGTERM (ADR-0232): `--http PORT` serves the HTTP Query API v2 statement endpoint ([HTTP](../http/spec.md)), `--mcp PORT` the MCP server on its streamable-HTTP transport and `--mcp stdio` its stdio transport ([MCP](../mcp/spec.md); ADR-0375), `--bolt PORT` the Bolt server ([Bolt](../bolt/spec.md); LABS-298) whose HELLO agent string `--bolt-identity` overrides (ADR-0293). `--host HOST` sets the bind host for every surface (ADR-0248; default loopback, a non-loopback host warns loudly — plaintext). `--load SOURCE` (ADR-0500) is the query path's two-axis grammar on the serve open: with the `:memory:` target it serves the ephemeral open — the store loaded into memory, the source released (no bundle lock held while serving, writes refuse, the banner's access-mode line names the mode) — and with a durable `-d PATH` target it creates the store there (refuse-if-exists, clone) before serving it. `--read-only[=FRESHNESS]` (ADR-0306, the ADR-0480 spelling) sets the open's access mode — `snapshot` (the default) serves the state the store had at open beside the process writing it, `live` keeps showing its commits. `--safe | --no-safe` (LABS-680, ADR-0477) turns on the safe-mode shape policy for every served statement. `--max-traversal-depth N` (LABS-610, ADR-0466) is the served store's traversal-depth admission limit with the query session's resolution. `--logical-graph-size-limit SIZE` (ADR-0390, the ADR-0368 follow-up) caps the served store's live graph + index bytes with the query session's flag and its shared resolution (`--logical-graph-size-limit` > `RELATE_LOGICAL_GRAPH_SIZE_LIMIT` > unlimited). `--durability-mode MODE` (ADR-0470) is the served store's commit durability policy (ADR-0077) with the query session's resolution (`--durability-mode` > `RELATE_DURABILITY_MODE` > `full`). `--snapshot-format F` (ADR-0163) is the served store's snapshot format with the query session's grammar — a codec name, the one open every served surface shares; the Parquet codec's checkpoints are wide-column parts (ADR-0553); `None` defers to `RELATE_SNAPSHOT_FORMAT`, then the build default. Every surface flag off is a usage error. The command is compiled with the `serve` feature, which each of the three surface features implies; a flag whose feature is off is an unknown flag |
| `extension install <name> [--github REPO@TAG\|@SHA] [--url URL] [--from DIR] [--version V] [--force] [--re-approve \| --approve SHA]` | `extension-install` | newest listed | Install a command-kind extension package (ADR-0431 §3-§7, ADR-0449). Default source: the signed first-party channel (the `extensions.json` index at the website root beside `releases.json`, SSHSIG-verified against the pinned release signing key, the tarball's SHA256 checked against the signed row). `--github` installs a GitHub release pinned to a commit: an untagged `owner/repo` is a usage error, resolution records the tag's commit and the platform asset's SHA256, and a per-user trust store makes a moved tag or changed asset refuse before any fetch unless `--re-approve`/`--approve SHA` says otherwise (see the [extensions spec](../extensions/spec.md#the-github-pin-tier---github)). `--url` is the explicit third-party trust act (https, or plain http to the loopback interface — no signature exists to check, so the transport is the act's one leg of integrity; any other URL is a usage error before any fetch); `--from DIR` the local-directory one (offline, CI, air-gapped: the directory is the trust act, the posture of `pull --from`; the executable must resolve inside it, an intermediate-directory symlink is a refusal). Install = verify, unpack into a staged sibling (the decompressed stream capped at 512 MiB at the read itself — a compressed bomb refuses before any member parses), run every load refusal (manifest decode, escaping executable, protocol, name collision), write the provenance marker, then rename into place — never a silent fetch, and a failed install leaves no residue |
| `extension list [--available \| --trust]` | `extensions` (`--available`: `extension-install`) | | Print the installed set — name, version, tier, source — from the local registry and provenance markers: offline, dependency-free, never a fetch (ADR-0431 §9). An unmarked package (copied in, no marker) lists `unmarked`. `--available` prints the channel's catalog instead — one row per non-hidden package: name, newest version, protocol, installed; offline it fails closed with exit 1 naming the index URL and the network cause. `--trust` (ADR-0449) prints the recorded GitHub pins instead — name, repo, tag, commit, asset SHA256 — from `$XDG_CONFIG_HOME/relate/extensions-trust.json`; the two flags are mutually exclusive |
| `extension remove <name> [--forget-pin]` | `extensions` | | Remove the installed package (ADR-0431 §3). The provenance marker is required: a foreign or unknown directory is a refusal naming what it found. `--forget-pin` (ADR-0449) also clears the name's recorded GitHub pin; without it the pin outlives the package, so a reinstall is still compared against it. There is no `--force` |
| `extension check <dir>` | `extensions` | none | Run the package directory `<dir>` through the mount's six stages (ADR-0429 §6, ADR-0447) — `manifest`, `executable`, `spawn`, `initialize`, `notifications/initialized`, `tools/list` — printing one line per stage and stopping at the first failure, then one advisory line on the package's recorded GitHub pin: `approved`, `drifted`, or `not from a GitHub source` (ADR-0449; the pin line never changes the exit code). It opens no store (no `--database`; the verb takes no flags), writes nothing, runs no tool and fetches nothing, so it is the pre-install gate. Exit **2** for the package's declaration and contents (stages 1-2), **1** for what it does when run (stages 3-6), **0** when it would mount. See [the extension verbs](../extensions/spec.md#verbs-and-lifecycle) |
Both `skill` actions also accept:

| Flag | Default | Meaning |
| --- | --- | --- |
| `--agent NAME` | all detected agents | Scope the action to one agent (case-insensitive) |
| `--force` | `refuse` | Clobber `<skills dir>/relate` even when the command would otherwise refuse it (install: an unmarked, unrecognizable dir; remove: any unmarked dir) |

`skill install` writes `<skills dir>/relate/.relate-installed`, the
ownership marker that proves the directory was created by relate. The
marker check does not follow symlinks: only a real regular file counts, so a
symlinked marker is never trusted. A destination is replaceable **without**
`--force` when it is any of:

- **missing** — the destination does not exist (`NotFound`), so it is safe to
  create fresh. An unreadable or otherwise-uninspectable destination is **not**
  missing: any metadata error other than `NotFound` is refused without
  `--force` (fail closed, matching ADR-0261's `Foreign` classification);
- **marker-owned** — `<dest>/.relate-installed` is a real regular file; or
- **a recognizable relate bundle** — no marker, but `<dest>/SKILL.md` is a
  regular file whose YAML frontmatter declares `name: relate` **and**
  contains a `version:` line. The version value may be a concrete version or
  the `{{VERSION}}` placeholder; either counts as a machine-written bundle.

Any other existing destination — a user-authored or third-party directory, a
`SKILL.md` declaring `name: relate` with no `version:` line, or a symlinked
`<skills dir>/relate` — is refused: `skill install` exits 1, leaves the
directory untouched, and prints
`Error: skill: refusing to touch <dest>: no .relate-installed marker present (directory was not created by relate); pass --force to override`
to stderr.

`skill list` and `skill check` (ADR-0242) are read-only: `--force` is a usage
error on them. `skill list` prints one line per target agent —
`<name>: <installed|installed (legacy bundle)|not installed|foreign (not
managed by relate)> (<path>)`. `skill check` compares every installed
bundle (Owned or Legacy) against the bundle embedded in the running binary,
`SKILL.md` against its version-stamped form: a missing, differing, or extra
file prints a `missing:`/`differs:`/`extra:` line under a `<name>: drifted`
heading and the command exits 1; a clean bundle prints `<name>: up to date`
and a not-installed or foreign destination is reported but is not drift.
Both accept `--agent NAME` (unknown name exits 2).

`agent-context [COMMAND]` (ADR-0242) prints the compact discovery readout for
agent harnesses, rendered from the same command-descriptor table as `--help`,
so the two cannot drift. With no argument: the binary version, one line per
compiled-in command, the query-run flag names, and the exit-code table. With
a command word (`update` resolves to `upgrade`): that command's synopsis,
flags, and examples. Output is always compact text. An unknown word or extra
argument exits 2 with the `agent-context` usage line and a `--help` hint.

```bash
relate agent-context
relate agent-context query
```

`skill remove` is stricter: with no replacement bundle to fall back on, it
removes only a marker-owned directory, and refuses any unmarked destination with
the same exit 1, untouched directory, and error. `--force` explicitly clobbers
any destination on either command. `--agent NAME` and `--force` may appear in
either order.

`skill install` writes the bundle into a sibling `.relate-stage-*` temp
dir inside the skills dir and then atomically renames it into place, so a
concurrent reader never sees a half-written bundle. Stale `.relate-stage-*`
dirs left by a crashed install are swept at the start of the next install.

`upgrade` reads the latest release from the static `releases.json` published
on the project website
(`https://relatedb.ai/releases.json`, ADR-0250 §1) — the
metadata request never touches the GitHub REST API — then downloads the
platform binary together with the `SHA256SUMS` manifest and its detached
`SHA256SUMS.sig` signature. The `SHA256SUMS` request doubles as the route
probe (ADR-0358): it first tries the direct release-download route
`https://github.com/relatedb/relate/releases/download/{tag}/{asset}`
(`{tag}` is the document's exact tag, leading `v` included; `{asset}` is an
exact name from the release's `assets` list, which must carry the platform
binary, `SHA256SUMS`, and `SHA256SUMS.sig` or the command fails closed). A 2xx
probe serves all three assets from that route and makes no GitHub API request
at all — the tokenless path once the repository is public. A **404** on the
probe with `GITHUB_TOKEN` set prints one `note: …` stderr line and falls back
to the authenticated GitHub API asset route: the release-tag lookup
`GET https://api.github.com/repos/relatedb/relate/releases/tags/{tag}`
resolves the three asset ids, then each asset is downloaded from
`GET https://api.github.com/repos/relatedb/relate/releases/assets/{id}`
with `Accept: application/octet-stream` and the bearer token. A 404 on the
probe **without** a token aborts with one `Error: upgrade: …` line that keeps
the `GET {url}: http status: 404` text and names `GITHUB_TOKEN` plus the
documented reinstall one-liner
(exit 1, nothing downloaded, nothing modified); any other probe failure —
403, 5xx, a network or read error — aborts exactly as the direct route always
did, because only the typed 404 status triggers the fallback. Both route bases
are overridable through the dev/test seams `RELATE_RELEASE_DOWNLOAD_URL`
and `RELATE_GITHUB_API_URL` ([Environment](#environment)). `upgrade`
authenticates the manifest, then checksum-verifies the binary against it and
atomically replaces the running binary (macOS/Linux only; on Windows it errors
and points at the manual `.exe` download). The signature is an OpenSSH SSHSIG
(`ssh-keygen -Y sign`) over the exact `SHA256SUMS` bytes, in the namespace
`relate-release`, made with the Ed25519 release key whose public half is
pinned in the binary; it is never fetched from the release (ADR-0115). The
verification runs **before** the manifest is parsed or any checksum is
compared, and it **fails closed**: a missing `SHA256SUMS.sig` asset, a
malformed or invalid signature, a signature made by the wrong key, a namespace
mismatch, or a tampered `SHA256SUMS` all abort with an `UpgradeError` and exit
1 before the running binary is touched — there is no warning-only or
"signature optional" path. The `ssh-key` crate performs the verification.

The signed bytes carry the release version (ADR-0457): the first line of
`SHA256SUMS` is `# relate-release-version: vX.Y.Z`. After the signature
verifies and **before** the binary download, `upgrade` requires that value to
equal the tag `releases.json` named. A mismatch — including a validly signed
older manifest replayed for a newer release — aborts with an `UpgradeError`
naming both versions (exit 1, nothing downloaded past the manifest, the
running binary untouched). A manifest with no header predates the field; it
is accepted with one `warning: …` stderr line only when the requested version
is at or below **v0.14.0** (the last headerless release), and rejected above
that. The legacy window is fixed, never extended. `install.sh` applies the
same rule against the tag resolved from `releases.json` (ADR-0571).

The four success sites are the `--check` upgrade-available report
(``upgrade available: run `relate upgrade` to install <latest>``), the
`--check` up-to-date report, the plain `upgrade` up-to-date report
(`relate <current> is up to date (latest release: <latest>)`), and the
completed replace. No success site prints anything after its status line,
and no failure path prints a status line.
bundle (ADR-0261). It pre-flights each detected agent's
`<skills dir>/relate` and classifies it:

- **Missing** — the directory is absent (`NotFound`). An unreadable directory
  is **not** Missing: any metadata error other than `NotFound` classifies it
  as Foreign (fail closed).
- **Owned** — `<dir>/.relate-installed` exists as a regular file;
  symlinks are not followed, so a symlinked marker does not count.
- **Legacy** — no marker, but `<dir>/SKILL.md` is a regular file whose YAML
  frontmatter declares `name: relate` **and** contains a `version:` line
  (a v0.7.0-era or hand-copied relate skill). The version value may be a
  concrete version or the `{{VERSION}}` placeholder from a pre-injection build.
- **Foreign** — the directory exists but is neither Owned nor Legacy; this
  includes a symlinked destination, a directory carrying only a symlinked
  marker, and a `SKILL.md` that declares `name: relate` with no `version:`
  line.

**Installed = Owned ∪ Legacy.** The refresh then behaves as follows:

- **Installed (Owned or Legacy)** — auto-updates with **no prompt**: one plain
  `skill install` spawn (no `--force`); the child adopts a recognized Legacy
  bundle on its own, re-validating ownership in the process that replaces the
  directory.
- **Nothing installed (all targets Missing)** — an interactive run asks
  `Install the relate skill? [Y/n] ` (empty input means yes) before
  installing; `--yes` installs without asking; a non-interactive run without
  `--yes` prints `run: relate skill install` and installs nothing.
- **Foreign is never overwritten** — each Foreign target is reported with
  `note: {path} is not a recognizable relate skill (no .relate-installed marker, and SKILL.md does not declare name: relate with a version line); leaving it untouched`
  followed by
  `to replace it with the relate skill bundle, run: relate skill install --force`.
- **Per-agent fallback on a foreign conflict** — when a Foreign target
  coexists with installed or missing agents, the refresh falls back to one
  `relate skill install --agent <name>` per non-foreign agent (no
  `--force`), so a forced clobber never reaches the Foreign dir.

The upgrade exit code stays **0** on every post-replace path: a failed or
skipped skill install warns but never undoes the binary upgrade.
`skill install` adopts a recognizable relate bundle on its own, so the
refresh spawns no `--force` and the user-invoked command shares the same
recognizer; `skill remove` keeps the strict marker refusal above. `--check` is
read-only — it reports the current version against the latest release and
changes nothing on disk:

```bash
relate upgrade --check
```

The document must carry `schema_version` 1 and a `latest` release object whose
`tag` is `v` plus an `X.Y.Z` numeric triple; unknown keys are ignored at every
level, so the website schema can grow without breaking older CLIs. Three
outcomes are defined (ADR-0250 §4):

- **Missing** — any non-2xx response (including 404) or a network failure
  fetching the URL aborts with one `Error: upgrade: …` line on stderr naming
  the URL and pointing at the documented reinstall one-liner, and exit 1.
- **Malformed** — invalid JSON, a missing or mistyped required key, a
  `schema_version` other than 1, a `latest.tag` that is not `vX.Y.Z`, or
  `latest: null` (no stable release published) aborts the same way, before any
  asset request.
- **Stale** — a `latest` that is not newer than the running binary is not an
  error: it prints
  `relate <current> is up to date (latest release: <latest>)` and exits 0.
  There is no downgrade path.

In all three cases nothing is downloaded or modified, and there is no retry,
no on-disk cache, and no API fallback for the metadata request itself — not
even when `GITHUB_TOKEN` is set; the asset-route fallback above applies only
to the asset download, after a valid document was read. The metadata request
never carries an `Authorization` header (ADR-0250 §5).

`extension install`'s flags (ADR-0431 §3, ADR-0449):

| Flag | Default | Meaning |
| --- | --- | --- |
| `--github REPO@TAG\|@SHA` | (none) | The commit-pinned GitHub tier: `REPO` is `owner/repo`, then `@` a release tag or a full 40-hex commit SHA. An untagged repository is a usage error (a moving target), as is any other shape. Resolution (the REST API's commit and release-tag lookups) records the tag's commit and the platform asset's SHA256; the first install approves that pin (also recorded in the per-user trust store), and a moved tag or changed asset refuses before any fetch unless re-approved. Mutually exclusive with `--url` and `--from` |
| `--re-approve` | `false` | With `--github`: re-approve whatever the pin resolves to now, replacing a recorded pin that moved. The new asset must verify against its resolved SHA256 first. Mutually exclusive with `--approve` |
| `--approve SHA` | (none) | With `--github`: re-approve only a resolution whose commit is exactly `SHA` (a full 40-hex commit); any other resolution refuses. Requires `--github` |
| `--url URL` | (none) | The third-party trust act: install the tarball at `URL` — https, or plain http to the loopback interface (no signature exists to check, so the transport is the act's one leg of integrity; any other URL is a usage error before any fetch). Mutually exclusive with `--from` (a usage error names both) |
| `--from DIR` | (none) | The local-directory trust act: install the unpacked package directory `DIR` (`extension.json` plus the executable) — no network, no signature, the posture of `pull --from`. The executable must *resolve* inside `DIR` (an intermediate-directory symlink is a refusal, canonicalize + containment). The source directory is left untouched |
| `--version V` | newest listed | Select channel version `V` exactly (an unlisted version refuses naming the listed ones). A usage error beside `--github`/`--url`/`--from`: the named source already determines the version |
| `--force` | `refuse` | Clobber a foreign target directory — one without the provenance marker — only. Never a name collision, never another tier's marked install |

`extension init`'s flag:

| Flag | Default | Meaning |
| --- | --- | --- |
| `--lang L` | `py` | The template language, a closed vocabulary: `sh`, `py`, `ts` or `rust`. An unknown value is a usage error naming the value and the vocabulary. `sh` inlines the canonical bash kit, `py` and `ts` generate the transport and a minimal client as source, `rust` writes a crate whose `main` is the wire kit's `serve` — none of them needs a published client dependency |

`extension check <dir>` takes **no flags**: the directory is the whole
invocation, and there is no store flag — the verb opens no store, so
`-d`/`--database` is an unexpected argument (exit 2), never a target. A
clean run's closing pin line reads the package's provenance marker and the
per-user trust store, all local: it is a report, not a stage, and it leaves
the exit code alone.

`-h`/`--help` in the tail prints the `extension` page, the pre-scan every
compiled-in command's tail gets.

Every install writes `<package>/.relate-provenance.json` beside the
manifest — the tier (`first-party` / `github-pin` / `third-party` / `local`)
and the source reference (the channel tag, `repo@tag`, the URL, or the
directory path); a `github-pin` marker also carries the resolved pin
(`repo`, `tag`, `commit`, `assetSha256`).
`extension list` reads it; `extension remove` requires it. The trust ladder
(ADR-0431 §7, ADR-0449): a first-party package verifies automatically
against the same signing root `upgrade` verifies (the one pinned Ed25519
SSHSIG key, namespace `relate-release`, never fetched); a `--github`
install records the tag's commit and the platform asset's SHA256 in the
per-user trust store (`$XDG_CONFIG_HOME/relate/extensions-trust.json`,
keyed by extension name) — the first install is the approval, `extension
list --trust` inspects the records, and a moved tag or changed asset refuses
before any fetch unless `--re-approve`/`--approve SHA` re-approves it
(`remove --forget-pin` clears a record; a plain `remove` keeps it); a
`--url` install is the explicit third-party act — the one loud install line
names the package, its version, and the URL it came from, and `extension
list` shows the tier, so a third-party install is never mistaken for a
first-party one; a `--from` install is the explicit local act. Third-party can never shadow a reserved
word or an installed package of any provenance: the name checks refuse
before anything is unpacked into place, naming both sides, and a marked
install of the same name replaces cleanly only within its own tier (reinstall
or version change — there is no `extension upgrade` verb); across tiers an
explicit `remove` is required. The channel index and signature are read from
`https://relatedb.ai/extensions.json` (+ `.sig`, the
same website origin as `releases.json`, no `Authorization` header), overridable
through `RELATE_EXTENSIONS_JSON_URL` ([Environment](#environment));
tarball downloads ride the same probe-then-fallback routes as `upgrade`
(`RELATE_RELEASE_DOWNLOAD_URL`, `RELATE_GITHUB_API_URL`,
`GITHUB_TOKEN`). Exit codes (ADR-0431 §3): flag problems and manifest
problems — a reserved-word package name, an unknown kind, a protocol the host
does not speak, an escaping executable — exit **2**; name-resolution,
environment, network, verification and unpack failures (unknown package,
unlisted version, missing platform row, signature failure, checksum
mismatch, a contract-violating tarball member, a collision with an installed
name, a foreign directory, offline catalog) exit **1**.

The unpack contract (ADR-0431 §2): the tarball is a gzip'd ustar archive
holding exactly the package directory — `extension.json` and the executable,
regular files only, the executable landing with its exec bit from the member
mode. No symlinks, no hardlinks, no device nodes, no pax or GNU extension
members, no duplicate members, no paths that escape the package directory,
and no members the manifest does not account for — each is a loud refusal
naming the member. A failed install leaves no residue: the package is staged
into a `.relate-stage-*` sibling and renamed into place only after every
check passes, and the registry's discovery skips (the next install sweeps)
staged siblings a crash left behind.

```bash
# offline, CI, and air-gapped: a package directory is the trust act
relate extension install git-to-graph --from /pkgs/git-to-graph
# the signed first-party channel (fetch, verify, unpack, check)
relate extension install git-to-graph
# what the channel offers, and what is installed
relate extension list --available
```

`telemetry on|off` persists the `telemetry` key in
`<config>/relate/config.json` (`$XDG_CONFIG_HOME/relate/` when
`$XDG_CONFIG_HOME` is set, non-empty and absolute — a relative or empty
value is treated as unset, as for the Windows bases — otherwise
`~/.config/relate/`, otherwise `%APPDATA%\relate\`, finally
`%USERPROFILE%\.config\relate\` — the Windows bases ADR-0545 §6 adds)
and prints a confirmation with the config path. `telemetry off` is one
ordered transition: the run disarms its own telemetry pipeline before
persisting, so it neither transmits nor records anything of its own,
and it removes any pending spooled events (see
[Usage metrics](#usage-metrics)) — turning telemetry off means no
further transmissions, the pending ones included. `telemetry status`
prints
`<enabled|disabled> (<layer>)` where the layer that decided the effective
state is `default`, `config file`, `RELATE_TELEMETRY`, or
`DO_NOT_TRACK`. Exit codes: 0 on success, 1 when no config directory
resolves or the write fails, and 2 for a bare verb, an unknown action, or
extra arguments — printing the `telemetry` usage line and a `--help` hint, the
hand-rolled parser's usage-error convention
([Help and usage errors](#help-and-usage-errors)):

```bash
relate telemetry status
```

`pull` runs a shape query against Neo4j or Aura over HTTP Query API v2 and
streams the returned nodes and relationships into `DATABASE` in batched
UNWIND materialization (ADR-0155), with one checkpoint at the end
(ADR-0158). `--query` is required: inline Cypher, a `;`-separated statement
list, a file path, or `-` (stdin); every statement must `RETURN` graph
elements, and all statements share one load (a later statement's rel
endpoints resolve through nodes an earlier statement created). `pull`
refuses a non-empty destination (ADR-0158): the identity map is load-scoped
memory, so a re-pull is never a merge. `--from` is a `KEY=VALUE`
env file (`NEO4J_URI`, `NEO4J_USERNAME`, `NEO4J_PASSWORD`, optional
`NEO4J_DATABASE`); omit it to use those process environment variables
(`NEO4J_USER` is an alias of `NEO4J_USERNAME`). `--param` binds source-query
parameters. `NEO4J_URI` must name a Query API origin, never a Bolt endpoint:
`http://` / `https://` URIs are used as-is
(`http://host:7474`, `https://host:7473`), a Bolt `neo4j://host:7687` /
`bolt://host:7687` URI is reinterpreted as `http://host:7474`, and an Aura
`neo4j+s://<id>.databases.neo4j.io` URI as
`https://<id>.databases.neo4j.io`. A transactional Bolt URI for an instance
whose Query API listens on another port — for example the hosted demo's
`:7473` — does not map, so pass the `http(s)` Query API origin instead.

Plaintext Basic auth is refused off-loopback: when the resolved Query API URL
is `http://` to a non-loopback host, `pull` fails before any network call with
an error naming `--allow-plaintext` (ADR-0122). Loopback origins (`localhost`,
`127.0.0.0/8`, `::1`) are allowed silently, and `https://` origins are never
refused. `--allow-plaintext` (default off) opts in and instead prints a
prominent warning naming the plaintext host; the spelling matches
`relate-bolt` (ADR-0109). The flag is also summarized in the
[lifecycle command table](#lifecycle-commands).

`--database` must be a directory or `.rl` bundle — `:memory:` is
rejected. `pull` works with either snapshot codec and does not need the
`parquet` feature; a new store gets the build's default codec (Parquet
with the feature, else JSON). No pull-identity map is persisted (ADR-0158):
the map is load-scoped memory for the pull, Parquet checkpoints write no
`pull-identity/` object and no `pull_identity_path` manifest key — a legacy
key in an old manifest is ignored on read and dropped on the next write —
and JSON checkpoints write no inline `graph.json` `pull_identity` field.
Identity WAL operations replay with their graph commits.

After the data stream completes, `pull` carries the source schema into the
pulled store (ADR-0201): it fetches `SHOW CONSTRAINTS` and `SHOW INDEXES`
from the source and replays each row as a standalone
`CREATE … IF NOT EXISTS` statement. Uniqueness, existence, node/relationship
key, and property-type constraints map to their `REQUIRE … IS UNIQUE` /
`IS NOT NULL` / `IS NODE KEY` / `IS RELATIONSHIP KEY` / `IS TYPED` forms;
`RANGE`, `TEXT`, and `POINT` indexes map to `CREATE [TEXT|POINT] INDEX`.
Constraint-backing indexes (non-null `owningConstraint`), non-`ONLINE`
indexes, and `LOOKUP` indexes are filtered out; `FULLTEXT` and `VECTOR`
indexes and unrecognized constraint kinds are skipped with a stderr note and
counted — schema replay never fails a pull. On success it prints
`pulled N new / M reused nodes, P new / Q reused rels (+ C constraints, I indexes; S schema skipped) to PATH`. Exit codes:
0 on success, 1 on runtime/network/engine failure, 2 for missing
`--query` / `--database`, `:memory:`, or unknown flags — printing the `pull`
usage line and a `--help` hint
([Help and usage errors](#help-and-usage-errors)). `NEO4J_URI` errors never
echo the raw URI: a URI carrying userinfo — an `@` anywhere after the scheme,
including a password containing `/` such as `http://user:p/ss@host:7687` — is
rejected outright with `NEO4J_URI must not contain userinfo; pass credentials
via NEO4J_USERNAME/NEO4J_PASSWORD` (pull credentials come from
`NEO4J_USERNAME`/`NEO4J_PASSWORD`; userinfo is never legitimate). The
missing-scheme, missing-host, and unsupported-scheme
errors are fixed text (the scheme token is never echoed either — everything
before the first `://` parses as the scheme, so a `user:pw@host://x` URI
would carry credentials there), and a parsed host outside the
DNS/IP-literal charset (a real hostname never
contains `%` or `@`, so percent-encoded userinfo such as
`http://user%3Asecretpw%40host` lands in the host slot) is rejected with
`NEO4J_URI has an invalid host` (ADR-0122, validate-don't-sanitize):

```bash
relate pull --from neo4j.env --query "MATCH (n) RETURN n" --database ./mirror
```

Pulled values keep their Cypher type (ADR-0181): `pull` asks the source for
the typed Query API v2 JSON media type, so every value arrives as a
`{"$type": …, "_value": …}` envelope and is stored as the engine type its
tag names:

| Source Cypher type | `$type` tag | Stored as |
| --- | --- | --- |
| `BOOLEAN` | `Boolean` | `BOOLEAN` |
| `INTEGER` | `Integer` | `INTEGER` (`i64`, parsed from the decimal `_value`) |
| `FLOAT` | `Float` | `FLOAT` (`f64`, including `NaN`, `Infinity`, `-Infinity`, and scientific forms) |
| `STRING` | `String` | `STRING` |
| `DATE` | `Date` | `DATE` |
| `TIME` | `Time` | `TIME` (offset kept) |
| `LOCAL TIME` | `LocalTime` | `LOCAL TIME` |
| `LOCAL DATETIME` | `LocalDateTime` | `LOCAL DATETIME` |
| `DATETIME` | `OffsetDateTime` | `DATETIME` (fixed offset, no named zone) |
| `ZONED DATETIME` | `ZonedDateTime` | `ZONED DATETIME` (named zone kept) |
| `DURATION` | `Duration` | `DURATION` |
| `POINT` | `Point` | `POINT` (2D or 3D, SRID kept) |
| `UUID` | `UUID` | `UUID` |
| `VECTOR` | `Vector` | `VECTOR` (declared coordinate kind kept) |
| `LIST<…>` | `List` | `LIST<…>`, each element decoded recursively |
| `MAP` | `Map` | `MAP`, each value decoded recursively (defensive) |
| `NULL` | `Null` | `NULL` (defensive) |
| `NODE` | `Node` | a local node: labels plus a property map decoded by this table |
| `RELATIONSHIP` | `Relationship` | a local relationship: type, endpoints, and a property map decoded by this table |
| `PATH` | `Path` | walked: each node/relationship decoded as above |
| `BYTE ARRAY` | `Base64` | **pull fails** (exit 1): the engine's byte array is statement-scoped and cannot be stored as a property (ADR-0327) |
| (server could not serialize the value) | `Unsupported` | **pull fails** (exit 1) |
| (unknown tag) | any other | **pull fails** (exit 1) |

`Node`, `Relationship`, and `Path` are cell tags — a `--query` must `RETURN`
graph elements, and a property value carrying one is unsupported. `NULL` and
`MAP` are defensive: a Neo4j property cannot carry them, but the decoder maps
them. A property the engine cannot represent (a `BYTE ARRAY`), a value the
server could not serialize (`Unsupported`), an unknown tag, an `_value` the
engine cannot parse, or an untagged cell fails the pull with exit 1 and an
error of the form:

```text
typed wire decode failed: property "blob" on element 4:x:0: unsupported typed value "Base64"
```

The error names the owning element id and property key (ADR-0181); there is
no silent fallback.

A `--fetch-params` example — fetch a JSON API response and bind it as `$value`:

```bash
relate ./so.rl \
  --fetch-params 'value=https://api.stackexchange.com/2.3/questions?tagged=neo4j&site=stackoverflow' \
  -c 'UNWIND $value.items AS q MERGE (:Question {id: q.question_id})'
```

The hosted demo database exposes its Query API at
`https://demo.neo4jlabs.com:7473` (database `companies`), so a try-it pull
needs no Aura instance: set `NEO4J_URI=https://demo.neo4jlabs.com:7473` and
supply the demo credentials in the env file.

## `run` command

`relate run <name> [flags]` (ADR-0429 §1, LABS-552) dispatches an
installed command-kind extension: the host resolves `<name>` through the
extensions registry (`$XDG_CONFIG_HOME/relate/extensions/`, falling back
to `~/.config/relate/extensions/` — a relative or empty
`$XDG_CONFIG_HOME` is treated as unset, so the walk is never resolved
against the working directory and cloning a checkout can never add
commands to the grammar), parses the
action's argv from its manifest flag table, and drives the child's wire
batches (ADR-0430) into a store the host alone owns through the same
import lifecycle — the `:memory:` default, the summary,
the REPL handoff, the final checkpoint. A build without the `extensions`
feature keeps the not-included error.

```bash
relate run greet ./src -d ./graph
```

The action's own flags come from its `extension.json` flag table —
`relate run <name> --help` prints them with their kinds and defaults;
the verb's grammar is the store pair every action shares:

| Input | When |
| --- | --- |
| Positional `<name>` | Required: the installed action's mount name |
| `-d`, `--database PATH` | Store to import into (default `:memory:`). An explicit `--database` exits after the import; without it, a TTY opens the shell on the in-memory graph |
| `--<flag> VALUE` | One per the action's manifest flag table — `string`, `integer`, `boolean` (bare), or `repeatable` (one value per occurrence); each flag's default comes from the table |
| `-h`, `--help` | `run --help` prints the verb's page with the installed-actions listing; `run <name> --help` prints the action's own page |

Every `run` invocation resolves the registry first, and a load refusal
(a malformed manifest, a name collision — ADR-0429 §6) fails the verb
closed: exit 2 with the refusal, the run synopsis, and the help hint —
help pages included, never a partial listing.

An uninstalled `<name>` — any name the registry does not resolve — fails
closed with
the pinned discovery error; discovery lists what is installed, it never
fetches (ADR-0429 §7):

```text
Error: no installed action 'git-to-graph' — first-party extension available: relate extension install git-to-graph
Usage: relate run <name> [flags]
Run 'relate run --help' for more information.
```

A bare `run` is the missing-name usage error naming what is installed —
`run requires an action name (installed: alpha, beta)` — or, in the
shipped zero-installed state, the wording that names the same install
path. Exit codes: **2** usage (an uninstalled name, an unknown flag, a
manifest problem), **1** runtime (a wire or child failure — the typed
host errors of ADR-0430 §5), **0** success. An unknown flag near-miss
suggests from the action's own flag table, and the action-scope synopsis
and hint name the action — all manifest data, rendered sanitized so a
package author cannot forge structure into the host's output.

Telemetry reports command `run` with action `extension` — never the
installed action's name (ADR-0429 §9, ADR-0079's anonymity rule).

The extension lifecycle — `relate extension init | install | list |
remove | check` — is the program's other reserved word (ADR-0431, ADR-0449,
LABS-469): `init` scaffolds a package from a language template (the generated
glue and a TODO-marked `walk()`, the [author guide](../extensions/authoring.md)),
the signed first-party channel over the release machinery, `--github` installs
pinned to a commit with a per-user trust store, `--from DIR` offline installs,
and `--url` as the explicit third-party trust act (the
[install guide](../extensions/install.md) covers the tiers). `init`, `list`,
`remove` and `check` ride the `extensions` feature; `install` and `list --available`
the `extension-install` feature (`full` and `agent`, never `minimal` —
the network dependencies gate the install verbs only). A build without
a feature answers the word with the not-included error (ADR-0104), and a
query run still never fetches (ADR-0429 §7): the network is touched only
by the explicit install verb. The manifest schema, the
wire records and caps, the host execution lifecycle, and the verbs have
their own surface documentation: [extensions](../extensions/spec.md).

## `push` command

`relate push --to CREDS --database PATH …` (ADR-0401) is `pull`'s mirror:
it reads a scoped subgraph of `--database` and pushes it into a
Neo4j/Aura remote over the same HTTP Query API v2 client and the same
credentials file (the "one connection string", LABS-9). Merge keys come from
the local store's uniqueness and node/relationship key constraints
(composite keys included); nodes and relationships are grouped by entity
and sent as batched, parameterized UNWIND statements —
`UNWIND $rows AS row MERGE (n:`Movie` {`title`: row.key.`title`}) SET n += row.props`
— with every value a typed parameter envelope, so a pushed property keeps
its Cypher type (`DATE`, `POINT`, `UUID`, …) on the remote. A label with no
merge key fails closed by default: it is reported and skipped, never
guessed; `--merge-key` supplies one, `--unkeyed-labels create` switches to
one-shot `CREATE` rows (non-idempotent). Nothing in V1 deletes remote
data. The same `NEO4J_URI` validation, plaintext policy, and fixed-text
credential errors as `pull` apply (`--to` accepts the same env file, or a
Query API URI whose `NEO4J_USERNAME`/`NEO4J_PASSWORD`/`NEO4J_DATABASE`
come from the process environment).

Before every write the push pre-reads the remote's current keyed rows for
each entity (one SELECT per label/type, using the same merge keys), so the
run report can say what was created, what was updated, and what was skipped
— and so `--updated-at` can compare timestamps. A pre-read failure fails
the push closed with the remote's own error; a failing write batch fails
the push naming the entity, batch, and row range, and the report lists
exactly which batches landed (exit 1; the partial report still prints).

| Flag | Default | Meaning |
| --- | --- | --- |
| `--to CREDS` | required | Credentials env file (`NEO4J_URI`, `NEO4J_USERNAME`, `NEO4J_PASSWORD`, optional `NEO4J_DATABASE`), or a Query API URI (username/password then from the process environment) |
| `-d`, `--database PATH` | required | Source store directory or `.rl` bundle. `:memory:` is rejected |
| `--query Q` | whole store | Only what the statements `RETURN` (nodes, rels, or paths — the same shape check `pull` applies; a `;`-list, file path, or `-` for stdin). Exactly one of `--query`/`--labels`/`--types`; none pushes the whole store |
| `--labels L1,L2` | whole store | Only nodes with these labels, and the relationships between two in-scope nodes (repeatable; out-of-scope endpoints are counted `dangling`, not written) |
| `--types T1,T2` | whole store | Only relationships of these types, with both their endpoints (repeatable) |
| `--batch-size N` | `5000` | Rows per batched statement |
| `--nodes-update POLICY` | `add` | Node update policy: `add` (`SET +=`; remote-only properties survive), `replace` (`SET =`; they are removed), or `skip` (only missing entities are created). Global form (`add`) or per-label form (`Movie=replace,Person=skip`); a bare global is the default for every label not named |
| `--rels-update POLICY` | `add` | The same policy vocabulary, per relationship type or global |
| `--updated-at NAME[,NAME]` | off | Candidate timestamp properties. When both the local entity and the remote row carry one (first present wins, per entity), a strictly newer remote row is skipped and counted `remote-newer`; a newer-or-equal local row applies (equal applies — idempotent re-push); incomparable pairs apply the scoped policy and increment a warning count. Temporals and numbers compare; everything else is incomparable. Without the flag the scoped update policy decides everything |
| `--rel-mode MODE` | `keyed` | Relationship merge mode: `keyed` (the default — merge on the type's resolved key: a `--rel-key` override, else the type's relationship uniqueness/key constraint properties, falling back to `structural` when neither exists), `structural` (`MERGE (a)-[:T]->(b)` — one relationship of a type between two nodes, forced even when a key exists; legitimate parallel rels of the same type between the same two nodes collapse into one), or `create` (the non-idempotent escape hatch) |
| `--merge-key LABEL:PROP[,PROP]` | constraint-derived | Merge key for one label, overriding the constraint-derived key (repeatable, one label each) |
| `--rel-key TYPE:PROP[,PROP]` | constraint-derived | Merge key for one relationship type, overriding its constraint-derived key or supplying one where no relationship constraint exists (repeatable, one type each; composite keys included) |
| `--unkeyed-labels ACTION` | `skip` | What happens to a label with no merge key: `skip` (reported and counted) or `create` (`CREATE` rows; non-idempotent. Relationship endpoints on such nodes are not key-addressable, so their rels are counted `unkeyed` and skipped) |
| `--dry-run` | off | Print the plan — per-label/type counts, merge keys, statement shapes, batch counts, policies — without touching the remote |
| `--json` | off | Print the run report as one JSON object (`entities` with `created`/`updated`/`skipped` by reason, `mergeKey`, `policy`, `statement`, `batches`, plus `statementsSent`/`rowsSent`/`bytesSent`/`elapsedMs`) instead of the table |
| `--allow-plaintext` | off | The same plaintext Basic-auth opt-in as `pull` (ADR-0122): permit `http://` to a non-loopback host, with a warning |

The report (table by default, `--json` for agents) counts per entity:
created, updated, skipped (`unkeyed` / `policy` / `remote-newer` /
`dangling`) and `--updated-at` warning counts, plus the batch/byte totals.
Idempotent re-push is the contract: running the same push twice lands the
same remote state (under `add`/`skip`, `structural` rels, and `--updated-at`
where applicable). Known limitation, stated by ADR-0401: the report does
not audit remote constraint drift — a local merge key the remote does not
enforce can still merge correctly but permits pre-existing remote
duplicates. Exit codes: 0 success (including a dry run), 1
runtime/remote/engine failure, 2 usage (missing `--to`/`--database`,
`:memory:`, more than one scope selector, unknown flags) — printing the
`push` usage line and a `--help` hint. A `--namespace` flag is rejected in
both positions (push always reads the `neo4j` namespace).

```bash
relate push --to neo4j.env --database ./movies --dry-run
relate push --to neo4j.env --database ./movies
```

A byte-array property cannot be pushed (the engine's byte array is
statement-scoped, ADR-0327) and fails with
`push failed: property "blob": a byte array property …`; graph elements in
property position fail the same way. There is no silent STRING fallback.

A `--from` env file that is group- or world-readable (Unix mode `& 0o077 != 0`)
triggers a non-fatal warning naming the file and suggesting `chmod 600`, but
its credentials still load (ADR-0122).

`clone` copies a database into a new store and exits, covering the full
native-directory / `.rl` bundle matrix (ADR-0156, ADR-0282). The format of
`SOURCE` and `DEST` is inferred from each path: a `.rl` suffix names a
single-file bundle, anything else a native directory — the same
classification `DATABASE` gets — so `clone ./db ./db-copy`,
`clone ./db ./backup.rl`, `clone ./backup.rl ./restored`, and
`clone ./a.rl ./b.rl` all work with no direction or format flag. It opens
the source read-only (the ADR-0136 safe path: stable-cut manifest retry,
bounded reads, fail-closed on corruption — a directory source is never
written or exclusively locked, so it can run beside a writer; a
writer-held `.rl` source fails with a lock error) and publishes the loaded
cut into the destination. The destination must not already exist — a failed
run removes a partially created one — and keeps the source's snapshot codec
in every direction unless `--to-snapshot-format F` names another (ADR-0163):
the destination checkpoint is then transcoded to `F`, and a codec the build
does not include fails (exit 1) before the destination is created.
`--wal-dir DIR` names the source WAL of a directory
source (default `<SOURCE>/wal`); a bundle embeds its WAL, so `--wal-dir`
with a `.rl` source fails (exit 1) with `wal_dir is not supported for .rl
single-file databases (ADR-0074)`. `:memory:` is refused on
both sides. On success clone prints nothing. Exit codes: 0 on success,
1 on runtime/engine failure (including an existing destination), 2 for missing
`SOURCE`/`DEST`, an extra positional, unknown flags, or
an invalid `--to-snapshot-format` (a repeated one is last-wins, like every
other CLI value flag) — printing the `clone` usage line and a `--help` hint
([Help and usage errors](#help-and-usage-errors)).
A query or a pre-verb access flag (`--read-only` before the
verb, ADR-0306/ADR-0480 — a read-only fence must not silently vanish on a command
that writes) is not part of the command (exit 2 with the `clone`
usage line and a `--help` hint; [Help and usage errors](#help-and-usage-errors)):

| Flag | Default | Meaning |
| --- | --- | --- |
| `--wal-dir DIR` | `<SOURCE>/wal` | Source WAL of a directory source; a `.rl` bundle embeds its WAL, so this fails with a bundle source |
| `--to-snapshot-format F` | the source codec | Destination snapshot codec (`json` or `parquet`, when compiled in); transcodes the published checkpoint (ADR-0163) |

```bash
relate clone ./mydata ./my-clone
relate clone ./mydata ./backup.rl
relate clone ./backup.rl ./restored
```

`info` answers "what is in this store?" without opening it (ADR-0165): a
cold, bounded read of the store's metadata through
`relate_storage::store_info_uri` — no `Wal::open`, no shard load, no WAL
replay, no lock acquisition, so it can run beside a live writer. `DATABASE`
is a native directory or a `.rl` bundle; a `file://` URI is accepted and
stripped to its path, exactly as at session open; `:memory:` is rejected.
The default
output is a human key/value table on stdout; `--json` emits one JSON object
on stdout instead. Either way `info` prints the layout (`native directory`
or `.rl bundle`, with the bundle superblock version), the snapshot codec,
the snapshot format version, the checkpoint LSN and commit timestamp, the
total size on disk in bytes, the number of WAL segments past the checkpoint
(a staleness hint: each may hold commits newer than the checkpoint), the
checkpoint-time node/rel counts, and the index/constraint catalog. Counts
are the sum of the manifest's shard row counts — checkpoint-time, never
exact on the cold path — so the human table labels them with the checkpoint
LSN (`nodes (at checkpoint LSN 42): 123`). Indexes print as
`name: :Label(property)`; constraints as
`name: kind node|relationship :Label(prop, …) [:: TYPE] [=> :EnforcedLabel]`
with the parser's lowercase kind strings (`unique`, `exists`, `key`,
`propertyType`, `labelExistence`, `nodeLabelExistence`, `sourceLabel`,
`sourceLabelExistence`, `targetLabel`, `targetLabelExistence`).

| Argument / flag | Default | Meaning |
| --- | --- | --- |
| `--database PATH` | required | Store to inspect: a native directory or a `.rl` bundle (a `file://` URI is accepted); `:memory:` is rejected |
| `--json` | off | Emit one JSON object instead of the key/value table; may be given only once |

The JSON object carries the same fields with snake_case names: `layout`
(`"native_dir"` or `"bundle"`), `superblock_version` (number for a bundle,
`null` for a native directory), `codec`, `format_version`, `checkpoint_lsn`,
`commit_ts`, `total_bytes`, `wal_segments`, `node_count`, `rel_count`,
`indexes` (`[{name, label, property}]`), and `constraints` (`[{name, kind,
label, relationship, properties, property_type, enforced_label}]`; a
constraint kind that carries no enforced label emits `null`). A JSON-codec store keeps
only the codec envelope in `manifest.json` — counts and the catalog live in
`graph.json`, which a cold info read never parses (ADR-0165 Decision 5) — so
for JSON-codec stores `format_version`, `node_count`, `rel_count`,
`indexes`, and `constraints` are `null` in the JSON object and print as the
literal word `unavailable` in the human table (followed by a `note:` line
explaining why). A fresh store before its first checkpoint
(`StoreSnapshot::None`) is a state, not an error: the table prints
`snapshot: none yet (no checkpoint since creation)` and the JSON object
emits `null` for every snapshot-derived field (`codec`, `format_version`,
`checkpoint_lsn`, `commit_ts`, `node_count`, `rel_count`, `indexes`,
`constraints`), while layout, total size and WAL segments still report. No
environment variables affect the command. Exit codes: 0 on success, 1 on
runtime failure (missing database, corrupt `manifest.json`, an unreadable
format version, or a writer-held `.rl` bundle on Windows — mandatory
`LockFileEx` byte-range locks admit no reader but the lock owner, so the
cold bundle read fails there with `storage locked: <path>`, the same typed
lock error a read-only open returns (ADR-0136); Unix locks are advisory, so
the read beside a live writer works there), 2 for a missing or
an extra positional, a missing `--database`, a
duplicated `--json`, an unknown flag, `:memory:`, or a query or pre-verb
access flag (`--read-only`, ADR-0306/ADR-0480) combined with the
command — printing the `info` usage line and a `--help`
hint ([Help and usage errors](#help-and-usage-errors)):

```bash
relate info --database ./mydata
relate info --database ./backup.rl --json
```

`run <name>` dispatches an installed first-party importer as an extension
package (ADR-0431 §11): `fs-to-graph` (a directory tree) and `git-to-graph`
(a checkout's history) mount as `relate run fs-to-graph` /
`relate run git-to-graph`, speak MCP stdio (ADR-0447) to a
store the host owns, and ship from the first-party channel
(`relate extension install <name>`, ADR-0431 — the install verbs ship
with this release; the channel lists a package only after its first release
dispatch). Their full flag tables, storage rules, bounds
and graph schemas are the extension surface's contract:
[docs/api/extensions/spec.md](../extensions/spec.md), and the question
catalog for the git graph is
[`references/run-git-to-graph.md`](../../../surfaces/relate-cli/src/skill/bundle/references/run-git-to-graph.md)
in the CLI skill bundle (`relate skill install`).

```bash
relate run fs-to-graph ./src --database ./code-graph --ignore-patterns target/
relate run git-to-graph . --database ./code-graph --max-commits 500 --since 2026-01-01
```

The `lab` command these importers rode is retired (ADR-0431 §12, the
2026-10-28 user direction): no stub, no compat — `lab` is not a command word
in any build, a bare `relate lab` falls into the path-first form and
fails as a Cypher syntax error, and `relate lab <action> …` fails as the
unexpected-argument usage error, the standard failures of a word the CLI
does not know.

`serve` hosts Relate's network surfaces in-process on one open store
(ADR-0232). It is the inverse of an exit-after verb: it runs until
SIGINT/SIGTERM, then shuts both listeners down with the process and takes
the final checkpoint a persistent, writable store is due (the same policy a
query session applies on exit). At least one surface flag is required —
with every flag off there is nothing to serve, a usage error. Ports carry
no defaults (the flag value is the port), and a taken port (`EADDRINUSE`
and friends) is a loud startup error before any banner prints. Every
surface binds `--host` (default `127.0.0.1`, loopback only — the
DNS-rebinding-safe posture). A non-loopback `--host` is deliberate
plaintext exposure (ADR-0248): `serve` has no TLS, so the server prints a
loud `WARNING` at startup stating that anyone who can reach the ports can
present credentials — front it with TLS or keep the loopback default.

`--read-only[=FRESHNESS]` (ADR-0306, the ADR-0480 spelling) sets the
open's access mode — the same opens as `relate query` (ADR-0136,
ADR-0137). One flag, one freshness value, attached-form only:
`--read-only` and `--read-only=snapshot` open the frozen snapshot the
store had at open; `--read-only=live` (the former `--follow`) tails the
WAL and keeps showing the writer's commits. A space-separated value
(`--read-only live`) is a loud usage error naming the attached spelling —
it never silently becomes the database path — and the flag is once-only
(a repeat, including a second value, is a usage error). A read-only or
live serve holds no writer lease, so on a native directory another
process can write the store while it is served — a live serve keeps
showing the writer's commits as it tails the WAL; a snapshot serve keeps
the state it opened at (a `.rl` bundle is the exception: its shared lock
still blocks a writer). Neither creates a missing store — that open is a
startup failure — and neither takes the final checkpoint on shutdown.
The startup banner names the mode, and the MCP surface does not list
`write_cypher`. `--read-only=live` is refused for `.rl` bundles and
`:memory:` (a startup failure), and a pre-verb `--read-only` — before
the `serve` verb — is a loud usage error naming the post-verb spelling:
it is never silently ignored, because a silently-writable server is the
exact hazard the user meant to fence.

| Argument / flag | Default | Meaning |
| --- | --- | --- |
| `-d`, `--database PATH` | required | The store to serve: a directory, a `.rl` bundle, or `:memory:` |
| `--host HOST` | `127.0.0.1` | Bind host for every served surface (ADR-0248). Any non-loopback value exposes the **plaintext** surfaces to the network and triggers a loud startup `WARNING`; `serve` has no TLS, so this is deliberate risk-taking in the ADR-0109 posture |
| `--http PORT` | unset | HTTP Query API v2 statement endpoint (`POST /db/{databaseName}/query/v2`, plus the public `GET /` discovery document; [HTTP spec](../http/spec.md)) on `HOST:PORT`; compiled in with the `http` feature |
| `--origin ORIGIN` | loopback + `https://browser.neo4j.io` and `https://console.neo4j.io` | Allow a browser Origin on the HTTP surface: an exact origin URL or `*` for any; repeatable, and an explicit list **replaces** the default. The browser guard (403 on every state-changing method — `POST` and the `DELETE` rollback alike, ADR-0455) and the CORS echo/preflight follow it; compiled in with the `http` feature |
| `--tx-idle-timeout-secs N` | `RELATE_TX_IDLE_TIMEOUT_SECS`, else `60` | Seconds an HTTP managed transaction may sit idle before the next registry operation expires it (ADR-0435): the operation answers 404/`42N90` and releases the transaction's snapshot pin. `0` disables expiry. Resolution is flag → environment → default (a blank variable is omitted); a malformed value, or one beyond the range the HTTP surface can render as an expiry deadline, is a loud startup error; compiled in with the `http` feature |
| `--max-open-transactions N` | `RELATE_MAX_OPEN_TRANSACTIONS`, else `128` | Cap on open HTTP managed transactions (ADR-0435): a begin over the cap answers 429/`42N91` before taking a snapshot pin. `0` is unlimited, and setting both it and `--tx-idle-timeout-secs` to `0` on a non-loopback bind warns at startup. Same resolution; compiled in with the `http` feature |
| `--max-connections N` | `RELATE_MAX_CONNECTIONS`, else `128` | Process-wide session cap shared by every listener (ADR-0333, ADR-0451). Over cap is accept-then-close; `0` is the explicit unlimited spelling and warns on a non-loopback bind. Resolution is flag → environment → default; a malformed value is a loud startup error |
| `--ws-max-connections N` | unset (`RELATE_WS_MAX_CONNECTIONS`; omit is unlimited) | Opt-in cap on concurrent Bolt-over-WebSocket sessions (ADR-0459), separate from and additional to `--max-connections`: the permit is taken before the upgrade read and held for the session, and an over-cap upgrade is accepted then closed with no HTTP response. `0`, omit, or an unset/blank variable is unlimited — the default, because bulk data-transfer workloads legitimately run many parallel sessions. Resolution is flag → environment → default; a malformed value is a loud startup error. Compiled in with the `bolt` feature, and rejected as unbound on a run that does not bind `--bolt` |
| `--ws-allow-origin ORIGIN` | unset (loopback plus `https://browser.neo4j.io` and `https://console.neo4j.io` with auth on; loopback only with `--auth none`) | Allow this Origin on Bolt-over-WebSocket upgrades (repeatable; an exact origin URL or `*` for any). An explicit list replaces the auth-dependent default, so a self-hosted Browser served from a non-loopback origin names it here. Values use the shared origin grammar and are stored canonically (ADR-0266, ADR-0459). Compiled in with the `bolt` feature, and rejected as unbound on a run that does not bind `--bolt` |
| `--header-deadline-secs N` | `RELATE_HEADER_DEADLINE_SECS`, else `10` | Absolute deadline for a complete HTTP/MCP request header block (ADR-0451), measured from the connection handler's start. A peer that has not finished its headers by then is dropped, however slowly it trickles; every header read re-arms the socket timeout to the remaining time. `0` disables the deadline; a value above 86400 seconds (one day) is a loud startup error. A run that binds neither `--http` nor an MCP listener (`--mcp stdio`, or a bolt-only serve) rejects the flag as unbound rather than silently discarding it. Same resolution and error policy; compiled in with the `http` or `mcp` feature |
| `--body-deadline-secs N` | `RELATE_BODY_DEADLINE_SECS`, else `60` | Absolute deadline for a complete HTTP/MCP request body (ADR-0451), measured from the body read. Every body read re-arms the socket timeout to the remaining time, so a declared `Content-Length` trickled one byte at a time is dropped at the deadline instead of holding a worker. Applies to public and authenticated bodies alike; `0` disables it, and a value above 86400 seconds is a loud startup error. A run that binds neither `--http` nor an MCP listener rejects it as unbound. Compiled in with the `http` or `mcp` feature |
| `--mcp PORT\|stdio` | unset | MCP transport (typed union, ADR-0375): `PORT` serves the MCP server on its streamable-HTTP transport (`POST /` of complete JSON-RPC messages, [MCP spec](../mcp/spec.md)) on `HOST:PORT`; `stdio` runs the stdio transport in the foreground — newline-delimited JSON-RPC on stdin/stdout until the host closes stdin, then the final checkpoint and a clean exit 0 (after every listener surface is spawned). Stdio skips the listener surfaces' stories entirely: no challenge and no token file, and a listener-only flag (`--auth`, `--origin`, `--bolt-identity`, `--ws-allow-origin`, `--ws-max-connections`) on a stdio-only run is a usage error (the serve-posture flags `--host`/`--max-connections` stay accepted). A stdio-only run binds no listener, so the listener warnings never print either — the non-loopback `--host` plaintext WARNING included. The transport choice is once-only — combining the two is a usage error; compiled in with the `mcp` feature |
| `--bolt PORT` | unset | Bolt server (Bolt + Bolt-over-WebSocket, [Bolt spec](../bolt/spec.md)) on `HOST:PORT`; compiled in with the `bolt` feature. Shares the resolved `--auth` credential with the other surfaces; the HTTP discovery document advertises it as `bolt_direct`. WebSocket Origins default to loopback (any port) plus `https://browser.neo4j.io` and `https://console.neo4j.io` with auth on, and loopback only with `--auth none`; a wider Origin needs `--ws-allow-origin` (ADR-0266, ADR-0459) |
| `--bolt-identity STRING` | `RELATE_IDENTITY`, else `Relate/<version>` | HELLO SUCCESS `server` agent string for the Bolt surface (ADR-0293), sent verbatim; compiled in with the `bolt` feature. Set a Neo4j-shaped value (e.g. `Neo4j/5.26.0`) for tooling like cypher-shell that hard-checks the identity — a compatibility shim, not a Neo4j feature-parity claim |
| `--read-only[=FRESHNESS]` | unset | Open the existing store read-only (ADR-0306, the ADR-0480 spelling): no writer lease, never creates it; every write fails. FRESHNESS is `snapshot` (the default: the frozen state at open) or `live` (tail the WAL and follow the writer's commits; refuses `.rl` and `:memory:`). The value is attached-form only, and the flag is once-only |
| `--load SOURCE` | unset | **Load a store from a source into `--database`** (LABS-872, LABS-903, ADR-0500): SOURCE is a local `.rl` bundle, a native directory, or an `http(s)` URL to a `.rl` bundle. With the default `:memory:` target it is the **ephemeral open** — a read-only open that loads the snapshot and replays the WAL as usual, then releases the source: nothing is held open or locked, the source can be deleted or opened writable underneath, writes refuse exactly like `--read-only`'s, the run reports `memory` storage, and a remote source's scratch copy is removed right after the load. With a durable `-d PATH` target it **creates** the store (the path must not exist; the source is cloned into it, then the target opens writable by default). `serve` takes the same grammar on its open. `--read-only=live` with any `--load` is a usage error (nothing is followed); `--read-only` cannot govern a creating load. A URL in `--database` is refused — the target is a local path or `:memory:` |
| `--auth USER:PASSWORD` | `RELATE_AUTH`, else a generated token | One basic-auth credential shared by every served surface (ADR-0232 §1); `none` disables authentication. A present-but-malformed value is a loud startup error |
| `--concurrent-logical-workspace-limit SIZE` | unset (`RELATE_CONCURRENT_LOGICAL_WORKSPACE_LIMIT`; omit leaves admission off) | Shared logical-workspace pool for executing queries (bytes or mb/mib/gb/gib; ADR-0452). A positive value turns admission on. `0` is rejected. Same resolution as the query session's flag: the flag, then the environment variable, then off |
| `--max-traversal-depth N` | unset (`RELATE_MAX_TRAVERSAL_DEPTH`, else 500) | Largest explicit traversal hop bound a served statement may ask for (LABS-610, ADR-0466). Values 1 through 2000; an `*1..N` / `{1,N}` bound above it fails at admission with the syntax error `TraversalDepthExceeded` (Bolt `Neo.ClientError.Statement.SyntaxError`, HTTP 400/`42N06`). Same resolution as the query session's flag |
| `--safe` / `--no-safe` | off (`RELATE_SAFE_MODE=1/0`) | Opt-in safe mode for the served store (LABS-680, ADR-0477): the query session's shape rules, applied at the serve open. A refused statement is a syntax error (`SafeModeViolation`; Bolt `Neo.ClientError.Statement.SyntaxError`, HTTP 400/`42N06`). `--no-safe` negates an enabled environment default |
| `--logical-graph-size-limit SIZE` | `RELATE_LOGICAL_GRAPH_SIZE_LIMIT`, else unlimited | Live graph + index cap for the served store (bytes or mb/mib/gb/gib; ADR-0390) — the query session's storage flag with its shared resolution, applied to the serve open exactly as `relate query` applies it to its own |
| `--expression-memory-limit SIZE` | `256 MiB` (`RELATE_EXPRESSION_MEMORY_LIMIT`) | Per-allocation expression cap for the served store (bytes, mb/mib/gb/gib, or `none`; ADR-0463), the query session's flag with the same resolution |
| `--durability-mode MODE` | `RELATE_DURABILITY_MODE`, else `full` | Commit durability policy for the served store (ADR-0077): `full` barriers the WAL before acknowledging each group commit, `everysecond` acknowledges after apply and barriers about once per second, `snapshot` acknowledges after apply with only completed checkpoints durable. Same resolution as the query session's flag (ADR-0470): the flag, then the environment variable, then `full`. A bad value (flag or variable) is a loud usage/startup error (exit 2), never a silent default |
| `--snapshot-format F` | `RELATE_SNAPSHOT_FORMAT` (a codec name), else the build default | Snapshot format for the served store (ADR-0163), the query session's grammar: a codec name, the one open every served surface shares. The Parquet codec's checkpoints are wide-column parts (ADR-0553) — one typed column per promoted property key beside a residual tagged-JSON tail. Needs the `parquet` feature; a bad value is a loud usage error (exit 2) |

Both surfaces enforce the one resolved credential identically — the
RFC 7617 `Authorization: Basic` header on every request, a missing or
wrong credential answered `401` with a `WWW-Authenticate: Basic` challenge.
The startup banner (stderr) tells the token story once for the server: the
preset source (`--auth` / `RELATE_AUTH`), or the generated token plus
its persisted `<dir>.auth-token` sibling (mode 0600, fresh per start for
`:memory:`) — the same story as the Bolt surface (ADR-0126). Exit codes:
0 on a clean signal shutdown, 1 on startup failures (auth, open, bind),
2 for usage errors (no surface flag, missing `--database`, an unknown
flag, a bad port, a repeated or pre-verb `--read-only` — the
access flags are once-only and post-verb-only, and the pre-verb error
names the post-verb spelling (ADR-0306 §6) — and a query combined with
the command):

```bash
relate serve --database ./mydata --http 7474 --mcp 8000
# all three surfaces on all interfaces — the container posture (ADR-0249);
# expect the loud plaintext WARNING of ADR-0248 on stderr
relate serve --database /data/graph --host 0.0.0.0 --bolt 7687 --http 7474 --mcp 7475
# Bolt with a Neo4j-shaped HELLO identity, admitting cypher-shell (ADR-0293)
relate serve --database ./mydata --bolt 7687 --bolt-identity Neo4j/5.26.0
relate serve --database ./mydata --read-only=live --http 7474
# serve a store another process is writing (ADR-0306/ADR-0480): no writer
# lease, following the writer's commits as they land
relate serve --database ./mydata --http 7474 --tx-idle-timeout-secs 120 --max-open-transactions 64
# bound the HTTP managed-transaction registry (ADR-0435): expire a transaction
# after 120 s idle; a begin beyond the 64-transaction cap is refused with 429/42N91
```

The runnable example is the end-to-end suite
`surfaces/relate-cli/tests/e2e_serve.rs`: it spawns the built binary
with both surfaces on one store, drives a statement through each (raw HTTP
to the query endpoint, an MCP `tools/call` over its HTTP transport),
asserts the shared credential behavior (401 without, success with) on both,
and SIGINTs the server to check the clean shutdown.

A database directory literally named `clone`, `skill`, `upgrade`,
`update`, `telemetry`, `pull`, `info`, or `serve` stays reachable
via a `./` prefix (`relate ./skill`) or after `--`
(`relate -- skill`).

## EXPLAIN and PROFILE output (ADR-0344/0345)

`EXPLAIN <statement>` prints the plan table — and nothing else: the
statement has **no columns and no rows** (ADR-0359; the old single `plan`
column of rendered string lines is gone), so the plan table is the whole
output. The plan text is rendered at this boundary through
`relate-formatter` (`plan_text_lines`), never in the engine.

The `Query:` echo and `Plan:` marker print raw — a statement can be
arbitrarily large, and a box stretched to its width would bury the table —
and every line from the column header onward is the operator table, boxed
with per-column alignment (each `|` lines up) and borders between rows, so
on larger plans the eye cannot slip into the next operator's line:

```
Query: MATCH (p:Person)-[k:KNOWS]->(f:Person) RETURN p.name AS who
Plan:
+------------------+----+----------------------------------+
| Operator         | Id | Details                          |
+------------------+----+----------------------------------+
| + ProduceResults | 0  | 1 item(s)                        |
+------------------+----+----------------------------------+
| | + Expand       | 1  | (p:Person)-[k:KNOWS]->(f:Person) |
+------------------+----+----------------------------------+
```

Each operator names its subject in the `Details` column: a `MATCH` pattern
renders its text (variables, labels, relationship types, quantifier
bounds — `MATCH (:Person)-[:KNOWS*1..2]->(:Person)` shows exactly that),
a node-only pattern is a `NodeByLabelScan` that names its labels
(`AllNodesScan` when it has none), an aggregation (`EagerAggregation`)
names its grouping keys, a `Create`/`Merge` carries its pattern text.
An index-backed start narrates the seek instead (ADR-0486):
`NodeIndexSeek` for an anchored-equality point read on a range-indexed
`(label, property)` pair, `NodeIndexSeekByRange` for a `<`/`<=`/`>`/`>=`
comparison window, the details reading
`RANGE INDEX <var>:<Label>(<prop>) WHERE <prop> <op> <bound>` (the bound
as spelled — `$param` or the literal's text), a multi-element pattern's
text after the seek line (the hops the fused operator still walks); the
`Estimated Rows` column carries the seek's own count (the hit count for
the point read, the posting-length window for the range), and a start no
index serves keeps the fused `NodeByLabelScan`/`Expand` narration with
the label-count estimate. A per-row keyed lookup — an equality predicate
on a range-indexed pair whose bound is a row expression (`m.id = n.id`),
or a label disjunction served as the per-arm union of keyed postings
(`p:Model OR p:Source`, LABS-1083) — narrates `NodeByLabelSeek`, the
details reading `RANGE INDEX <var>:<Label>(<prop>)` (one target per arm,
comma-joined, no `WHERE` clause: the bound is per row) with no count of
its own, the label-count estimate staying.

In the machine modes, an `EXPLAIN` statement emits the **structured plan**
— the tree itself, in Neo4j's structured-plan field shape, the
agent-native form — and nothing else: no columns, no rows. The root's
`args` additionally carry the Neo4j-facing args (ADR-0357):
`string-representation` — the plan text this boundary rendered through
`relate-formatter` (ADR-0359), headed by the
`Cypher`/`Planner`/`Runtime` lines tooling parses and showing the same
boxed operator table as `-mode table` — `version`, the `planner*`/`runtime*`
identity args (named honestly `relate`) and — on a `PROFILE` tree —
`Memory`; child nodes keep the engine's own args.

`-mode json` emits the object form: one `{"plan": ..., "notifications": [...]}`
object for an `EXPLAIN` statement, `{"data": [...], "plan": ...,
"notifications": [...]}` for a `PROFILE` statement (the wrapped statement's
own rows beside the plan). `-mode jsonl` emits one line per data row, then
one final plan line (`{"plan": ..., "notifications": [...]}`; an `EXPLAIN`
statement has no rows, so the plan line is the whole output). `-mode
envelope` carries the same `"plan"` and `"notifications"` keys beside
`columns`, `columnTypes`, `rows` and `truncated` — an `EXPLAIN` envelope
has empty `columns`/`rows`. The `notifications` list is the complete
engine-native notification shape (ADR-0363 decision 9; see
[Query notifications](#query-notifications-adr-0363)), unfiltered — the
text modes' plan-warning exclusion is a rendering rule, not a transport
rule. Key order is not significant: the binary serializes through
`serde_json`, which emits object keys in sorted order (`"notifications"`
before `"plan"`), so consumers match by key name, never by position:

```
$ relate --database demo.cypherl -mode json "EXPLAIN MATCH (p:Person)-[k:KNOWS]->(f:Person) RETURN p.name AS who"
{"notifications":[],"plan":{"args":{"Details":"1 item(s)","Id":0,"planner":"relate","planner-impl":"relate","planner-version":"0.12.6","runtime":"relate","runtime-impl":"relate","runtime-version":"0.12.6","string-representation":"Cypher 5\n\nPlanner relate\n\nRuntime relate\n\nRuntime version 0.12.6\n\nQuery: MATCH (p:Person)-[k:KNOWS]->(f:Person) RETURN p.name AS who\nPlan:\n+------------------+----+----------------------------------+\n| Operator         | Id | Details                          |\n+------------------+----+----------------------------------+\n| + ProduceResults | 0  | 1 item(s)                        |\n+------------------+----+----------------------------------+\n| | + Expand       | 1  | (p:Person)-[k:KNOWS]->(f:Person) |\n+------------------+----+----------------------------------+","version":"5"},"children":[{"args":{"Details":"(p:Person)-[k:KNOWS]->(f:Person)","Id":1},"identifiers":[],"operatorType":"Expand"}],"identifiers":[],"operatorType":"ProduceResults"}}
```

```
$ relate --database demo.cypherl -mode envelope "EXPLAIN MATCH (p:Person)-[k:KNOWS]->(f:Person) RETURN p.name AS who"
{"columnTypes":[],"columns":[],"notifications":[],"plan":{"args":{"Details":"1 item(s)","Id":0,"planner":"relate","planner-impl":"relate","planner-version":"0.12.6","runtime":"relate","runtime-impl":"relate","runtime-version":"0.12.6","string-representation":"Cypher 5\n\nPlanner relate\n\nRuntime relate\n\nRuntime version 0.12.6\n\nQuery: MATCH (p:Person)-[k:KNOWS]->(f:Person) RETURN p.name AS who\nPlan:\n+------------------+----+----------------------------------+\n| Operator         | Id | Details                          |\n+------------------+----+----------------------------------+\n| + ProduceResults | 0  | 1 item(s)                        |\n+------------------+----+----------------------------------+\n| | + Expand       | 1  | (p:Person)-[k:KNOWS]->(f:Person) |\n+------------------+----+----------------------------------+","version":"5"},"children":[{"args":{"Details":"(p:Person)-[k:KNOWS]->(f:Person)","Id":1},"identifiers":[],"operatorType":"Expand"}],"identifiers":[],"operatorType":"ProduceResults"},"rows":[],"truncated":false}
```

`-mode toon` emits the terse structural form — one `plan:` entry with the
tree beneath it, then one `notifications:` entry with the complete
notification list, both rendered by toon's nested-value rule (inline
compact JSON); a `PROFILE` statement emits its `rows[...]` block first,
then the `plan:` and `notifications:` blocks:

```
$ relate --database demo.cypherl -mode toon "EXPLAIN MATCH (p:Person)-[k:KNOWS]->(f:Person) RETURN p.name AS who"
plan:
  {"args":{"Details":"1 item(s)","Id":0,"planner":"relate","planner-impl":"relate","planner-version":"0.12.6","runtime":"relate","runtime-impl":"relate","runtime-version":"0.12.6","string-representation":"Cypher 5\n\nPlanner relate\n\nRuntime relate\n\nRuntime version 0.12.6\n\nQuery: MATCH (p:Person)-[k:KNOWS]->(f:Person) RETURN p.name AS who\nPlan:\n+------------------+----+----------------------------------+\n| Operator         | Id | Details                          |\n+------------------+----+----------------------------------+\n| + ProduceResults | 0  | 1 item(s)                        |\n+------------------+----+----------------------------------+\n| | + Expand       | 1  | (p:Person)-[k:KNOWS]->(f:Person) |\n+------------------+----+----------------------------------+","version":"5"},"children":[{"args":{"Details":"(p:Person)-[k:KNOWS]->(f:Person)","Id":1},"identifiers":[],"operatorType":"Expand"}],"identifiers":[],"operatorType":"ProduceResults"}
notifications:
  []
```

In `-mode table`, the human-readable presentation, the data table (a
`PROFILE` statement's own rows) stays on **stdout** — pipeable, `jq`-clean —
and the plan table prints on **stderr**, where the terminal shows it and a
pipe does not; `EXPLAIN` has no data table, so its plan table is the whole
stderr output. The notification block follows the plan table on stderr
(ADR-0363 decision 8). `-mode markdown` prints the data rows on stdout and
the complete notification list after them (it has no plan table);
`-mode csv` and `-mode cypher` print the rows only. The machine modes carry
the plan and the notifications inside their own payload.


The `EXPLAIN`/`PROFILE` preamble is stripped from the `Query:` echo; a
`CYPHER <version>` preamble stays. `| +` glyphs nest each clause beneath
the one it feeds (`ProduceResults` outermost); `Union`/`UnionAll` carry
multi-part pipelines as children, `Next` carries chained `NEXT` stages.
The `Estimated Rows` column appears on `EXPLAIN` and `PROFILE` alike
whenever any operator carries an estimate — filled at tree construction
from the store's cardinality hints (ADR-0218): a single-label scan's
label row count, a single typed relationship's type count; a node
without a covering signal renders the shared unavailable spelling `-`,
never a fabricated number. Actual-metric columns appear only on `PROFILE`
trees: `Rows`, `Time (ms)`, `Total (ms)`, `Db Hits`, the gated
`Nodes`/`Rels`/`Degree` kind columns, `Memory (Bytes)`, and
`Expression Built (Bytes)`.
A plan where some operator never ran (the starved-operator shape: a scan
that yields no row, so the expansion over it is skipped) also gains an
`Executed` column right after the operator's own cells (LABS-343): `✓`
for an operator that ran, `x` for one that never did — one at-a-glance
run state **alongside** the `(never executed)` marker the metric cells keep: the two
serve different readers and both are deliberate — the per-cell marker is the
signal (a starved operator's cells would otherwise show bare zeros, which read
as real data), while the column summarizes the whole tree in one place. Both
derive from the one `never_executed_node` predicate, never two policies.
An all-executed plan gains no such column.
`Db Hits` is the operator's store-access count: the hop expansions and
neighbor walks its clause performed **plus the node-store reads a scan or
index seek made** — every node a scan examines and every index posting
read counts, not only those that match (ADR-0567; LABS-375 — before the node
reads were counted, `MATCH (n)` reported 0); The kinds behind that total render as their own columns in plain words
(ADR-0397): **`Nodes`** (node-record reads — a scan, label-match or
index-seek candidate each), **`Rels`** (relationships followed to their
other endpoint), **`Degree`** (adjacency entries enumerated — "who is
this node connected to?", one per neighbor a walk read), between
`Db Hits` and `Memory (Bytes)`. A kind column renders only when some
operator in the tree reported a nonzero count of that kind (a plan with
no pattern walk gains no `Rels`/`Degree` columns), and a zero renders
**empty** —
a measured absence as visual absence, so the nonzero counts stand out
vertically; the shared `-` spelling stays reserved for "not produced".
The machine modes keep the three named fields in full
(`Hops`/`NeighborWalks`/`NodeReads` in the wire args, `hops`/`walks`/
`node_reads` in the plan document). On a walk the operator's
`Details` cell also names the chosen start: `[start #k, backward|forward,
bound-pivot|cheapest|seek-anchored|index-0]` (LABS-369) — a displaced
start (an index seek that became a backward traversal) names itself
instead of showing up only as a large `Db Hits`; a clause whose interior
ran the ADR-0468 batch scan/filter/project chain marks itself
`[batched]` (LABS-359) the same way, and its filter's zone skips —
conjunct-batch evaluations the chunk's derived per-column min/max zone
provably replaced with the correct empty survivor set (LABS-645) — mark
themselves `[zone-skips N]` when nonzero; a clause the frontier-NFA QPP
kernel served (ADR-0478 — a quantified-path shape with a pair-shaped
answer contract) marks itself
`[frontier edges=N levels=L endpoints=P]`, its BFS run counters;
`Total (ms)` is the cumulative form of `Time (ms)` (cumulative vs
self): the operator's own elapsed time **plus every descendant's** — the
cost of the sub-pipeline it heads, the way Postgres' `EXPLAIN ANALYZE`
reports a node's subtree total. A leaf operator's total equals its own
time; the root's total is the whole pipeline's cost. It renders on every
metrics-carrying node, derived at the table layout through the one
subtree fold in the model's plan vocabulary — the engine's producer does
not stamp it, exactly like the `Est/Actual` ratio.
`Memory (Bytes)` is the row-materialization bytes, the summed logical size
(ADR-0042) of the rows the operator produced — a streaming operator that
materializes no rows (a node scan feeding a sink) reports 0, which is the
metric's meaning, not a missing stamp; stateful operators' non-row state
(aggregation tables, sort buffers) is a follow-up.
`Expression Built (Bytes)` is **construction-work telemetry**, not memory
usage (ADR-0463): the total size of the values the operator's clause built
(`range`, concatenation, a comprehension output, a reducer accumulator,
`collect`, `UNWIND`). It is a cumulative per-clause figure — values are
dropped as the statement runs, so peak live memory is lower — and it is
never an enforcement total. The enforcement itself is per allocation: one
built value over the `--expression-memory-limit` cap (default 256 MiB) is
refused. It is reported separately from `Memory (Bytes)` so a reader can
tell a row-materializing operator from one that built values. Neither page-cache columns nor other Neo4j cache columns
exist: the engine is in-memory (ADR-0344). On `PROFILE` trees that carry
estimates, an `Est/Actual` column renders the estimated-vs-actual ratio
per operator (`25.0x`), flagged with `!` when the estimate overshoots the
actual rows by 10x or more; an operator without both facts renders `-`.
An operator whose input clause produced no rows and that produced none
itself (never executed) renders `(never executed)` in its metric cells —
never zeros; the tree's root always renders its actual numbers.

A `Warnings` column appears when any operator carries a typed plan
warning, and a `Warnings:` footer line after the table summarizes every
one of them by the affected operator's id. Three warnings exist, each
detected at plan-tree construction (`EXPLAIN` and `PROFILE` alike unless
noted):

| Warning | Fires on | Message |
| --- | --- | --- |
| `CartesianProduct` | a multi-pattern `MATCH` whose patterns share no join anchor (no node variable appears in more than one pattern) | `the MATCH patterns share no join anchor` |
| `AllNodesScan` | a label-less scan (`MATCH (n)`) while the store carries a property index — the message names the existing index as the suggestion | `the scan matches every node; consider the property index (:Label(property))` |
| `LabelDisjunctionScan` | a folded label disjunction no seek serves (LABS-1083) — the scan answers per disjunct label, never every node; the message names the predicate's own per-label `:Label(property)` pairs as the suggestion, never an unrelated store index | `the disjunction scan answers per label; consider the property index (:Model(id), :Source(id))` |
| `EstimationMiss` | `PROFILE` only: an operator whose estimate overshoots its actual rows by 10x or more — the same policy that flags the `Est/Actual` cell with `!` | `estimated rows 10x or more above the actual rows` |

The warning rides beside the affected operator's row (a node without one
renders `-` in the column) and, in the machine modes, the structured
plan's nodes carry a `warnings` list of the warning names — `EstimationMiss`,
`CartesianProduct`, `AllNodesScan`, `LabelDisjunctionScan` — present only on the nodes that carry
one.

### Query notifications (ADR-0363)

`EXPLAIN` and `PROFILE` also derive one flat notification list beside the
plan (`QueryResult::notifications()`): the three plan-warning kinds above plus
every statement- and operator-scoped diagnostic — an unbounded
variable-length pattern, a missing range index, a label/relationship
type/property key the catalog does not know, a deprecated function or
procedure, a scope-less `CALL { … }`. Each entry carries a stable `code`, a
GQLSTATUS (`gql_status`), a severity (`INFORMATION` or `WARNING`), a
category (`PERFORMANCE`, `UNRECOGNIZED`, `DEPRECATION`, `SCHEMA` or `HINT`), a title, the
instance `description` (the diagnosis) and a runnable `remedy` (the
action). A plain run derives no list.

Rendering per mode:

| Mode | Notifications |
| --- | --- |
| `table` | one `Notifications:` block after the plan table (both on stderr, keeping stdout the pipeable data), one line per entry — `<SEVERITY> <gql_status> <code>: <description> Remedy: <remedy>`, prefixed `Id <id>: ` on an operator-scoped entry. The three plan-warning kinds are excluded: the plan table's `Warnings` column and footer are their only rendering, so a kind is never printed twice (ADR-0363 decision 8). The block is absent when the filtered list is empty. |
| `markdown` | the same block after the rendered rows (markdown never renders the structured plan table), carrying the complete list — the three plan-warning kinds included, because no plan table prints them in this mode. |
| `envelope` | `"notifications": [...]` on every result envelope — always present, an empty array on a plain run — carrying the complete native shape below. |
| `json` | on an `EXPLAIN`/`PROFILE` result, one `{"plan": ..., "notifications": [...]}` object; a plain result stays a bare row array with no list wrapper. |
| `jsonl` | the same plan object as one line for `EXPLAIN`/`PROFILE`; plain results stay one row object per line. |
| `toon` | one `notifications:` entry beside `plan:`, the complete list as inline compact JSON. |
| `csv`, `cypher` | rows only — no notification rendering. |

Each native entry's keys are `code`, `gql_status`, `severity`, `category`,
`title`, `description`, `remedy`, plus `operator_id` on operator-scoped
kinds (the plan node's `Id` column value). Example over a store holding a
`:Person` and a `:City` node:

```
$ relate --database demo.cypherl --mode table "EXPLAIN MATCH (a:Person), (b:City) RETURN a, b"
Query: MATCH (a:Person), (b:City) RETURN a, b
Plan:
...
Warnings: Id 1: the MATCH patterns share no join anchor
```

That plan-warning kind stays in the plan table (column and footer); the
`Notifications:` block appears only for the other kinds, for example:

```
Notifications:
WARNING 01N50 Neo.ClientNotification.Statement.UnknownLabelWarning: The label `Persn` does not exist in this database. Remedy: Check the spelling; the label exists after the first write that creates it, and no index or constraint uses the unknown name.
Id 3: INFORMATION 03C91 Relate.Notification.Statement.MissingIndex: the equality predicate on email filters :Person without a range index on (:Person {email}) Remedy: CREATE INDEX relate_Person__email FOR (n:Person) ON (n.email)
```

The same list in the envelope's native shape:

```json
{"code":"Neo.ClientNotification.Statement.CartesianProduct","gql_status":"03N90","severity":"INFORMATION","category":"PERFORMANCE","title":"Cartesian product","description":"the MATCH patterns share no join anchor","remedy":"Give the patterns a join anchor: reuse a node variable across them, or add a pattern that relates their endpoints; otherwise run them as separate queries.","operator_id":1}
```

`PROFILE <statement>` executes through the normal dispatch and returns
the wrapped statement's own columns and rows unchanged, with the
structured plan tree (`Rows`, `Time (ms)`, `Total (ms)`, `Db Hits`,
the gated `Nodes`/`Rels`/`Degree` kind columns, `Memory (Bytes)` and
`Expression Built (Bytes)` per operator, plus the walk's
`[start …]` on a walking operator's details) stamped beside them
(ADR-0344 phase 2, ADR-0375). The plan
**accompanies** the data, never replacing it: `-mode table` prints the
data table on stdout, then the plan table on stderr — exactly EXPLAIN's
plan table plus the metric columns (and the `Est/Actual` column when any
operator carries an estimate):

```
$ relate --database demo.cypherl -mode table "PROFILE MATCH (p:Person)-[k:KNOWS]->(f:Person) RETURN p.name AS who"
+-------+
| who   |
+-------+
| "Ada" |
+-------+

Query: MATCH (p:Person)-[k:KNOWS]->(f:Person) RETURN p.name AS who
Plan:
+------------------+----+----------------------------------+------+-----------+---------+----------------+
| Operator         | Id | Details                          | Rows | Time (ms) | Total (ms) | Db Hits | Nodes | Rels | Degree | Memory (Bytes) | Expression Built (Bytes) |
+------------------+----+----------------------------------+------+-----------+------------+---------+-------+---------+-----------+----------------+--------------------+
| + ProduceResults | 0  | 1 item(s)                        | 1    | 0.043     | 0.314      | 0       |       |         |           | 39             | 0                  |
+------------------+----+----------------------------------+------+-----------+------------+---------+-------+---------+-----------+----------------+--------------------+
| | + Expand       | 1  | (p:Person)-[k:KNOWS]->(f:Person) | 1    | 0.271     | 0.271      | 3       | 2     | 1       |           | 116            | 0                  |
+------------------+----+----------------------------------+------+-----------+------------+---------+-------+---------+-----------+----------------+--------------------+
```

In the machine modes the structured plan's `args` carry `Rows`, `TimeNs`,
`DbHits`, `MemoryBytes`, `ExpressionBuiltBytes`, `Hops`, `NeighborWalks`,
`NodeReads`, `SeekTimeNs` and `WalkTimeNs` together on every profiled
operator, plus `StartElement`/`StartBackward`/`StartReason` when the
operator walked and `Batched` when it ran the ADR-0468 batch
scan/filter/project chain (LABS-359; the wire `plan`/`profile` value,
ADR-0345, ADR-0375) — and `ZoneSkips` when that chain's filter
zone-skipped, its count of conjunct-batch evaluations the chunk's derived
per-column min/max provably replaced with the correct empty survivor set
(LABS-645; only when nonzero), and the frontier-NFA QPP kernel's run
set when it served the clause (ADR-0478, ADR-0516 — the
`FrontierEdges`/`FrontierLevels`/`FrontierEndpoints` trio plus
`FrontierCells` (the (node, state) pairs the runs created) and the NFA
rung's own facts: `FrontierMemoHits`/`FrontierMemoMisses` (the
re-arrival rule's verdict-table asks), the admission asks by verdict
`FrontierRepresentativeProofs`/`FrontierSearchProven`/
`FrontierSearchDisproven`/`FrontierDeclinedExhausted`, and the
`FrontierBudgetFraction` float — the rule's spent work over its budget
constant, `1.0` a run the budget cut; only when engaged, the set
together) — and `FrontierDeclineReason` (ADR-0516) when the rung ran
and fell through at run time: the typed decline's stable name
(`rearrival-budget`, ADR-0516, or `table-budget`, the sparse tables'
allocation ceiling, ADR-0521), beside the counters of the run that
declined. `SeekTimeNs`/
`WalkTimeNs` are the phase times (LABS-448): the elapsed time the
candidate resolution (`NodeReads`) and the relationship traversal
(`Hops`/`NeighborWalks`) each spent, inclusive of a multi-element
pattern's nested phases — zero for a clause that does no pattern work.

`KernelRung` (LABS-1117) names the traversal rung that actually served
a `MATCH` clause: `chain` (the unrolled fixed-k kernel), `hub-star`
(the hub candidate intersection), `frontier-nfa` (the frontier-NFA
rung), or `walker` (the per-row walker, every gate's decline). The
chain rung emits `KernelHops` beside it; no other rung carries a hop
count, and a non-`MATCH` clause, a `MATCH` a fold answered, or a `MATCH`
the batch scan/filter chain answered (`Batched`) emits no `KernelRung`; a
`SEARCH` `MATCH` emits `walker` (it walks the pattern per row). On every
`MATCH`
operator, EXPLAIN and PROFILE alike also emit `PatternEngine` — the
store's `RELATE_PATTERN_ENGINE` mode (`auto`, `nfa`, or `walk`), the
one static rung fact the plan can state: the rungs themselves are
runtime choices inside an operator, so EXPLAIN names the mode that
governs the gates and PROFILE's `KernelRung` names the executed rung.
The plan table's `Details` cell marks the executed rung
`[kernel chain hops=3]`, `[kernel hub-star]`, `[kernel frontier-nfa]`,
or `[kernel walker]`.

The deprecated `_profile_plan`/`_profile_time`/`_profile_rows`/
`_profile_mutations` columns are **removed** (ADR-0345's sanctioned
phase-2 removal — a breaking change recorded in the CHANGELOG): the
tree strictly exceeds what they approximated, and the mutation
counters remain available on `QueryResult::stats`.

## Speedscope export

`--mode speedscope` (`-mode speedscope`, ADR-0349) exports a **`PROFILE`** statement's
plan tree as one speedscope evented-profile JSON document, loadable at
<https://www.speedscope.app> — one frame per operator (`"{operator} {id}"`,
pre-order) and one `O` (open) plus one `C` (close) event per operator, `at`
values in nanoseconds:

```
$ relate --database demo.cypherl -mode speedscope "PROFILE MATCH (p:Person) RETURN p.name AS who"
{"$schema":"https://www.speedscope.app/file-format-schema.json","exporter":"relate@0.12.4","profiles":[{"endValue":256000,"events":[{"at":0,"frame":0,"type":"O"},{"at":256000,"frame":0,"type":"C"}],"name":"relate PROFILE","startValue":0,"type":"evented","unit":"nanoseconds"}],"shared":{"frames":[{"name":"ProduceResults 0"}]}}
```

The document is the upstream file-format shape
(`$schema` = `https://www.speedscope.app/file-format-schema.json`,
`shared.frames`, one `evented` profile with `startValue`/`endValue` and a
balanced, non-decreasing `events` list, `unit: nanoseconds`).

The event timeline is an approximation, not a recording: the PROFILE tree
carries per-operator **inclusive** times only — no offsets — so each parent's
interval is its own inclusive time and its children are laid out sequentially
within it. When the children's inclusive times sum past the parent's (whole-arm
measurement can double-count shared work), the children's intervals are scaled
down proportionally to fit, so every parent's interval contains its children's
and the event order stays balanced — the property speedscope's importer
requires.

The mode is honest about what it cannot export: a plain query (no plan tree)
or an `EXPLAIN` statement (a tree without PROFILE metrics) is an error —
`speedscope mode requires a PROFILE statement: per-operator times are what it
exports (...)` — on stderr, exit code 1. There is no empty profile.
## Query-run errors

A failing statement, a failed open, or a failed session connect in a query run
(one-shot, piped stdin) prints the same human line on stderr as before and
exits with a code from the closed map below (ADR-0194). The map is shared with
`neo4j-cli`; the codes this CLI does not produce yet are reserved.

| Exit | `code` | When |
| --- | --- | --- |
| `0` | | success, including an empty result |
| `1` | `fatal_error` | runtime failure: store open or I/O error, capacity limit, query execution failure, internal error, failed final checkpoint |
| `2` | `usage_error` | unknown flag, bad or missing value, extra positional ([Help and usage errors](#help-and-usage-errors)) |
| `3` | `not_found` | reserved |
| `4` | `auth_error` | reserved |
| `5` | `conflict` | write-write conflict, or the store is locked by another opener |
| `6` | `validation_error` | the statement is rejected: syntax, semantic, argument or type error (including a write on a `--read-only` database) |
| `7` | `timeout` | a statement exceeded `--timeout` (ADR-0241); the stderr line reads `Error: query timed out after <duration>` |
| `8` | `upstream_error` | reserved |

In `json`, `jsonl` and `envelope` output modes a failure also prints one compact JSON object
on stdout, after any rows already written, so `... | jq` sees the cause:

```json
{"error":{"code":"validation_error","exit_code":6,"kind":"syntax","message":"syntax error: …","retryable":false}}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `code` | string | the stable class name from the table; branch on this |
| `exit_code` | number | the process exit code, equal to the table row |
| `kind` | string | the engine error variant: `syntax`, `semantic`, `argument`, `type`, `execution`, `conflict`, `capacity`, `storage`, `locked`, `io`, `internal` |
| `message` | string | the sanitized text also printed on stderr |
| `retryable` | boolean | `true` only for a write-write `conflict`; a locked store is not retryable until its other opener exits |
| `statement` | number | present only for a multi-statement run: the 1-based index of the failed statement |
| `statements` | number | present with `statement`: how many statements the input held |
| `committed` | number | present with `statement`: how many earlier statements are durable, all that completed, or `0` while an explicit `.begin` transaction is open |
| `column` | number, optional | syntax errors only (ADR-0290): the 1-based character column the error points at, within `source` |
| `source` | string, optional | syntax errors only: the line of the statement the parser echoed, sanitized to one line |
| `line` | number, optional | syntax errors only, when the parser reported a position but no source line: the 1-based line within the statement |

Position fields are absent, never `null` or `0`, when the position is unknown
(for example an error at the end of input), and for every other `kind`. They are
relative to the statement the parser was given, not to the script it came from:
a script is split into statements first, so for the second statement of a
script `source` is that statement's text. `message` is unchanged.

On stderr a syntax error that echoed its source prints the first line as it
always was, then the source and a caret under the offending token on their own
lines, indented two spaces. The source is sanitized like any engine text
(control characters removed), and the caret is placed against the sanitized
text:

```text
syntax error: parse error: unexpected token "RETRN"
  RETRN 1
  ^
```

New fields may be added; existing fields and `code` values are never renamed or
repurposed. Table, CSV, Markdown and Cypher modes print nothing extra on
stdout. A one-shot (`-c`) or piped run stops at its first failing statement and
exits with that statement's code.

The `capacity` kind is the one memory/capacity-limit family: the
`--logical-graph-size-limit` cap, the per-allocation expression cap
(ADR-0463), and the per-record 64 MiB WAL payload cap — a single commit
whose record is too large to journal, its message naming the record and
the cap (ADR-0494). All are `fatal_error`, exit 1, `retryable: false`.

### Did-you-mean command suggestions

When a statement fails to parse and its first word is plausibly a mistyped
command word (ADR-0230) — within a small edit distance of, or a prefix of, a
compiled-in command or its alias — the unchanged parse error stays the
headline and stderr gains a blank line plus one suggestion naming the
canonical command and its one-line synopsis:

```text
syntax error: parse error: unexpected token "qu"
  qu
  ^

did you mean "query"?  -- run a statement session (oneshot, stdin, or shell)
```

The check is strictly a fallback after the failed parse: a statement that
parses runs untouched, however command-like its first token. Syntax errors
only; the JSON error envelope gains no field, and the exit code is unchanged
(**6**). The candidate set is the compiled-in command set (the same table
`--help` renders), including the `update` alias, which reports as `upgrade`.

A usage error raised **after** the misspelled word carries the same line
(ADR-0518): `relate service --bolt 7687` exits on the unknown flag
before the statement ever reaches the engine, so the exit itself appends the
blank line and the `serve` suggestion after `Error: unknown flag: --bolt`
(exit code stays **2**). The same applies to the unexpected-argument exit
(`relate service extra`). When the misspelled word matches a command,
the command suggestion replaces the flag did-you-mean on that exit — the
flag candidate list belongs to the query run, the wrong scope for the
command the user meant. A usage error raised **before** any statement is
pending (`relate --bolt 7687 service`), or a pending statement whose
first word is not a plausible command typo (`relate "RETURN 1" --bolt`),
prints exactly the pre-ADR-0518 output.

### Progress of a multi-statement run

A run of several statements that stops on a failure says how far it got
(ADR-0199), because the statements before it may already have changed the
store. One line follows the unchanged error line on stderr, and the three
fields above join the error object:

```text
syntax error: parse error: unexpected token "RETRN"
  RETRN 2
  ^
statement 2 of 3 failed; 1 completed and committed; 1 not run
{"error":{"code":"validation_error","column":9,"committed":1,"exit_code":6,"kind":"syntax","message":"…","retryable":false,"source":"RETRN 2","statement":2,"statements":3}}
```

Inside an open `.begin` transaction the earlier statements are not durable, and
the line says so: `statement 3 of 4 failed; 2 completed, not committed (open
transaction); 1 not run`, with `"committed":0`. A `.`-command such as `.begin`
or `.timer on` counts toward the numbering, as typed; empty fragments and a
trailing `;` do not. Only a failed Cypher statement reports: a failed
`.`-command (say `.commit` with no open transaction) stops the run with its
usual error and exit code and no progress line or error object.
A single statement, a run that succeeds, and an interactive session print
nothing extra. The report changes nothing about execution: there is still no
`--atomic` or `--continue-on-error`. Usage errors, the lifecycle commands
(`clone`, `pull`, `info`, `skill`, `upgrade`, `telemetry`, `run`,
`extension`) and the interactive shell keep their existing output and 0/1/2
codes.

## Row cap

`--width N` wraps the human `table` output to N display columns (LABS-399):
the widest columns give up width first and their cells wrap over several
lines, so a wide result fits the terminal instead of scrolling sideways.
An explicit `--width` wins; otherwise the terminal is asked (a `TIOCGWINSZ`
probe on stdout), then an exported `COLUMNS` (both floored at 20), and a run
with no answer — a pipe with no `COLUMNS` — keeps the natural width, so its
output stays stable. `markdown` ignores it — a GFM table has no wrapped
cells.

`--max-rows N` limits the rows printed per statement (ADR-0196); `0` is
unlimited. Without the flag or `RELATE_MAX_ROWS`, the cap is **100 rows
when an agent harness is detected and stdout is not a terminal**, and unlimited
otherwise, so terminals, interactive shells and scripts outside a harness see no
change. An explicit value is honored everywhere, including on a terminal.

An agent harness is detected by a non-empty value of any of the
cross-harness generic markers `AI_AGENT`, `AGENT`, the harness variables
`CLAUDECODE`, `CLAUDE_CODE`, `COPILOT_AGENT`, `COPILOT_CLI`,
`COPILOT_AGENT_SESSION_ID`, `REPL_ID`, `GEMINI_CLI`, `CODEX_SANDBOX`,
`CODEX_THREAD_ID`, `OPENCODE`, `AUGMENT_AGENT`, `GOOSE_PROVIDER`,
`CURSOR_AGENT`, or a case-insensitive match of `devin` in `EDITOR`, `kiro`
in `TERM_PROGRAM`, or `.pi/agent` in `PATH` (the list `neo4j-cli` uses).

A statement whose result is cut prints its first `N` rows, then writes one
line to stderr and still exits `0` — truncation is not a failure:

```text
warning: result truncated to 100 rows (the statement returned 250); pass --max-rows 0 for all rows
```

The cut also rides **in-band** (ADR-0403) in every mode that can carry it,
naming both counts, so a capped result never reads as a complete one:

| Mode | The record |
| --- | --- |
| `jsonl` | one keyed terminal line after the rows: `{"rowCap":{"kept":100,"total":250}}` — keyed like the plan line, never confusable with a data row |
| `envelope` | `"truncated": true` plus `"rowCap": {"kept": 100, "total": 250}` ([Result envelope](#result-envelope)) |
| `json` | the rows are wrapped: `{"data": [...], "rowCap": {...}}` (the `PROFILE` wrapper's shape, with the always-present `timings`/`planCache` riders beside it — LABS-1120) |
| `toon` | a terminal `rowCap:` entry with the counts as one inline compact JSON value |
| `markdown` | one footer line after the table: `> truncated: 100 of 250 rows shown` (counts only — the `--max-rows` opt-out stays in the stderr warning, the channel the CLI owns) |
| `csv`, `cypher`, `table` | no in-band record — the strict formats' streams stay pure and the human table keeps stderr; the warning above is their only channel |

Each statement of a multi-statement run is capped and reported on its own.
`.info` catalog output is never capped.

The cap limits what is printed, not what the engine computes: the engine
materializes a statement's full result first. Bound the work with a Cypher
`LIMIT`; bound the output with `--max-rows`.

## Agent mode (ADR-0407)

Agent ergonomics (today: the [row cap](#row-cap)'s default, and the piped
output default — `toon` instead of `jsonl`, ADR-0426) resolve through
one ladder — the explicit `--agent-mode`/`--no-agent-mode` flag, then
`RELATE_AGENT_MODE=1/0`, then detection gated on stdout not being a
terminal. A terminal always keeps the human table; the toon default
applies to piped runs only, and an explicit `--mode jsonl` pins the
machine-line default for any agent pipeline that wants objects. An explicit act is unconditional: `--agent-mode` applies the cap
even on a terminal, and both explicit on-states print one stderr line
naming the trigger (`relate agent mode on: --agent-mode
(--no-agent-mode to turn off)`);
detection never announces — a per-command CLI would repeat a banner on
every one-shot run. An invalid `RELATE_AGENT_MODE` exits 2 before any
statement runs.

## Hidden REPL commands

Builds with the `eggs` feature (`full` only, ADR-0543) carry a small set of
client-side REPL dot commands that are deliberately **not documented
individually**: they appear in no `.help` and no `--help`, and this spec does
not enumerate them. They exist only in the interactive human REPL on a
terminal; in agent mode, a one-shot or piped run, or a build without the
feature, their names are ordinary unknown commands. They never touch the
store's contents unless a command says so on screen. Everything an operator
can configure about them is the environment variables `RELATE_BANNER` and
`RELATE_PROMPT` in [Environment](#environment).

## Result envelope

`--mode envelope` (ADR-0195) prints one compact JSON object per result set, on
one line, so a multi-statement run is JSON Lines of envelopes:

```json
{"columnTypes":["INTEGER","LIST"],"columns":["a","l"],"notifications":[],"rows":[{"a":1,"l":[1,2]}],"timings":{"elapsed_ms":0.5,"parse_ms":0.25,"plan_ms":0.19,"execute_ms":0.06,"lex_parse_ms":0.13,"rewrite_ms":0.06,"lower_ms":0.06,"result_generation_ms":0.03},"planCache":"miss","truncated":false}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `columns` | array of strings | the result columns in order, present even when there are no rows |
| `columnTypes` | array of strings | the engine-stamped per-column type names (ADR-0224), one per `columns` entry in column order, from the 18-name set (`STRING`, `INTEGER`, `FLOAT`, `BOOLEAN`, `NODE`, `RELATIONSHIP`, `PATH`, `LIST`, `MAP`, `POINT`, `DATETIME`, `DATE`, `TIME`, `DURATION`, `VECTOR`, `UUID`, `BINARY`, `ANY`); a column whose first-row value is `Null`, or a result with no rows, stamps `ANY` — the type describes the first row only. `BINARY` marks the engine's statement-scoped byte array (ADR-0327), which the CLI cannot receive through `--param` (JSON) and renders, everywhere, as a list of integers 0–255 |
| `rows` | array of objects | one object per row, rendered exactly as `--json` renders it: values as in `--json`, and keys sorted alphabetically, so read `columns` for the query's column order |
| `truncated` | boolean | `true` when the [row cap](#row-cap) cut the rows, else `false` |
| `rowCap` | object | present only when the [row cap](#row-cap) cut the rows (ADR-0403): `{"kept": N, "total": M}` — the rows printed and the rows the statement returned |
| `notifications` | array of objects | the complete query notification list (ADR-0363 decision 9), always present and `[]` on a plain run; entries carry `code`, `gql_status`, `severity`, `category`, `title`, `description`, `remedy` and `operator_id` on operator-scoped kinds. See [Query notifications](#query-notifications-adr-0363) |
| `plan` | object | present only on an `EXPLAIN`/`PROFILE` result: the structured plan tree ([EXPLAIN and PROFILE output](#explain-and-profile-output-adr-03440345)) |
| `timings` | object | the statement's timing block (LABS-1120), always present: `elapsed_ms` (the ADR-0221 wall-clock total) with its always-on `parse_ms` / `plan_ms` / `execute_ms` split (ADR-0491) and the fine sub-split — `lex_parse_ms` (lexing + AST build), `rewrite_ms` (the CIP-231 resolver pass), `lower_ms` (validation + lowering, fused), all sub-windows of `parse_ms`, and `result_generation_ms` (the result build), a sub-window of `execute_ms`. All non-negative milliseconds; every fine window never exceeds its coarse phase. On a plan-cache hit the fine parse windows are `0` — nothing was parsed — which `planCache` explains |
| `planCache` | string | the plan-cache outcome (LABS-1120): `miss` (no cached plan served — the first run of a template, or a hit whose cached plan was rejected for literal-shape safety), `hit-raw-fastpath` (stage 1: the raw statement text matched), `hit-template` (stage 2: the auto-parameterized template matched), `replan-generation-mismatch` (a hit whose entry re-planned under a fresh stats generation), `bypassed` (the cache was not consulted — a caller parameter map carrying the reserved `__AUTO_*__` prefix, or a statement the normalizer refuses) |

An empty result is `{"columnTypes":["ANY"],"columns":["n"],...,"rows":[],"timings":{...},"planCache":"miss","truncated":false}`,
so "no rows" is distinguishable from "no output". `--json` and `--jsonl`
carry the same timing block for every plain result (LABS-1120): `--json`
wraps the rows as `{"data": [...], "timings": {...}, "planCache": "..."}`
(the `PROFILE` wrapper's shape — a plain run with its result behind it no
longer renders the bare array), and `--jsonl` closes every statement's
stream with one keyed terminal line `{"timings": {...}, "planCache": "..."}`
after any plan and `rowCap` lines — a zero-row statement emits exactly that
line, nothing else. `-mode toon` renders terminal `timings:` and
`planCache:` entries after the rows (the timing object inline, the outcome
as a bare token). The strict text formats (`csv`, `cypher`) and the human
`table` mode carry none of it — the interactive session's `.timer` line
remains the human rendering. An `EXPLAIN`/`PROFILE` result is one
`{"plan": ..., "notifications": [...]}` object in `--json`/`--jsonl`, with
the complete list. A failure prints the [error
envelope](#query-run-errors) as in the other JSON modes. New fields may be
added; existing ones are never renamed.

## Help and usage errors

Top-level `relate --help` (or `-h`) prints usage to stdout and exits 0.
Its layout is:

- the two usage lines `Usage: relate [OPTIONS] [--database PATH] [QUERY]` and
  `       relate <COMMAND> [ARGS]`;
- a `Commands:` section indexing every command compiled into this binary, one
  line each — `query`, `clone`, `info`, `pull`, `push`, `skill`, `upgrade`
  (its line names the `update` alias), `telemetry`, `run` (whose own page
  carries the dynamic installed-actions listing; the top-level page stays
  byte-stable across installs, ADR-0429 §1), `serve`, `agent-context`, and
  `extension`;
- the full query-run option list (every flag under
  [Query-engine flags](#query-engine-flags)); lifecycle-command flags
  (`--agent`, `--to-snapshot-format`, `--allow-plaintext`,
  `--max-content-bytes`, `--include-hidden`, `--from FILE`) do not appear in
  the top-level help — they appear in each command's own help;
- an `Examples:` section with two invocations, each a `# comment` above a
  `relate ...` command line (a plain read as JSON, and a `--read-only`
  exploration with `--mode envelope`; ADR-0200);
- as its last line,
  `Run 'relate <COMMAND> --help' for details on a command.`

`relate <command> --help` (or `-h`, e.g. `relate clone --help`,
`relate skill install -h`) prints that command's synopsis, actions, and
flags to stdout and exits 0, followed by an `Examples:` section with one
invocation of the command (ADR-0200). `relate update --help` prints the
`upgrade` help. A command compiled out of the build is not a help target: it still
prints the not-included error and exits 2
([Build bundles](#build-bundles)).

A parser usage error exits **2** with exactly three logical lines on stderr:
the `Error: <msg>` line, the relevant command's synopsis as a
`Usage: relate …` line, and a hint pointing at that command's help:

```text
Error: clone requires SOURCE and DEST
Usage: relate clone [--wal-dir DIR] [--to-snapshot-format F] SOURCE DEST
Run 'relate clone --help' for more information.
```

A query-run usage error uses the top-level synopsis and the top-level help
hint:

```text
Error: unknown flag: --bogus
Usage: relate [OPTIONS] [--database PATH] [QUERY]
Run 'relate --help' for more information.
```

When an unknown flag is a close match for a real one of the same scope — the
query run or the command (dashes, underscores and case ignored, small edit
distance; ADR-0192) — a `Did you mean '<flag>'?` line follows the `Error:`
line. No close match, no line; a rejected positional or subcommand word never
gets one:

```text
Error: unknown flag: --readonly
Did you mean '--read-only'?
Usage: relate [OPTIONS] [--database PATH] [QUERY]
Run 'relate --help' for more information.
```

```text
Error: unexpected argument to clone: --wal-dri
Did you mean '--wal-dir'?
Usage: relate clone [--wal-dir DIR] [--to-snapshot-format F] SOURCE DEST
Run 'relate clone --help' for more information.
```

Errors that already print no usage are unchanged: an invalid `--mode`,
`--param`, or `--fetch-params` value, `--command` combined with a positional
query, a missing value on a query-run flag, and `skill`'s unknown-agent error
(two stderr lines — `Error: skill: unknown agent: '<name>'` and
`valid agents: …` — with no usage line or `--help` hint, though it also
exits 2).

Per-command help emits the same usage metrics as top-level help:
`RELATE-CLI_STARTUP` with kind `help` plus `RELATE-CLI_HELP`, with no
schema change ([Usage metrics](#usage-metrics)).

See the [executable help example](examples/help.md).

## Environment

| Variable | Used by | Meaning |
| --- | --- | --- |
| `GITHUB_TOKEN` | `upgrade`, `extension install` | Optional bearer token for the release-asset downloads (ADR-0068/0069): sent as `Authorization: Bearer $GITHUB_TOKEN` on the direct release-asset requests, and — when the direct probe answers 404 — it is what selects the authenticated GitHub API asset route (ADR-0358 §3). Never sent on the `releases.json` metadata request, nor on the `extensions.json` channel-index or signature requests. While the repository is internal the direct route 404s, so the token is required; once the repository is public the direct route serves tokenless upgrades and the token is unnecessary |
| `RELATE_RELEASES_JSON_URL` | `upgrade`, `install.sh` | Override the release-metadata URL (dev/test); the default is the canonical website URL `https://relatedb.ai/releases.json` (ADR-0250 §1). An empty value means the default, and the named URL is the only source consulted — there is no fallback. `install.sh` reads the same seam at install time to resolve the release it installs (ADR-0571) |
| `RELATE_EXTENSIONS_JSON_URL` | `extension install`, `extension list --available` | Override the extension channel-index URL (dev/test; ADR-0431's consequences — the ADR-0250 §1 pattern); the default is `https://relatedb.ai/extensions.json`, and the signature URL derives from it (`<index>.sig`). An empty value means the default; the named URL is the only source consulted — there is no fallback, and no token is ever sent |
| `RELATE_RELEASE_DOWNLOAD_URL` | `upgrade`, `extension install` | Override the base of the direct release-download route (dev/test seam only); the default is the canonical `https://github.com/relatedb/relate/releases/download` (ADR-0250 §3, ADR-0358 §8). An empty value means the default, and the named base is the only direct-route base consulted — there is no fallback. The extension tarball download rides the same route (ADR-0431 §1) |
| `RELATE_GITHUB_API_URL` | `upgrade`, `extension install` | Override the base of the GitHub API asset route (dev/test seam only); the default is the canonical `https://api.github.com` (ADR-0358 §8). An empty value means the default, and the named base is the only API base consulted — there is no fallback. The extension tarball's API fallback rides the same route (ADR-0431 §1) |
| `RELATE_SNAPSHOT_FORMAT` | `--snapshot-format` | Default snapshot codec for empty URIs |
| `RELATE_CHECKPOINT_INTERVAL_SECS` | `--checkpoint-interval-secs` | Default timed checkpoint period |
| `RELATE_DURABILITY_MODE` | `--durability-mode` | Default commit durability policy (ADR-0077, ADR-0470): `full`, `everysecond`, or `snapshot`; unset or blank is `full`. A value that is not one of the three exits 2 with `Error: invalid RELATE_DURABILITY_MODE: …` (no usage dump) — never a silent default. The flag overrides it. Honored by every open: the query session, `serve`, and library embeddings through `OpenOptions::durability_mode: None`. `--help` and `--version` still win |
| `RELATE_HELP_EXAMPLES` | (none) | `0`, `false`, `off` or `no` (case-insensitive) drops the `Examples:` section from every `--help` page; any other value, or unset, keeps it. Only the examples change: the synopsis, flags, stdout and exit code do not (ADR-0200) |
| `RELATE_MAX_ROWS` | `--max-rows` | Default row cap per statement; `0` is unlimited. A value that is not a non-negative integer exits 2 with `Error: invalid RELATE_MAX_ROWS: …` before any statement runs. The flag overrides it ([Row cap](#row-cap)) |
| `RELATE_AGENT_MODE` | `--agent-mode` / `--no-agent-mode` | The agent-mode override (ADR-0407): `1` turns agent ergonomics on unconditionally, `0` turns them off; anything else exits 2 with `Error: invalid RELATE_AGENT_MODE: …` before any statement runs. The flags override it ([Agent mode](#agent-mode-adr-0407)) |
| `RELATE_BANNER` | (none) | `eggs` builds, interactive REPL only: `1` always prints the Archie banner art, `0` never does; unset or empty prints it once per install, on the first interactive start, tracked by the `archie-banner-seen` marker file in the config directory. Any other value is reported on stderr and ignored. Needs a UTF-8 locale; never printed in agent mode, on a non-terminal stdout, or by a one-shot or piped run. Without `COLORTERM=truecolor`/`24bit`, or under `NO_COLOR` or `TERM=dumb`, the art is plain shade characters |
| `RELATE_PROMPT` | (none) | `eggs` builds, interactive REPL only: `archie` turns the primary prompt into the `∩ ` glyph (the continuation prompt is unchanged). Falls back to `cypher> ` on a non-UTF-8 locale, under `NO_COLOR`, in agent mode, or on a non-terminal stdout. Any other value is reported on stderr and ignored |
| `RELATE_LOGICAL_GRAPH_SIZE_LIMIT` | `--logical-graph-size-limit` | Default live graph + index cap; `0` or unset is unlimited. A value that does not parse exits 2 with `Error: invalid RELATE_LOGICAL_GRAPH_SIZE_LIMIT: …`, the same rejection as a bad `--logical-graph-size-limit` (no usage dump). `--help` and `--version` still win |
| `RELATE_EXPRESSION_MEMORY_LIMIT` | `--expression-memory-limit` | Default per-allocation expression cap (ADR-0463); unset keeps 256 MiB, `none`/`0` disables it. A value that does not parse exits 2 with `Error: invalid RELATE_EXPRESSION_MEMORY_LIMIT: …` |
| `RELATE_CONCURRENT_LOGICAL_WORKSPACE_LIMIT` | `--concurrent-logical-workspace-limit` | Default concurrent logical-workspace pool (ADR-0452). Unset or blank leaves admission off. A positive `SIZE` (bytes or mb/mib/gb/gib) turns admission on. `0` exits 2 with `Error: invalid RELATE_CONCURRENT_LOGICAL_WORKSPACE_LIMIT: must be positive`; a value that does not parse exits 2 with `Error: invalid RELATE_CONCURRENT_LOGICAL_WORKSPACE_LIMIT: …` (no usage dump). The flag overrides it. `--help` and `--version` still win. A set `RELATE_TOKEN_ADMISSION` fails open: that variable is ignored; set this one |
| `RELATE_MAX_TRAVERSAL_DEPTH` | `--max-traversal-depth` | Default traversal-depth admission limit (LABS-610, ADR-0466). Unset or blank uses 500. Values are 1 through 2000; `0`, a value over the ceiling, and a non-integer exit 2 with `Error: invalid RELATE_MAX_TRAVERSAL_DEPTH: …` (no usage dump). The flag overrides it. `--help` and `--version` still win |
| `RELATE_SAFE_MODE` | `--safe` / `--no-safe` | Default safe mode (LABS-680, ADR-0477): `1` turns it on, `0` off; unset or blank is off. Any other value exits 2 with `Error: invalid RELATE_SAFE_MODE: '<v>' is not 1 or 0` (no usage dump). The flags override it, and `--no-safe` beats an enabled environment default. `--help` and `--version` still win |
| `RELATE_LOAD_CSV_DIR` | `--load-csv-dir` | Explicit fallback CSV directory when the flag is absent; neither set means local files are denied (ADR-0081) |
| `RELATE_TELEMETRY` | usage metrics | `0`/`false`/`off` disable, `1`/`true`/`on` enable anonymous usage metrics (ADR-0079); unrecognized values fall through to the config file |
| `DO_NOT_TRACK` | usage metrics | `1`/`true` disable anonymous usage metrics; beats every other layer (ADR-0079) |
| `NEO4J_URI` | `pull` | Source Query API URI when `--from` is omitted (ADR-0080); see the `pull` notes above for accepted schemes |
| `NEO4J_USERNAME` / `NEO4J_USER` | `pull` | Source username when `--from` is omitted |
| `NEO4J_PASSWORD` | `pull` | Source password when `--from` is omitted |
| `NEO4J_DATABASE` | `pull` | Source database name when `--from` is omitted (default `neo4j`) |
| `RELATE_AUTH` | `serve` | Basic-auth credential (`user:password`, or `none`) for every served network surface; `--auth` overrides it, and with neither set a token is generated and persisted (ADR-0232 §1). A present-but-malformed value is a loud startup error |
| `RELATE_MIXPANEL_ENDPOINT` | usage metrics | Override the Mixpanel `/track` POST URL (dev/test); `https://` required for any host, `http://` allowed only for loopback hosts (`localhost`, `127.0.0.1`, `::1`); a rejected value disables telemetry for the run with one stderr warning (ADR-0263) |
| `RELATE_MIXPANEL_TOKEN` | usage metrics | Override the compiled-in Mixpanel project token (dev/test) |
| `RELATE_BULK_BATCH_THRESHOLD` | query engine | Estimated touched-entity count at which an eligible global bulk write (`MATCH (n) DETACH DELETE n`, cross-product `CREATE`, global `SET`/`REMOVE`) switches to batched commits; default `10000` (ADR-0209). Unset defaults; a set value that does not parse as a positive integer fails `Database::open` with a semantic error (same contract as `RELATE_LOGICAL_GRAPH_SIZE_LIMIT`); read once at open |
| `RELATE_BULK_BATCH_BUDGET` | query engine | Estimated touched-entity budget per batch commit of an eligible global bulk write; default `25000` (ADR-0209). Unset defaults; a set value that does not parse as a positive integer fails `Database::open` with a semantic error (same contract as `RELATE_LOGICAL_GRAPH_SIZE_LIMIT`); read once at open |
| `RELATE_PREVIEW` | query engine | Global opt-in for preview Cypher features (ADR-0371): a comma-separated list of kebab-case feature names, whitespace tolerated, read once at open. Unset or empty enables nothing. An unknown name fails `Database::open` (exit 1) with `Error: …: invalid RELATE_PREVIEW value: unknown preview feature '<name>' (known features: …)` naming the known names — fail-closed, never silently ignored. Fourteen features are gated live: `hybrid-search` (CIP-278 hybrid search `COMBINE`, ADR-0379), `local-callables` (CIP-231 `DEFINE PROCEDURE`/`DEFINE FUNCTION`, ADR-0382), `ontology` (the `db.ontology.*` procedure family, ADR-0378), the CIP graph-type trio — `graph-type-opt-props` (CIP-283 optional properties `prop :: ANY`), `graph-type-default-values` (CIP-262 `DEFAULT ON COMMIT <value expression>`) and `graph-type-implied-labels` (CIP-266 implied labels, inheritance) — `obfuscated-literals` (the Neo4j `******` dummy literal, gated at the lexer so the disabled form is the parser's own syntax error — ADR-0454), `mixed-label-syntax` (Neo4j's mixed-label-syntax allowance: colon-conjunction, GQL label symbols and `IS` may mix across one clause's label expressions — ADR-0456), `float16-coordinate-types` (CIP-280 FLOAT16/BFLOAT16 vector coordinate types in `VECTOR<…>` type expressions and the `vector()` constructor — ADR-0464), `chronological-comparisons` (Cypher 25's four chronological operators — `IS [NOT] BEFORE`, `AFTER`, `SAME INSTANT`, `SAME REPRESENTATION` — ADR-0467) and `complex-search-patterns` (widening a SEARCH MATCH statement's pattern shape — ADR-0467), `coll-zip` (CIP-274's `coll.zip`, ADR-0497), `coll-has` (CIP-286's `coll.hasAnyOf`/`coll.hasAllOf`/`coll.hasNoneOf`, ADR-0497), `gql-stddev-aliases` (CIP-245's `stddev_samp`/`stddev_pop` spellings, ADR-0497). The name gates are one of the four gate-location exceptions ADR-0497 records: they gate *name admission*, not syntax, so a gated name invoked while disabled fails with the registry's ordinary unknown-function rejection (`UnknownFunction: <name>`, 42N48) exactly like any unknown name — never the authored `preview feature …` error, which would advertise a name the catalogue must not list. While a name gate is off its names are also absent from `SHOW FUNCTIONS`. With `ontology` off, every `db.ontology.*` call fails with the authored `semantic error: SemanticError: preview feature 'ontology' is not enabled: set RELATE_PREVIEW=ontology`, and `SHOW PROCEDURES` omits the four rows; with it on, the four rows are listed with unchanged signatures and modes and every call runs. Enabling it on a served store — `relate serve` with Bolt, HTTP or MCP — exposes the four procedures, including the store-level writers `db.ontology.set` and `db.ontology.drop`, to every connected client: the gate is the server process's environment, read once at open, not a per-caller check. A gated feature invoked while disabled fails with `Error: …: preview feature '<name>' is not enabled: set RELATE_PREVIEW=<name>` |
| `RELATE_PATTERN_ENGINE` | query engine | Diagnostic pattern-engine switch (ADR-0519): `auto`, `nfa`, or `walk`, read once at open by every session (`query`, `serve`, and library embeddings — there is no flag, the variable is the whole surface). Unset, blank, or `auto` routes exactly as today; `nfa` forces the frontier NFA rung (ADR-0478) onto every frontier-compilable clause (its cost gate is bypassed; every structural, answer-shape and budget proof stands, declines still fail through); `walk` stands the traversal kernels (ADR-0473, ADR-0478) down, so the batch scan-filter chain and the per-row walker serve every clause. A value that is not one of the three fails the open with `Error: …: invalid RELATE_PATTERN_ENGINE: invalid pattern engine "…"; expected auto, nfa, or walk` — never a silent default. Diagnostic-only — a measurement instrument for the pattern-engine spike, not a supported tuning knob — and never Cypher syntax: a statement cannot select its engine. It dies with the old engines it switches between |

## Query-engine flags

| Argument / flag | Default | Meaning |
| --- | --- | --- |
| `-d`, `--database PATH` | `:memory:` | Directory (WAL + snapshot), a `.rl` single-file bundle, or `:memory:` (ADR-0200). `-d` is the short form |
| `QUERY` | unset | One-shot Cypher on the path-first form. Omit it on a terminal and the interactive shell opens (ADR-0231); piped or redirected, the process prints help. A stdin script is `query` with no `STATEMENT` |
| `-c, --command <CYPHER>` | unset | Same as positional `QUERY` (cannot combine with both) |
| `--json` / `--csv` / `--jsonl` / `--markdown` / `--toon` | auto: `table` on a terminal, `jsonl` otherwise (ADR-0107), `toon` when agent mode is in effect ([ADR-0426](#agent-mode-adr-0407)) | Output format shortcuts; an explicit shortcut always overrides the default. `json` renders indented on a terminal and compact (one line) when stdout is piped or redirected (ADR-0240) |
| `--pretty` | off | Force indented `json` output even when stdout is not a terminal (ADR-0240); ignored by the other modes |
| `--mode MODE` | auto: `table` on a terminal, `jsonl` otherwise (ADR-0107), `toon` when agent mode is in effect ([ADR-0426](#agent-mode-adr-0407)) | `table`, `csv`, `json`, `jsonl`, `envelope`, `markdown`, `cypher`, `toon`, `speedscope`; an explicit `--mode` always overrides the default. `envelope` is [the result envelope](#result-envelope); `toon` is the compact TOON tabular form (one `rows[N]{cols}:` header plus one line per row, nested values inline JSON; ADR-0239); `speedscope` exports a `PROFILE` plan tree as one speedscope evented-profile JSON document ([Speedscope export](#speedscope-export), below) |
| `--format MODE` | as `--mode` | Documented alias of `--mode` (the neo4j-cli spelling, ADR-0239). `--output`/`-o` stay errors whose did-you-mean names `--format` |
| `--noheader` | off | Suppress column headers |
| `--fetch-params N=SRC` | unset | Fetch an `http(s)` URL or read a file, parse it as JSON, and bind it as the parameter `N` (repeatable; 64 MiB cap, 60 s timeout). A CLI-process fetch, not an engine capability — no URL allowlist, same trust as piping through `--param` (ADR-0267). Parses in every build; the fetch needs the `fetch-params` feature (in `full` and `agent`, not `minimal`) and fails closed at resolution without it ([Build bundles](#build-bundles); ADR-0270) |
| `--param N=V` | unset | Bind a Cypher parameter (repeatable); `V` is a JSON scalar, array, or map, or a bare word string. A statement that references a parameter no `--param`/`--fetch-params` binds fails with `ParameterMissing: Expected parameter(s): <names>` (exit 6, ADR-0308) |
| `--snapshot-format parquet\|json\|markdown` | parquet with the `parquet` feature, else json (`RELATE_SNAPSHOT_FORMAT`, codec names) | Snapshot codec for an **empty** directory (ADR-0050). `parquet` needs the `parquet` feature (in `full`, not `agent` or `minimal`); without it `--help` lists `(json, markdown)` and `parquet` fails at open ([Build bundles](#build-bundles)). The Parquet codec's checkpoints are wide-column parts (ADR-0553) — one typed column per promoted property key beside a residual tagged-JSON tail, a `format_version` 5 manifest (6 with an ontology); a legacy-encoded store upgrades at its next checkpoint, never the reverse. `markdown` (ADR-0406) makes the directory an Obsidian-compatible vault: the `.md` files with YAML frontmatter are the storage, the engine's state lives in the hidden `.relate/` sidecar; opening an existing vault requires the opt-in (a `.relate/manifest.json` naming the markdown codec also selects it) |
| `--wal-dir PATH` | `<DATABASE>/wal` | Local WAL directory (Native URIs). Rejected for `.rl` bundles |
| `--checkpoint-interval-secs N` | `300` (`RELATE_CHECKPOINT_INTERVAL_SECS`) | Timed checkpoint period; `0` disables. Persistent URIs only |
| `--durability-mode full\|everysecond\|snapshot` | `full` (`RELATE_DURABILITY_MODE`) | Commit durability policy (ADR-0077, ADR-0470). Relaxed modes acknowledge before a WAL barrier |
| `--logical-graph-size-limit SIZE` | unset (`RELATE_LOGICAL_GRAPH_SIZE_LIMIT`; `0` or omit is unlimited) | Live graph + index cap (ADR-0042) |
| `--expression-memory-limit SIZE` | `256 MiB` (`RELATE_EXPRESSION_MEMORY_LIMIT`) | Per-**allocation** expression cap (bytes or mb/mib/gb/gib, or `none`; ADR-0463). One value an expression builds may not exceed it; a long streaming statement with many small builds is not charged a running total. Explicit `none`/`0` disables the cap for known bulk pipelines. The refusal is the plain-language `Error::Capacity` line naming the attempt, the safety intent, and this knob |
| `--concurrent-logical-workspace-limit SIZE` | unset (`RELATE_CONCURRENT_LOGICAL_WORKSPACE_LIMIT`; omit leaves admission off) | Shared logical-workspace pool (bytes or mb/mib/gb/gib; ADR-0452). A positive value turns admission on. `0` is rejected |
| `--max-traversal-depth N` | `500` (`RELATE_MAX_TRAVERSAL_DEPTH`) | Largest explicit traversal hop bound (LABS-610, ADR-0466). Values 1 through 2000; an `*1..N` / `{1,N}` bound above it fails at admission with `TraversalDepthExceeded` (syntax error). A safety limit, not a database limit |
| `--safe` / `--no-safe` | off (`RELATE_SAFE_MODE=1/0`) | Opt-in safe mode (LABS-680, ADR-0477): refuse a new named node without a label, and a `MATCH` read whose final `RETURN`/`WITH` has no `LIMIT` and no reducing aggregate, as `SafeModeViolation` (syntax error, exit 6). Policy, not a database limit |
| `--timeout DURATION` | unset (no bound) | Per-statement wall-clock bound (ADR-0241): bare number = seconds, `ms`/`s`/`m` suffixes, fractions allowed (`1.5s`); `0` disables. A timed-out statement prints `Error: query timed out after <duration>`, exits 7, and carries `code: "timeout"` in the JSON error envelope |
| `--max-rows N` | `100` under a detected agent harness with stdout not a terminal, otherwise unlimited (`RELATE_MAX_ROWS`) | Rows printed per statement; `0` is unlimited. A bad value exits 2 ([Row cap](#row-cap)) |
| `--width N` | the terminal width, else an exported `COLUMNS` (both floored at 20), else the natural width | Wrap the human `table` output to N columns: the widest columns shrink and their cells wrap over several lines (LABS-399). `markdown` ignores it; a run with no terminal answer (a pipe, no `COLUMNS`) keeps the natural width. A bad value or a width under 20 exits 2 |
| `--read-only[=FRESHNESS]` | off | Open an existing database without writing, checkpointing, or migrating (ADR-0136; the ADR-0480 spelling). A native directory can be open beside a writer. A `.rl` file opens only while no writer holds it. `:memory:` rejects writes and creates nothing. FRESHNESS is `snapshot` (the default: the frozen state at open) or `live` (tail a native-directory WAL and reload across a checkpoint gap, the former `--follow`, ADR-0137; refused for `.rl` and `:memory:`). The value is attached-form only (`--read-only live` is a usage error naming the attached spelling, never a swallowed path or query), and the flag is once-only |
| `--load-csv-dir PATH` | denied (`RELATE_LOAD_CSV_DIR` fallback) | Allow local `LOAD CSV` only for regular files under this canonical directory; flag overrides env (ADR-0081). Per-file byte limit is 8 MiB (cannot be raised via CLI) |
| `--namespace neo4j\|system\|system_ontologies` | `neo4j` | Session namespace for the run (ADR-0118): every statement without `USE` targets it, in one-shot, piped and interactive use. Names match case-insensitively; any other value exits 2 with the top-level usage line and the `--help` hint before the store opens ([Help and usage errors](#help-and-usage-errors)). No short form, env var or dot-command; not accepted by `pull`, in either flag position (exit 2). `skill`/`upgrade`/`telemetry` never open a store, so a pre-verb `--namespace` is accepted and inert there. |
| `--` | | Treat all following arguments as positional |
| `-h, --help` / `--version` | | Usage / version. `--version` prints two lines — `Relate v<version>` then `License: Apache 2.0 (binary)` — in every mode but `json` (`--csv` and `--jsonl` included, ADR-0432); `--version --json` prints one compact JSON object on one line with the members `arch`, `license`, `name`, `os`, `version` — `arch` and `os` name the machine the binary runs on, `name` is `relate`, `version` is the release, and `license` is the one contract-bearing value, `Apache 2.0 (binary)`, by name only, never the license text (ADR-0193, ADR-0432); the concrete recordings stay in [`docs/reference/cli-flags.md`](../../reference/cli-flags.md) and [`examples/agents.md`](examples/agents.md). After a command word, `relate <command> --help` prints that command's help ([Help and usage errors](#help-and-usage-errors)) |

The hand-rolled parser also accepts legacy `-json`, `-csv`, `-jsonl`,
`-markdown`, `-noheader`, `-mode`, `-param`, and `-version`/`-v` spellings and
`-c` as shown above. There is no single-dash `-load-csv-dir` alias; the newer
`--format` and `--toon` spellings (ADR-0239) and `--pretty` (ADR-0240) are
double-dash only. An unknown
flag exits 2 with the command usage line and a `--help` hint; a missing
query-run flag value exits 2 without usage. `--help` exits 0. Legacy output
spellings
select the same formats as their double-dash forms; when no output flag is
given, the default is still auto — `table` on a terminal, `jsonl` otherwise
(`toon` when agent mode is in effect, ADR-0426)
(ADR-0107).

In `json` and `jsonl` modes, non-finite floats render as the JSON strings
`"NaN"`, `"Inf"`, and `"-Inf"` — the same spellings `table`/`csv` print —
never `null`, so a non-finite float is never indistinguishable from a
genuine null (ADR-0280).

`inf`, `infinity`, and `nan` are CIP-55 float-literal keywords: in expression
position they always read as the `Infinity`/`NaN` literals, even when a
variable of that name is in scope (`WITH 0 AS nan RETURN nan` yields `NaN`).
Binding such a name (a pattern variable `(inf:N)`, an alias `AS nan`) is
legal but every expression-position use reads the literal — backtick-escape
(`` `inf` ``) to reference the variable (ADR-0280).

`everysecond` can lose the latest approximately one second of acknowledged
writes in a crash; delayed or failed barriers can extend the window.
`snapshot` can lose all acknowledged writes since the latest completed
checkpoint. Its timed checkpoint defaults to 300 seconds and is tuned with
`--checkpoint-interval-secs`; `0` disables only the timed checkpoint.

`--param` splits at the first `=` and trims the parameter name. JSON arrays
and objects decode recursively; **every** JSON object is an ordinary map,
including `{"__type":"point","x":1}` and even a complete point-looking
object. `__type` does not convert parameters to typed values (ADR-0087). Pass
`--param 'coords={"x":1.5,"y":2.5}' --param 'iso="2024-03-14"'` and use
`point($coords)` or `date($iso)` in Cypher to construct typed values. A bare
word such as `name=Alice` is a string; a date starting with digits must be
JSON-quoted as above. Bad JSON for a value that looks like JSON, non-finite
JSON numbers such as `1e400`, and an integer above `i64::MAX` still parsed as
a JSON integer (such as `18446744073709551615`) report
`Error: invalid --param value: parameter N: ...` on stderr and exit **2**
before opening the database or executing the query. Very large decimal
literals parsed as finite JSON floats bind as floats instead of integers. See the
[executable parameter example](examples/params.md).

`--namespace` selects the session namespace — one of the store's three fixed
namespaces `neo4j` (user data, the default), `system` (SHOW/administration
commands) and `system_ontologies` (listed but not queryable, ADR-0242: every
statement routed to it fails with 42N17, except the ADR-0116 §2
administration commands) — matched case-insensitively
(ADR-0116/ADR-0118/ADR-0242). An unknown name prints
`Error: invalid --namespace: unknown namespace '<name>' (expected neo4j, system or system_ontologies)`
plus the top-level usage line and the `--help` hint to stderr and exits **2**
before the store is opened ([Help and usage errors](#help-and-usage-errors)).
`pull` does
not accept the flag (it always writes `neo4j`): after the verb it is
reported as an unknown argument, and a valid `--namespace` placed **before**
the `pull` verb exits **2** with
`Error: --namespace is not valid with pull (pull always writes the neo4j namespace)`
plus the `pull` usage line and a `--help` hint, rather than being silently
ignored. Example:

```sh
relate ./store.rl --namespace system -c 'SHOW DATABASES'
```

> **INFO — argv visibility.** The positional `QUERY` argument and `--param`
> values travel on the command line, so they are visible to other processes in
> `ps` output and are captured by shell history. This is standard DB-shell
> behavior. When a literal is sensitive, source the `--param` value from a
> shell-only place (e.g. an environment variable) or feed the query through
> piped stdin instead of the `QUERY` positional; piped stdin is not exposed in
> `ps`.

Opening a persistent `DATABASE` takes nonblocking exclusive ownership of its
Native root and WAL directory (including `--wal-dir`), or its `.rl` host file
(ADR-0097). If already owned by another opener, the CLI prints
`Failed to open Relate: storage locked: <path>` to stderr and exits 5
(`conflict`, [Query-run errors](#query-run-errors)) before starting the REPL
or executing a query. Invalid storage/permissions
produce their respective storage or I/O open error, not a lock-contention
message. No lock flag is required.

Fields follow RFC 4180 double-quoting: a field wrapped in `"` may contain the
delimiter and newlines (a quoted record can span physical lines, and `""`
inside a quoted field is one `"`), for the header row and data rows alike,
with any `FIELDTERMINATOR`. An unterminated quote is preserved as-is (the
rest of the file reads as one field) rather than silently dropping data. A
data row with fewer fields than the `WITH HEADERS` header maps the missing
keys to null (Neo4j's `LOAD CSV WITH HEADERS` semantics, so an absent field
is distinguishable from an empty one); extra fields past the header width
are dropped, so the row map is keyed by exactly the header names. The header is always
the file's first record; blank records after it produce no data rows.

For enabled `LOAD CSV`, relative paths resolve within the configured root;
absolute paths require local `file:///` and must remain under it. Traversal,
symlink escapes and special files fail as `LOAD CSV file unavailable`, with no
path or OS error leaked. Files over 8,388,608 bytes fail with `LOAD CSV file
exceeds byte limit`; repeated references to a file in one statement reuse
the bounded source bytes. See the [executable CLI example](examples/load_csv.md).

REPL prompt `cypher>` (continuation `   ...> `). On a TTY the prompt uses
rustyline for GNU readline-style editing (arrow keys, Home/End, emacs
bindings) and in-session history (ADR-0073). There is no tab completion and
no history file. Ctrl-C cancels the pending buffer; Ctrl-D / EOF executes
any non-empty pending input and exits 0 (1 if the final checkpoint below
fails). `TERM=dumb` is treated as
unsupported (cooked stdin, no cursor-key editing). Statements end with `;`.
`.begin` / `.commit` / `.rollback` / `.checkpoint` / `.info` /
`.timer on|off` / `.quit`. `.timer on` prints one `Time:` line per
statement after its result — the engine-reported ADR-0221 total beside its
ADR-0491 PARSE / PLAN / EXECUTE phase split, every number one-decimal
milliseconds: `Time: 12.3ms (parse 0.4 / plan 1.1 / execute 10.8)`. `.info`
prints a read-only diagnostic
(ADR-0165): for a persistent `DATABASE`, cold store metadata — the same
store block, with the same wording, that `relate info` prints (layout,
codec, manifest format version, checkpoint LSN, commit timestamp, total
size in bytes, WAL segments past the checkpoint; `snapshot: none yet`
before the first checkpoint) — plus exact
live node/relationship counts and the `SHOW INDEXES` / `SHOW CONSTRAINTS`
catalog through the open connection; on `:memory:` the store metadata is
marked not applicable and only the live counts and catalog print. Piped
stdin runs the batch and stops on the first error (the failing statement's
exit code, [Query-run errors](#query-run-errors)). A
read error on piped stdin — including invalid UTF-8 — is fatal: the CLI prints
`Error: could not read query input from stdin: <reason>` to stderr, executes
nothing, and exits 1. There is no size cap on piped stdin.

The output default follows **stdout**, not stdin: when stdout is piped or
redirected the run emits JSONL unless an output flag (`--json`, `--csv`,
`--jsonl`, `--markdown`, or `--mode`) is given — or agent mode is in effect
(ADR-0407's ladder, detected or explicit), which defaults the piped run to
`toon` (ADR-0426), so `relate db -c '...' |
jq` works with no flag; a piped-stdin run still writing to a terminal keeps
the table default (ADR-0107).

Every exit after a successful open — one-shot `QUERY`, end of piped stdin,
a piped-stdin read error, Ctrl-D / EOF, and `.quit` / `.exit` — closes a
persistent `DATABASE` cleanly: the CLI discards any open explicit
transaction (`.begin` without `.commit`) and runs a final checkpoint. That
checkpoint pins the `--snapshot-format` codec into `manifest.json`, so a
store created with `--snapshot-format json` reopens as JSON without the flag.
`:memory:` skips it. If the final checkpoint fails, the CLI prints
`Error: final checkpoint failed: <reason>` to stderr and exits 1 when the
exit code would otherwise be 0; a non-zero statement exit code is kept.
Every persistent run, including a read-only one, rewrites the snapshot on
exit: this costs time on large stores, and a full disk now yields exit 1.

The discard of an open transaction is never silent (ADR-0164). When a
transaction is still open at exit, every exit path prints

```
Warning: exiting with an open transaction; rolling back uncommitted changes. Use .commit before exiting to persist them.
```

to stderr, before the final checkpoint. The wording is byte-for-byte stable:
wrapper scripts grep stderr for `rolling back`. One-shot and piped-stdin
runs then exit **1** (a higher code already earned by a failed statement is
kept), so batch callers can detect a forgotten `.commit` via the exit code;
interactive exit (`.quit` / EOF, including a bare `relate`) prints the
same warning but keeps exit **0** — implicit rollback on connection drop is
the ADR-0028 contract, not an error. `.commit` before exiting remains the
only way to persist the transaction's writes.

On a TTY, `--help` also notes that the interactive shell supports
readline-style editing. Builds without the `repl` feature
([Build bundles](#build-bundles)) use a plain stdin line loop with the same
prompts, `;`-terminated statements, dot-commands, and EOF behavior, but no
cursor-key editing or history, and `--help` omits the readline note.

## Output safety

Human-facing output is hardened against control-character injection and
spreadsheet-formula injection **by default**: there is no opt-out flag, and the
behavior does not depend on whether stdout is a terminal. JSON and JSONL are
the byte-exact exception — serde already escapes controls, so those modes are
unchanged.

| Mode | Hardening |
| --- | --- |
| `table`, `markdown` | Every cell **and header** escapes C0/C1/DEL control characters before layout: a literal backslash becomes `\\`, newline/tab/CR become `\n` / `\t` / `\r`, and every other control becomes `\u{XXXX}` with uppercase hex (e.g. `\u{1B}`), so an ESC/OSC-52 sequence can never reach the terminal. `markdown` additionally escapes `|` as `\|` so a value cannot inject table columns. Escaping runs before width math, so a raw control char (display width 0) still counts against a `.width` cap. |
| `csv` | Every field (header or value) is hardened in three ordered steps: **escape** controls, **guard** spreadsheet formulas, then **quote** per RFC 4180. Escaping first turns a tab (`0x09`) into the two characters `\t` and every other unsafe character (C0/C1/DEL, `Cf` formats) into braced uppercase `\u{XXXX}` (e.g. `\u{1B}`); LF and CR stay raw so the quoting step keeps multi-line values byte-intact, and a literal backslash is left untouched. The formula guard then prefixes a single `'` to a field whose first character is `=`, `+`, `-`, `@`, or a raw CR (`0x0D`) — a leading tab has already become `\t`, so it no longer triggers the guard, while a field that is exactly CR still becomes `"'\r"`. |
| `cypher` | String literals escape controls the lexer understands: `\n`, `\t`, `\r` becomes `\u000D`, and every other control becomes `\uXXXX` (uppercase). Identifiers, backticked property keys, and the `// column <name> (<TYPE>) omitted` comment path sanitize controls first, so a newline in an alias cannot break out of the comment or split the statement. |

**stderr diagnostics** are sanitized too: every interpolated error or warning
message routes through one printer that removes unsafe characters (Cc
controls, `Cf` formats) before they reach the terminal. Whitespace control
characters (LF, CR, tab, …) become a single space instead of vanishing, so a
multi-line error text renders legibly on one line (`a.\nb` prints as
`a. b`, not `a.b`); every other unsafe character (ESC, bidi overrides,
zero-width marks) is stripped. No raw control byte ever reaches stderr.

The CSV formula guard has a deliberate consequence: a negative numeric cell
such as `-5` is emitted as the text `'-5`, so machine consumers that parse CSV
output should account for the leading `'`. Because control escaping runs first,
only `=`, `+`, `-`, `@`, and a raw leading CR reach the guard; a leading tab is
already the text `\t`. There is no flag to disable the guard. A JSON or JSONL
serialization failure now returns an error from the formatter instead of
silently emitting `[]` / `{}`. See the
[executable output-safety example](examples/output_safety.md).

## Usage metrics

The CLI binary collects anonymous usage metrics and sends them to Mixpanel
(ADR-0079). The embedded library, the C ABI, and the Bolt server never
phone home. Builds without the `telemetry` feature (the `agent` and
`minimal` bundles, [Build bundles](#build-bundles)) record and send
nothing: nothing below applies to them.

Events and properties (this listing is the privacy disclosure):

| Event | Event-specific properties |
| --- | --- |
| `RELATE-CLI_STARTUP` | `kind`: `oneshot` \| `stdin` \| `repl` \| `version` \| `help` |
| `RELATE-CLI_QUERY` | `success` (bool), `source`: `oneshot` \| `stdin` \| `repl`, `storage`: `memory` \| `persistent`, `output_mode`: `table` \| `csv` \| `json` \| `jsonl` \| `toon` \| `markdown` \| `cypher` — the mode actually used, so a piped default run reports `jsonl` (ADR-0107) or `toon` under agent mode (ADR-0426); `--mode envelope` reports `json` (ADR-0195) |
| `RELATE-CLI_COMMAND` | `command` (lifecycle command name, e.g. `skill`, `upgrade`, `telemetry`, `pull`, `run`), `action` (e.g. `install`; `extension` for `run` — never the installed action's name, ADR-0429 §9; omitted for action-less commands), `success` (bool) |
| `RELATE-CLI_HELP` | base dimensions only |

Every event also carries the Mixpanel envelope and base dimensions:

| Property | Meaning |
| --- | --- |
| `token` | Mixpanel project token |
| `time` | Event time, ms since epoch |
| `distinct_id` | Random install ID (see below) |
| `$insert_id` | Random UUID per event (dedup) |
| `$os` | Operating system |
| `arch` | CPU architecture |
| `version` | CLI version |
| `uptime` | Seconds since CLI start |
| `invoker` | `human` \| `agent` \| `ci` — `agent` when a known agent env var is set, else `ci` when `CI` is truthy, else `human` |

**Identity.** The install ID is a random UUID generated once and persisted
in `state.json` under `<config>/relate/`. It is not a hardware or OS
machine identifier — none is ever read.

**Never collected.** Query text, database paths, and parameter values are
structurally excluded: the event-builder types have no fields for them, so
capturing them is a compile error, not a policy violation (ADR-0079 §4).

**Delivery.** A run's exit writes its pending events to a local spool
file under `<config>/relate/` — one fresh JSONL file per run, mode 0600
on unix, never touching a file that already existed — and that is the
whole exit cost: no network wait, ever (ADR-0079 §5, ADR-0545). The
next enabled run's background drain thread (started at that run's
first recorded event) claims the spool files (spooled files and any
claims a killed drain left behind; regular files only, never symbolic
links — a planted link is neither claimed, read, nor followed) and
POSTs them in batches of at most 50 with a 2-second HTTP timeout — at
most 8 files per sweep, the rest waiting for the next run — deleting
each claim only after every batch is confirmed; an undelivered claim
waits for the next enabled run, and a claim whose content cannot be
read is removed on the spot — attempt-once, like a corrupt line — so
undeliverable content never starves a sweep. Each file is bounded by the run's in-memory backlog cap,
and the spool holds at most 64 pending files — past that a sweep drops
the excess. Delivery is at-least-once — the sink dedups on
`$insert_id`, making it effectively-once — and event `time` is stamped
at emit, so late delivery does not skew time-based reports. A run with
no resolvable config directory records nothing — the same degraded
mode as its ephemeral install ID. All telemetry errors are swallowed —
nothing on stdout or stderr, exit codes unaffected.

**Opt-out.** Telemetry defaults to ON. First match wins (ADR-0079 §6):

1. `DO_NOT_TRACK=1` (or `true`)
2. `RELATE_TELEMETRY` (`0`/`false`/`off` disables)
3. `telemetry: false` in `<config>/relate/config.json` (written by
   `relate telemetry off`)
4. enabled

The first enabled run prints a one-time notice to **stderr** pointing at the
opt-out, regardless of whether stdin is a terminal (the notice is claimed once
and is suppressed entirely when telemetry is opted out). A `notice_shown` flag
in `state.json` suppresses repeats.

`state.json`, `config.json`, and the per-run spool files
(`telemetry-spool-<uuid>.jsonl`, `telemetry-claim-<uuid>.jsonl`) are
written under `<config>/relate/` — the state and config files via unique
`create_new` staging files renamed into place, the spool by a direct
0600-creating append — and all of them are mode **0600** on unix: the
install UUID, the opt-out choice, and the spooled event envelopes are
private.
