---
type: adr
area: ecosystem
claim: da94d034a1878fd69d1c5d9f58ea6ad28ef71925
name: 0447-extension-single-wire
status: accepted
status_note: 2026-10-29
tags:
  - extensions
  - mcp
title: The extension mount speaks MCP stdio only
REFERENCES:
  - "[[adr/0430-cli-extension-wire-protocol]]"
  - "[[adr/0442-extension-mcp-stdio-wire]]"
SUPERSEDES:
  - "[[adr/0430-cli-extension-wire-protocol]]"
---

# ADR-0447: The extension mount speaks MCP stdio only

## Context

[ADR-0430](./0430-cli-extension-wire-protocol.md) fixed wire v1 for
command-kind extensions: the host spawned the child over std pipes, handed
it one parsed JSON envelope on stdin, and read NDJSON records (`hello`,
`batch`, `rows`, `index`, `summary`, `error`) from its stdout.
[ADR-0442](./0442-extension-mcp-stdio-wire.md) added an MCP stdio mount
beside it, chosen by a manifest `wire` field, and the first-party
importers stayed on v1.

That left three shapes the audit and the review rounds flagged:

- **A vestigial protocol field.** The manifest's `protocol` was wire v1's
  number (`1`) even for a package that speaks MCP, while the handshake's
  `protocolVersion` names the MCP revision. Two protocol facts for one
  wire, and the documented `protocol: 1` had no meaning for an MCP
  author.
- **Two wires, two everything.** Two record readers, two sets of refusal
  texts, two sets of test lanes, and two ways to say the same thing (a
  `batch` + `rows` pair against `write_cypher` with a `rows` parameter).
  Nothing but the first-party importers still spoke v1, and every one of
  them can speak MCP.
- **Two tool tables.** The extension mount's five tools and the product
  MCP server's three were spelled, described and implemented separately —
  different names for the same operation (`schema_write` /
  `write-cypher`), different argument names (`statement` / `query`),
  parallel implementations of the same gating, and parallel refusal
  strings.

## Decision

### 1. One wire: MCP stdio

The mount speaks MCP stdio and nothing else. Wire v1 is retired: the
`cypherlite-wire` child kit (`wire::child`), the CLI's v1 record reader,
its stream driver, and the manifest's `wire` field are gone. The v1-only
test lanes are ported onto the mount's own refusals — the JSON-RPC decode,
the handshake ordering, the per-call caps and every pinned text live in
`extensions::mcp::tests` — rather than deleted, and the shipped `sh`
fixtures speak the bash MCP kit instead of printing NDJSON records.

A child's failure is a non-zero exit with its text on stderr; the host
renders it under its one `Error:` line. There is no in-band `error`
record left to carry it.

### 2. `protocol` names the MCP revision

The manifest's `protocol` is the MCP revision the package speaks
(`2025-06-18`) — the same fact the handshake's `protocolVersion` carries,
so one value, one meaning. A manifest naming another revision is the
usage refusal (exit 2) that names both, exactly as the retired `hello`
mismatch did. The first-party channel index's `protocol` column carries
the same string.

### 3. One tool table, one implementation per tool

`cypherlite_wire::mcp::tools` owns **the table**: every tool's name,
title, description, input schema, capability annotations, and the mounts
that serve it.

| Tool | Mounts | Params | Result |
| --- | --- | --- | --- |
| `get_schema` | product | `{sample_size?}` | the schema map |
| `update_schema` | extension | `{statement}` | `{created: string[]}` |
| `read_cypher` | both | `{statement, parameters?}` | product: the wire envelope; extension: `{columns, rows}` |
| `write_cypher` | both | `{statement, parameters?}` | extension: `{counts, storeRemainingBytes}` |
| `batch_cypher` | extension | `{statements: [{statement, parameters?}]}` | `{results, storeRemainingBytes}` |
| `report_output` | extension | `{text, starters?}` | `{}` |

Every name is `<verb>_<object>`: the graph tools take the object `cypher`,
the schema tools `schema`, and `report_output` the run's summary.

`cypherlite_mcp::tools` owns **the bodies**: `Tools` runs one call
against a host-owned connection — the `EXPLAIN`-plan gating per access
class, the per-call caps, and the typed refusal vocabulary
(`ToolError`), whose guard texts (`READ_ONLY_REJECTION` and siblings) live
in the wire kit so the child kit and both surfaces read one copy. The
product server applies its two policies on top (its namespace prefix and
its read-only gate); the extension mount serves its rows as they are.

Report and lifecycle facts stay with the surface that has them:
`report_output`, the store's capacity fields and the commit framing are
the extension mount's; the namespace and the read-only gate are the
product server's.

### 4. Catalog DDL is one class, tools are one DDL gate

A graph-type command (`CREATE GRAPH TYPE`, `ALTER CURRENT GRAPH TYPE`) is
catalog DDL, so the planner classifies it as `QueryType::Schema` beside
index and constraint DDL — not as a data write. That is what lets the one
DDL tool (`update_schema`) admit a graph type and what lets the read tool
refuse it; before, the read tool's gate admitted a graph-type command and
the DDL tool refused it. `ALTER DATABASE` stays a write (administration, not
a schema definition).

### 5. The first-party children speak it

`fs-to-graph` and `git-to-graph` emit through `cypherlite_wire::mcp::McpChild`:
`initialize`, `update_schema` for their indexes, one `write_cypher` per
chunk of rows, and `report_output` for the summary. Their graphs, counts
and starters are unchanged; only the wire under them is.

## Alternatives considered

- **Keep both wires ([ADR-0442](./0442-extension-mcp-stdio-wire.md)'s dual mount).** Rejected: nothing but the
  first-party importers spoke v1 and all of them can speak MCP, so the
  dual mount bought no user anything while costing a second reader, a
  second refusal vocabulary, a second set of test lanes, and the
  vestigial `protocol` field that started this ADR.
- **Keep the tool bodies in the CLI host.** Rejected: the product MCP
  server needs the same bodies, so this is the second implementation the
  review round already caught — two gatings, two refusal vocabularies,
  and two places to fix a tool bug.
- **Put the shared bodies in the wire kit behind a feature.** Rejected:
 the kit is what a *child* links and must stay engine-free ([ADR-0430](./0430-cli-extension-wire-protocol.md) §6);
  an engine-side tool implementation belongs to the surface that serves
  agents, which the CLI may depend on (R3).
- **Detect the wire from the first line.** Rejected, unchanged from
 [ADR-0442](./0442-extension-mcp-stdio-wire.md) §1: unimplementable while a v1 child reads stdin to
  end-of-input before its first record — and moot once one wire exists.
- **Keep the product server's hyphenated tool names as aliases.** Rejected:
  two spellings for one tool is the parallel vocabulary this ADR removes,
  and the server's clients are agents that read `tools/list` on connect.
  The rename is documented as a break (`docs/api/mcp/spec.md`).

## Consequences

- A package declares `protocol: "2025-06-18"` and no `wire`; a package
  written against the retired v1 wire no longer loads (the strict manifest
  decode names the unknown field), which is the intended migration: the
  first-party packages ship the new shape.
- The product MCP server's tools are `get_schema` / `read_cypher` /
  `write_cypher` (were `get-schema` / `read-cypher` / `write-cypher`), and
  their arguments are `statement` / `parameters` (were `query` / `params`).
  Existing clients must rename; the results keep the server's envelope.
- The extension mount gains a real read tool and a DDL tool named
  `update_schema`; a child's `write_cypher` of a pure read is refused
  (`Only write queries are allowed for write-query`), which the importers
  never trip: their statements all write.
- `cypherlite-cli`'s `extensions` feature carries `cypherlite-mcp`
  (and the engine's parquet feature that crate implies): one tool
  implementation is worth the build weight, and the `minimal` bundle is
  untouched.
- The channel index's `protocol` column is a string; the publisher, its
  bats fixtures and the CLI's fixture index move together in one change.
- The shipped examples speak the mount directly and walk a real corpus:
  the public Stack Exchange API's newest questions for a tag, loaded with
  the Stack Overflow corpus's graph type and write shape — a bash
  example over `curl`/`jq` (its `mcp-client.sh` kit inlined, because a
  package is one executable), an SDK v2 Python client (`mode="legacy"`:
  the mount serves the handshake-era protocol, so the SDK must not open with
  its `server/discover` probe) and an SDK v2 TypeScript client, each with
  one small transport over the process's own stdio. They need network
  access, so CI gates them for syntax only
  (`scripts/check-extension-examples.sh`); the executed, offline coverage of
  the mount is the CLI's fixture suites (`e2e_extension_fixtures.rs`,
  `e2e_extension_mcp.rs`, `e2e_fs_to_graph.rs`, `e2e_git_to_graph.rs`), and
  the network-dependent example e2e lane is gone.
- [ADR-0430](./0430-cli-extension-wire-protocol.md)'s records, caps and commit vocabulary survive in substance:
  the numbers, the `whole-import` / `per-batch` framing and the
  no-argv, no-store envelope are unchanged — only the record stream and
  the tool surface under them moved.
