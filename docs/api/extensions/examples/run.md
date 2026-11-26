# Example: author, install, and run a command-kind extension

One extension is a package directory under the extensions dir. This
example builds the smallest possible one — a POSIX `sh` child that speaks
MCP stdio and reports a two-row import — installs it by hand, runs it into
a store, and queries the result. It is the fixture the end-to-end suite
[`surfaces/relate-cli/tests/integration/e2e_run.rs`](../../../surfaces/relate-cli/tests/integration/e2e_run.rs)
drives through the real binary in CI (the same manifest, the same child,
the same assertions; the wire-driven tests are unix-gated, like the host
and protocol suites).

## The package

`$XDG_CONFIG_HOME/relate/extensions/` (falling back to
`~/.config/relate/extensions/`) is the only discovery root — never
the working directory. A package is a directory holding `extension.json`
plus the executable:

```bash
pkg="$XDG_CONFIG_HOME/relate/extensions/greet-pkg"
mkdir -p "$pkg"
```

`extension.json` declares the mount, the MCP revision it speaks, the flag
table the host parses argv from, and the lifecycle knobs:

```bash
cat > "$pkg/extension.json" <<'EOF'
{"kind":"command","name":"greet","version":"0.1.0","protocol":"2025-06-18",
 "flags":[{"kind":"string","name":"message","default":"hello"},
          {"kind":"integer","name":"count","default":3},
          {"kind":"boolean","name":"shout","default":false},
          {"kind":"repeatable","name":"tag","default":[]}],
 "positional":{"name":"DIR"},"commit":"per-batch","executable":"run.sh"}
EOF
```

The child is any executable that reads the host's one-line flag envelope
from stdin, then speaks MCP: JSON-RPC 2.0 requests on stdout, the host's
answers on stdin. No shell is involved, and it never sees a store path.
This one reads the envelope away, initializes, writes its two rows with
one `write_cypher`, and reports the summary:

```bash
cat > "$pkg/run.sh" <<'EOF'
#!/bin/sh
read -r envelope
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"greet","version":"0.1.0"}}}'
read -r answer
printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/initialized"}'
printf '%s\n' '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"write_cypher","arguments":{"statement":"UNWIND $rows AS row CREATE (n:N {id: row.id})","parameters":{"rows":[{"id": 1},{"id": 2}]}}}}'
read -r answer
printf '%s\n' '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"report_output","arguments":{"text":"imported 2 nodes","starters":["MATCH (n:N) RETURN count(n) AS n"]}}}'
read -r answer
EOF
chmod +x "$pkg/run.sh"
```

Every request carries an `id`; the host echoes it and the child must match
it. The shipped bash kit (`mcp-client.sh`) writes the same session with
the envelope readers, the id matching and the tool calls already in place —
`extension init` inlines it, so a scaffolded package only edits `walk()`.

## Run it

The action's own flags and defaults come from the manifest —
`relate run greet --help` prints them:

```bash
relate run greet --help
```

```text
Usage: relate run greet [DIR] [--message VALUE] [--count N] [--shout] [--tag VALUE]

Run the installed extension 'greet' (v0.1.0) as a batched import into a store the host owns.
The child walks its source and drives the import as an MCP client; the
host alone owns the store, the caps and the exit codes (ADR-0447).

  DIR                   The action's positional (at most one)
  -d, --database PATH   Database to import into (default :memory:).
                         An explicit --database exits after the import; without
                         it, a TTY opens the shell on the in-memory graph.
  --message VALUE       A string flag (default 'hello')
  --count N             An integer flag (default 3)
  --shout               A boolean flag (default false)
  --tag VALUE           A repeatable string flag (default none)
```

With an explicit target the run exits after the summary — the child's
one-line result from `report_output`, then each starter indented two
spaces:

```bash
relate run greet -d ./graph
```

```text
imported 2 nodes
  MATCH (n:N) RETURN count(n) AS n
```

The host wrote the child's `write_cypher` rows through its own store;
query it like any other (`--jsonl` pins one JSON object per row — the piped
default is JSONL, TOON under agent mode; ADR-0107, ADR-0426):

```bash
relate query --database ./graph --jsonl "MATCH (n:N) RETURN count(n) AS n"
```

```text
{"n":2}
```

Without `--database` the import lands in `:memory:` — on a TTY the shell
opens on the graph (ADR-0286); piped, the process exits after the summary
and leaves no store behind.

## What fails closed

An uninstalled name — any word the registry does not resolve —
never fetches; it fails with the pinned discovery error, exit 2:

```bash
relate run git-to-graph
```

```text
Error: no installed action 'git-to-graph' — first-party extension available: relate extension install git-to-graph
Usage: relate run <name> [flags]
Run 'relate run --help' for more information.
```

A broken install (a malformed manifest, a colliding name) fails the verb
closed the same way, help pages included. A child that misbehaves at
runtime — a request before `initialize`, a cap exceeded, a non-zero exit —
is one `Error:` line and exit 1.

Full contract: [spec.md](spec.md) — the manifest schema, the MCP mount and
its five tools, host execution, and the error classes. The line-by-line
protocol walkthrough is [mcp-mount.md](mcp-mount.md).
