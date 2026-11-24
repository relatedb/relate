# MCP surface — what it is for

The MCP surface lets LLM agents run Cypher against a Relate store
through the Model Context Protocol, the way the reference
`mcp-neo4j-cypher` server does against Neo4j (ADR-0232 §4). It is the
second network surface after Bolt (ADR-0057): where Bolt speaks to
drivers, MCP speaks to agent hosts — Claude Desktop, Claude Code, and
other MCP clients — as either a spawned child process (stdio) or a network
listener (streamable HTTP, the `relate serve --mcp PORT` shape).

The surface deliberately exposes exactly the reference server's three
tools, under Relate names — the explicit correspondence to the
reference's `get_neo4j_schema` / `read_neo4j_cypher` /
`write_neo4j_cypher` is in the spec's Tool-name mapping section —
`get_schema` (the store's labels,
relationship types, and sampled property types), `read_cypher` (read-only
statements; writes are rejected), and `write_cypher` (writes, answered
with the mutation counters). An agent discovers the graph with
`get_schema`, then explores and edits it with the two cypher tools.

The protocol is hand-rolled JSON-RPC 2.0 over `serde_json` — the tool
surface is small enough that an external MCP SDK would only add dependency
weight (ADR-0232 §4). Auth is the shared generated-token basic-auth story
(ADR-0232 §1): skipped on stdio (no trust boundary), enforced as RFC 7617
`Authorization: Basic` on the HTTP transport, with one credential story
shared across Bolt, HTTP, and MCP.

Session semantics on HTTP are stateless per request — the reference server
runs the same way — so any HTTP client that can POST JSON can drive the
server; no session handshake bookkeeping is required.

Plan trees an agent reads through this surface describe a seek-started `MATCH` with display-only fused children (ADR-0622); they are narration, never executed operators, and carry no per-step rows, time or memory.
A write may carry the **why** beside the what: `write_cypher` (and the
extension mount's `batch_cypher`) take the optional `tx_metadata` object
(ADR-0610, the spelling the ADR records for the driver channel) — the
transaction's audit map, serialized into the commit's transaction record on
a store opened with `RELATE_PREVIEW=value-provenance`, where the provenance
queries read who set a value and why. Without the opt-in a call carrying it
fails loudly with the authored preview error rather than silently losing the
audit fact.
