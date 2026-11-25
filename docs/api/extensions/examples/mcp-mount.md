# Example: an MCP stdio extension

An extension mounts its executable over two std pipes: the child writes
JSON-RPC 2.0 requests on stdout and reads the host's responses from stdin,
after the one request envelope. This example walks the protocol a child must
speak;
the compiled fixture that CI drives through the real binary is
[`surfaces/relate-cli/tests/fixtures/mcp-extension-fixture.rs`](../../../../surfaces/relate-cli/tests/fixtures/mcp-extension-fixture.rs)
(Rust, links `relate_wire::mcp`), and the shell children of
[`surfaces/relate-cli/tests/integration/e2e_extension_mcp.rs`](../../../../surfaces/relate-cli/tests/integration/e2e_extension_mcp.rs)
show the same lines hand-printed.

## The package

`extension.json` declares the mount, the MCP revision it speaks, the flag
table and the commit policy:

```bash
pkg="$XDG_CONFIG_HOME/relate/extensions/greet-mcp"
mkdir -p "$pkg"
cat > "$pkg/extension.json" <<'EOF'
{"kind":"command","name":"greet-mcp","version":"0.1.0","protocol":"2025-06-18",
 "flags":[{"kind":"integer","name":"count","default":3}],
 "commit":"per-batch","executable":"run.sh"}
EOF
```

## The handshake

Every step below is one line. The host writes the flag envelope first (the
`count` value), then answers the child's requests:

```
# host -> child (stdin, once)
{"count":3}

# child -> host
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"greet-mcp","version":"0.1.0"}}}
# host -> child: serverInfo, protocolVersion, capabilities, freshStoreRequired, commitPolicy, caps, capacity

# child -> host: the handshake confirmation (a notification, never answered)
{"jsonrpc":"2.0","method":"notifications/initialized"}

# child -> host: the tools
{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"write_cypher","arguments":{"statement":"UNWIND $rows AS row CREATE (n:Greet {id: row.id})","parameters":{"rows":[{"id":1},{"id":2}]}}}}
{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"batch_cypher","arguments":{"statements":[{"statement":"CREATE (n:One)"},{"statement":"CREATE (n:Two)"}]}}}
{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"report_output","arguments":{"text":"imported 2 nodes","starters":["MATCH (n:Greet) RETURN n.id AS id"]}}}
```

Every request except the notification carries an `id`; the host echoes it
and the child must match it. A `tools/call` result is the tool's JSON
carried as MCP text content; the writing tools close it with
`storeRemainingBytes`, the store's budget after the write.

Want a client instead of a hand-rolled loop? The shipped examples do this
with an SDK: `so-to-graph.py` uses the official Python SDK's `Client` and
`so-to-graph.ts` the TypeScript SDK's, each with one small transport over
the process's own stdio, and `mcp-client.sh` is the bash kit the shell
example inlines.

## Run it

```bash
relate run greet-mcp --count 3 --database /tmp/greet.db
```

The summary from `report_output` is printed by the host (text, then each
starter indented two spaces); the store is checkpointed before the process
exits. A failing `batch_cypher` statement under `"commit": "whole-import"`
fails the whole run and commits nothing.

## Gotchas

- The envelope line comes **before** the handshake, and stdin must stay
  open afterwards for the host's responses — a `cat > /dev/null` child
  can never speak this wire.
- `initialize` params must carry a string `protocolVersion`; the mount
  serves one revision and answers with it, so a child that speaks another
  one fails the handshake instead of downgrading.
- A read whose result would not fit one 8 MiB response line is refused
  with `the tools/call result would exceed the 8388608-byte response cap` —
  narrow the read (`LIMIT`, fewer columns) instead of retrying.
