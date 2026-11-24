# Example: an MCP host session over stdio

MCP hosts (Claude Desktop, Claude Code) spawn the server as a child
process and speak newline-delimited JSON-RPC 2.0 over its stdin/stdout.
No auth is needed on stdio — the parent spawned the server, so there is no
trust boundary to cross.

An MCP host configuration entry looks like:

```json
{
  "mcpServers": {
    "relate": {
      "command": "relate",
      "args": ["serve", "--mcp", "stdio", "--database", "/tmp/graph.db"]
    }
  }
}
```

The session itself is one JSON-RPC message per line. From a shell you can
reproduce exactly what a host does (the assertions below are what the
crate's `tests/stdio_tools.rs` integration test checks against the same
stdio transport, and the CLI's e2e suite spawns
`relate serve --mcp stdio` as a child — the product path a host
spawns):

```bash
{
  echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"demo","version":"0"}}}'
  echo '{"jsonrpc":"2.0","method":"notifications/initialized"}'
  echo '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"write_cypher","arguments":{"statement":"CREATE (:Person {name: '"'"'Ada'"'"'})"}}}'
  echo '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"read_cypher","arguments":{"statement":"MATCH (p:Person) RETURN p.name AS name"}}}'
  echo '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"get_schema","arguments":{}}}'
} | cargo run -p relate-cli -- serve --mcp stdio --database :memory: 2>/dev/null
```

Expected output, one response per request (the notification gets none):

```json
{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"relate-mcp","version":"…"}}}
{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"{\"columns\":[],\"columnTypes\":[],\"rows\":[],\"stats\":{\"nodes_created\":1,…},\"notifications\":[],\"elapsed_ms\":0.12,\"parse_ms\":0.03,\"plan_ms\":0.05,\"execute_ms\":0.04,\"error\":null}"}],"isError":false}}
{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"{\"columns\":[\"name\"],\"columnTypes\":[\"STRING\"],\"rows\":[{\"name\":\"Ada\"}],\"stats\":{…},\"notifications\":[],\"elapsed_ms\":0.08,\"parse_ms\":0.02,\"plan_ms\":0.03,\"execute_ms\":0.03,\"error\":null}"}],"isError":false}}
{"jsonrpc":"2.0","id":4,"result":{"content":[{"type":"text","text":"{\"Person\":{\"type\":\"node\",\"count\":1,…}}"}],"isError":false}}
```

The write's `notifications` is the classification `EXPLAIN`'s list, and a
write's own target names are not read-side evidence (ADR-0363 decision
13): `CREATE (:Person {name: 'Ada'})` brings `Person` and `name` into
existence, so the fresh store raises no 01N50/01N52 for them and the array
stays `[]`. The read that follows finds both names known, so it carries
`[]` too; a typo in a read clause of the same write still reports.

A write sent to `read_cypher` is rejected as a tool error, not a protocol
error:

```json
{"jsonrpc":"2.0","id":5,"result":{"content":[{"type":"text","text":"Only MATCH queries are allowed for read-query"}],"isError":true}}
```

A write may carry transaction metadata (ADR-0610): the `tx_metadata`
argument rides `write_cypher` (call-level on the extension mount's
`batch_cypher` too) and serializes into the commit's transaction record on
a store the server opened with `RELATE_PREVIEW=value-provenance` — the
assertion is what the crate's `tests/tx_metadata.rs` integration test
checks against the same stdio transport. Without the opt-in this call is
an `isError` tool result carrying the authored preview error:

```bash
{
  echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"demo","version":"0"}}}'
  echo '{"jsonrpc":"2.0","method":"notifications/initialized"}'
  echo '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"write_cypher","arguments":{"statement":"CREATE (:Person {name: '"'"'Ada'"'"'})","tx_metadata":{"by":"seed-script","run":42}}}}'
} | RELATE_PREVIEW=value-provenance cargo run -p relate-cli -- serve --mcp stdio --database :memory: 2>/dev/null
```

Expected output, the write answering as usual (the metadata rides the
commit, not the reply):

```json
{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"{\"columns\":[],\"columnTypes\":[],\"rows\":[],\"stats\":{\"nodes_created\":1,…},\"notifications\":[],\"elapsed_ms\":0.12,\"parse_ms\":0.03,\"plan_ms\":0.05,\"execute_ms\":0.04,\"error\":null}"}],"isError":false}}
```
