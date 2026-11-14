# CLI extensions: the `run` mount

Command-kind extensions: userland commands that install as packages and
mount as `relate run <name> [flags]` (ADR-0429), speaking MCP stdio to
a host that alone owns the store (ADR-0442, ADR-0447). The child is a
minimal MCP client over the two pipes — an MCP SDK works unchanged, and so
does a shell script that writes one JSON-RPC line per request.

## Install

Nothing to install to *use* extensions — the `extensions` feature ships in
the `full` and `agent` bundles (`run` is a not-included command only in
`minimal`; verify with `relate --version` and `relate run
--help`).

An extension *package* is a directory under
`$XDG_CONFIG_HOME/relate/extensions/<pkg>/` (falling back to
`~/.config/relate/extensions/` when `$XDG_CONFIG_HOME` is unset,
empty, or relative — a relative value is treated as unset, so the
discovery root is never resolved against the working directory)
holding `extension.json` plus the executable — the only discovery
root; never the working directory, so a
cloned repo can never add commands. The `relate extension init |
install | list | remove | check` verbs (ADR-0431) ship with this CLI:
`install` and `list --available` need the `extension-install` feature
(`full` and `agent` builds), while `init`, `list`, `remove` and `check` ride
`extensions`. An uninstalled name fails closed with the pinned discovery
error — a query run never fetches anything.

| Verb | What it does |
| --- | --- |
| `extension init <name> [--lang sh\|py\|ts\|rust]` | Scaffold a package into a new `<name>/` directory: the manifest, the language's minimal MCP client generated inline (no published dependency to install), a TODO-marked `walk()`, a README quickstart |
| `extension install <name> [--github REPO@TAG\|@SHA \| --url URL \| --from DIR] [--version V] [--force] [--re-approve \| --approve SHA]` | Put a package on the machine — the signed first-party channel by default, `--github` the commit-pinned GitHub release, `--from DIR` the offline local act, `--url` the explicit third-party one |
| `extension list [--available \| --trust]` | The installed set (offline), the channel's catalog with `--available`, or the recorded GitHub pins with `--trust` |
| `extension remove <name> [--forget-pin]` | Take a package off (the provenance marker is required); `--forget-pin` also clears its recorded GitHub pin |
| `extension check <dir>` | Run a package directory through the mount's six stages without a store or network (see below) |

## Check a package before installing it

`relate extension check <dir>` runs a package directory through the
mount's six stages — `manifest`, `executable`, `spawn`, `initialize`,
`notifications/initialized`, `tools/list` — printing one line per stage
and stopping at the first failure, then one advisory line on the recorded
GitHub pin (`approved` / `drifted` / `not from a GitHub source`, from the
provenance marker and the per-user trust store; it never changes the exit
code). It opens no store (no `--database`), fetches nothing and runs no
tool, so it is safe anywhere: exit **2** for the package's declaration and
contents, **1** for what it does when run, **0** when it would mount. Run
it after every manifest or handshake change; the authoring loop — with a
failing check and its fix — is in `docs/api/extensions/authoring.md`, and
the check verb's stages are in `docs/api/extensions/spec.md`.

## Minimal usage

Fastest path: `relate extension init my-ext` writes a complete package
(`./my-ext/` — manifest, executable, README), then `extension install my-ext
--from ./my-ext` and `relate run my-ext -d ./store` run it as it is.
Edit only `walk()`; the glue around it is generated. The author loop
(`init → edit → check → install --from → run`), the language tiers and the
transport contract are `docs/api/extensions/authoring.md`; the trust tiers
and what the install prints are `docs/api/extensions/install.md`.

To hand-roll instead, write the manifest (the mount name, the MCP revision,
the flag table the host parses argv from, the commit policy) and a child that
reads its request envelope from stdin, then speaks MCP: `initialize`,
`notifications/initialized`, one `write_cypher` and `report_output`. The
shortest honest child is the shipped bash kit
(`docs/api/extensions/examples/mcp-client.sh`); hand-rolling it looks like
this:

```sh
pkg="$XDG_CONFIG_HOME/relate/extensions/greet-pkg"
mkdir -p "$pkg"
cat > "$pkg/extension.json" <<'EOF'
{"kind":"command","name":"greet","version":"0.1.0","protocol":"2025-06-18",
 "flags":[{"kind":"string","name":"message","default":"hello"}],
 "commit":"per-batch","executable":"run.sh"}
EOF
printf '#!/bin/sh\nread -r envelope\n' > "$pkg/run.sh"
cat >> "$pkg/run.sh" <<'EOF'
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"greet","version":"0.1.0"}}}'
read -r answer
printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/initialized"}'
printf '%s\n' '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"write_cypher","arguments":{"statement":"UNWIND $rows AS row CREATE (n:N {id: row.id})","parameters":{"rows":[{"id": 1},{"id": 2}]}}}}'
read -r answer
printf '%s\n' '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"report_output","arguments":{"text":"imported 2 nodes"}}}'
read -r answer
EOF
chmod +x "$pkg/run.sh"

relate run greet --help     # the action's page, from its manifest
relate run greet -d ./graph # the import; exits after the summary
relate query --database ./graph "MATCH (n:N) RETURN count(n) AS n"
```

The full walkthrough (every flag kind, a positional, the summary's
starter queries, the discovery error) is
`docs/api/extensions/examples/run.md`; the same fixture runs in CI as
`surfaces/relate-cli/tests/e2e_run.rs`.

## The MCP wire

The mount speaks MCP stdio, and only MCP stdio (ADR-0447): the manifest
declares no wire, and its `protocol` is the MCP revision
(`"2025-06-18"`). The package, the install verbs, the flag table, the
commit policy and the caps are the host's; the traffic is the child's.

The **child is the MCP client** and the host is the MCP server. After the
one-line flag envelope on stdin, the child writes JSON-RPC 2.0 on stdout
and reads the answers on stdin:

1. `initialize` with `"protocolVersion":"2025-06-18"` and its
   `clientInfo`. The host answers its own revision, capabilities,
   identity and the mount's facts (commit policy, caps, store capacity);
   a client that names another revision is answered with `2025-06-18` and
   must fail rather than downgrade.
2. `notifications/initialized`, id-less — the one confirmation the host
   never answers.
3. `tools/list` and `tools/call` for as long as the walk takes.

The five tools (results arrive as MCP text content, one JSON object
each):

| Tool | Arguments | Result |
| --- | --- | --- |
| `update_schema` | `{statement}` | `{created}` — the index/constraint names the call added |
| `write_cypher` | `{statement, parameters}` | `{counts, storeRemainingBytes}`; a `rows` array feeds `$rows` |
| `read_cypher` | `{statement, parameters}` | `{columns, rows}` — only a statement the plan classifies as a read passes |
| `batch_cypher` | `{statements: [{statement, parameters}]}` | `{results, storeRemainingBytes}` — one transaction per call |
| `report_output` | `{text, starters?}` | `{}` — the run summary, printed and handed to the shell |

`read_cypher`'s guard is the plan's own type (`EXPLAIN`), not a keyword
match: a writing or schema statement is refused with `Only MATCH queries
are allowed for read-query`, and `update_schema` refuses everything else
with `Only DDL queries are allowed for update-schema`. A refusal is an
answered JSON-RPC error (`-32602`), so the child may narrow the read and
carry on.

Writing a child in Rust: link `relate_wire::mcp` — the one kit both
sides share. `serve` is the whole `main` (envelope, handshake, failure
report, exit); `write_rows` is the chunked `write_cypher`;
`update_schema` is the DDL call; `report_output` sends the summary. The kit
also owns the revision constant and the tool-name constants, so a child
never spells them itself. The shipped hello-world examples
(`docs/api/extensions/examples/`): the bash kit (`mcp-client.sh`, which
`so-to-graph.sh` inlines because a package is one executable), Python on
the official MCP SDK's `Client` with a small `Transport`, and TypeScript on
the same shape.

## Gotchas

- **The child never sees the store** — `-d`/`--database` is host property
  (ADR-0383); the child gets only its resolved flag values as one JSON
  envelope on stdin, never raw argv, never a store path. All writes flow
  through the host-owned store.
- **Exit codes split usage from runtime** — usage problems (an unknown
  flag, a manifest problem, an uninstalled name) exit **2**; runtime
  failures (a protocol violation, a cap exceeded, a child error or
  non-zero exit) exit **1** with one sanitized `Error:` line.
- **stdin stays open** — the child reads the one-line flag envelope first
  and must leave stdin open afterwards, because the host's responses
  arrive there. A `cat > /dev/null` child can never finish the handshake,
  and the mount reads with a deadline: a child that writes nothing fails
  the run with `the extension wrote no MCP request within 10s` rather
  than hanging it.
- **The host caps the child** — 8 MiB per JSON-RPC line, 1000 rows per
  writing call, 4096 statements per `batch_cypher` call, and 128 MiB
  across the session under `whole-import` framing only (a `per-batch`
  session has no whole-run cap — the per-call caps and the store's
  live-graph limit are its bounds); the first violation kills the child
  and fails the run, never truncates.
- **`protocol` must be `"2025-06-18"`** — the MCP revision this host
  serves; any other value fails closed naming both, before the child
  runs. The protocol is the only host↔package coupling — not the CLI
  version.
- **The commit policy is manifest property** — every `batch_cypher` call
  runs under the manifest's `commit`; the child never chooses per call.
- **A broken install breaks the verb** — a malformed manifest or a name
  collision fails every `run` invocation closed (exit 2), `run --help`
  included: the grammar is machine state, never partially loaded.
- **A hidden extension still runs** — `"hidden": true` removes it from
  listings only; `"requires-fresh-store": true` refuses a populated
  target before the child spawns.

Full contract: `docs/api/extensions/spec.md` (manifest schema, the
Transport contract section, caps, host execution, errors); the author loop
`docs/api/extensions/authoring.md`; the install tiers
`docs/api/extensions/install.md`.
