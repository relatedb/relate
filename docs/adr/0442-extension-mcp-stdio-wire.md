---
type: adr
area: ecosystem
claim: 3c12dcdd1759b4111ac2900406d282a9a4c3b389
name: 0442-extension-mcp-stdio-wire
status: proposed
tags:
  - extensions
  - mcp
title: The extension mount speaks MCP stdio
REFERENCES:
  - "[[adr/0042-graph-memory-limit]]"
  - "[[adr/0429-cli-extension-registry]]"
  - "[[adr/0430-cli-extension-wire-protocol]]"
  - "[[adr/0431-cli-extension-distribution]]"
  - "[[adr/0447-extension-single-wire]]"
---

# ADR-0442: The extension mount speaks MCP stdio

Amended: 2026-10-29 (see [ADR-0447](./0447-extension-single-wire.md))

Later references: [ADR-0447](./0447-extension-single-wire.md) (2026-10-29)
amends §1 (the manifest `wire` field is retired — the mount speaks MCP
stdio only), §2's tool table (one table in `cypherlite_wire::mcp::tools`,
`update_schema` instead of `schema_write`, the bodies in
`cypherlite_mcp::tools`), and the note that the manifest's `protocol` is
wire v1's number: it now names the MCP revision. The tool semantics, the
caps, the capacity facts and the host lifecycle below stand as written.

## Context

[ADR-0430](./0430-cli-extension-wire-protocol.md) fixed wire v1 for command-kind extensions: the host spawns the
child over std pipes, hands it one parsed JSON envelope on stdin, and
reads NDJSON records (`hello`, `batch`, `rows`, `index`, `summary`,
`error`) from its stdout. The child walks a source; the host alone owns
the store. That wire made the first-party importers (`fs-to-graph`,
`git-to-graph`) ordinary installed packages ([ADR-0431](./0431-cli-extension-distribution.md) §11) and proved the
process boundary, the caps, and the exit-code mapping.

Wire v1 has two limits the extension programme now runs into:

- it is one-way in practice: the child emits writes, and nothing flows
  back except the three caps and a kill. An extension cannot read the
  store it is filling — no dedup check, no query-driven walk, no
  verification pass.
- it is a bespoke protocol. Every extension author re-learns it, and an
  agent or framework that already speaks MCP cannot mount as an extension
  without an adapter.

The design discussion on PR #511 (the extension-system PR) proposed
letting the same mount speak MCP instead: the child becomes a minimal MCP
client over the same two pipes, the host a minimal MCP server. Oskar's
constraint is unchanged from [ADR-0429](./0429-cli-extension-registry.md)/[ADR-0430](./0430-cli-extension-wire-protocol.md): an extension must stay
installable through `cypherlite extension install` and the signed
distribution channel ([ADR-0431](./0431-cli-extension-distribution.md)). Distribution is orthogonal — an MCP
child is packaged, signed, discovered and installed exactly like a v1
child, because the mount, not the wire, owns installation.

This ADR records the decision. It adds no external dependency: the
JSON-RPC 2.0 framing is hand-rolled over the in-tree `serde_json`, and the
pipes and the caps are the existing ones. The spike that proved the shape
is §5's baseline: every item there is an implementation gap between this
decision and the code landed beside it, not a different decision.

## Decision

### 1. Dual-protocol mount, MCP stdio as the target wire

`cypherlite run <name>` keeps one mount for both wires. A command
manifest may declare `"wire": "v1" | "mcp"`; absent means `v1`, so every
package written before this ADR behaves exactly as before. The host still
validates the child's first line against the declared wire: `hello` for
v1, an `initialize` request for MCP. The wire choice is declared rather
than sniffed because the two wires disagree about the child's stdin — v1
children (including every `sh` fixture, e.g. `cat > /dev/null`) read
stdin to end-of-input before emitting a byte, while an MCP child needs
that pipe open for the host's responses. No first line can arrive from a
v1 child until the host closes stdin, and an MCP child cannot answer on a
closed one, so the host must know the framing before it writes the
envelope. This is the one deviation from the "detect by first line"
sketch in PR #511: detection is retained as first-line *validation*, and
the choice is a typed manifest field. `/tmp/pr511-mcp` is the spike
branch; the deviation is recorded here (§5.1) rather than hidden.

The transport is unchanged: std pipes, no shell, no argv, the same
one-line request envelope first (its resolved flag values), stderr
drained and capped. The traffic is inverted: after the envelope the child
writes JSON-RPC 2.0 requests on stdout and reads the host's responses
from stdin.

### 2. The five tools

The extension-facing tool set is exactly these five. Result content is
the tool's JSON object carried as MCP `content[0].text`.

| Tool | Params | Result content | Notes |
| --- | --- | --- | --- |
| `write_cypher` | `{statement: string, parameters: object}` | `{counts: {...}, storeRemainingBytes}` | `parameters` is passed to `query_with_params`; a `rows` array feeds the statement's `$rows`, exactly what the v1 `batch`+`rows` records feed. Counters are the shared wire vocabulary (`nodes_created`, …). `storeRemainingBytes` is the store's budget after the write (§2's capacity facts) |
| `read_cypher` | `{statement, parameters}` | `{columns: string[], rows: array}` | New capability: v1 cannot read. Classified through `EXPLAIN`'s plan-derived query type; a non-read statement is refused with `Only MATCH queries are allowed for read-query` |
| `update_schema` | `{statement: string}` | `{created: string[]}` | DDL (indexes, constraints), classified through `EXPLAIN`'s plan type: a data write or a read is refused with `Only DDL queries are allowed for update-schema`. `created` is the set difference of `SHOW INDEXES`/`SHOW CONSTRAINTS` names across the call — the v1 `index` record's replacement |
| `batch_cypher` | `{statements: [{statement, parameters}]}` | `{results: [counts…], storeRemainingBytes}` | Executes all in ONE transaction (one explicit transaction per call under `per-batch`; the session's one transaction under `whole-import`). A failing statement is atomic per call: `per-batch` rolls the call back and answers `-32602`; `whole-import` ends the run — the session's one transaction cannot roll a single call back, so the host never answers a continuation it cannot keep |
| `report_output` | `{text: string, starters?: string[]}` | `{}` | Replaces the v1 `summary` record; the host stores text + starters and prints/hands them off exactly as `print_summary`/the REPL handoff does |

`tools/list` answers these five with `readOnlyHint: true` on
`read_cypher` and `report_output`, `destructiveHint: false` on all five.

The handshake is MCP's, not merely MCP-shaped: the client's `initialize`
names its revision (`protocolVersion`), its capabilities and its identity;
the result carries the revision the server serves
(`MCP_PROTOCOL_VERSION`), the server's capabilities, its identity, and the
mount's own facts; the client then confirms with the id-less
`notifications/initialized` before its first request.

```json
{
  "protocolVersion": "2025-06-18",
  "capabilities": {"tools": {"listChanged": false}},
  "serverInfo": {"name": "cypherlite", "version": "…"},
  "freshStoreRequired": false,
  "commitPolicy": "whole-import",
  "caps": {
    "maxStatementBytes": 8388608,
    "maxRowsPerStatement": 1000,
    "maxStatements": 4096,
    "maxStreamBytes": 134217728,
    "maxRequestBytes": 50332672
  },
  "capacity": {
    "storeLimitBytes": 18446744073709551615,
    "storeUsedBytes": 258,
    "storeRemainingBytes": 18446744073709551357
  }
}
```

`protocolVersion` is the published MCP revision this wire speaks; the
manifest's `protocol` field stays wire v1's number and is not the
handshake's version. The mount serves one revision, so it answers with
that one whatever the client named — MCP's rule for a version the server
does not speak — and a client that cannot speak it fails the handshake
loudly. Every cap value is a projection of the existing caps, not a second
table: the statement, rows, statements and stream caps are the wire kit's
constants, and `maxRequestBytes` is derived from the statement cap by the
kit's one framing derivation (`request_bytes_cap`: the statement bytes,
their JSON escaping allowance, and the envelope). The host's capped
request-line read runs under that same derivation, so a statement at the
published statement cap always fits one request line instead of dying as
an oversized record.

The record cap also bounds the *response* direction: a `tools/call` result
whose response line would exceed it is answered with the typed
`the tools/call result would exceed the <n>-byte response cap` refusal
instead of a line the child's capped response read would reject as
malformed. Refusing keeps the failure legible (the child may narrow its
read and continue) and is the one policy that holds for reads of unknown
size.

The handshake and the writing results also carry the host-owned store's
**capacity** — [ADR-0042](./0042-graph-memory-limit.md)'s live-graph accounting, read through
`Database::memory_usage`, never estimated on the wire:

| Field | Published in | Meaning |
| --- | --- | --- |
| `storeLimitBytes` | `initialize`, as `capacity` | The configured live-graph + index cap. `u64::MAX` is unlimited (the engine's zero-cap spelling, published as a bound) |
| `storeUsedBytes` | `initialize`, as `capacity` | The store's live logical bytes at the handshake |
| `storeRemainingBytes` | `initialize`'s `capacity`, and every `write_cypher` / `batch_cypher` result | `storeLimitBytes - storeUsedBytes`, saturated at zero, read from the host's `Database` after the write |

All three are raw-byte `u64`s, and `storeUsedBytes == storeLimitBytes -
storeRemainingBytes` holds in both cases, so a child subtracting two
results learns exactly what its own calls cost between them — no extra
round-trip, no second convention for "no cap". The numbers are the
store's committed state: `per-batch` moves them per call, while
`whole-import` holds the session's writes in one transaction until the
session's commit, so they stay at the handshake's reading for the whole
session; a child that needs per-call feedback declares `per-batch`.

### 3. Host lifecycle: store, framing, caps, checkpoint

The host owns everything it owned under v1:

- **Fresh store** — `requires-fresh-store` is checked before the child
  spawns, and the same fact is published in the initialize result.
- **Commit policy** — the manifest's policy frames the session through
  the shared write layer: `per-batch` commits each writing call,
  `whole-import` opens one transaction around the whole session and
  closes it at the session's end. `batch_cypher` is one transaction
  within that framing: under `per-batch` its own transaction rolls the
  call back on a failure and the refusal is answered, while under
  `whole-import` a failure ends the run, because the session's one
  transaction cannot roll a single call back and a per-call answer would
  promise a continuation the framing cannot keep. There is no begin/end
  transaction tool: external transaction control is not exposed.
- **Caps, re-anchored per call** — the v1 numbers bound every
  `tools/call` payload before it reaches the engine: the statement byte
  cap, the rows-per-statement cap, the statements-per-call cap, the
  total stream cap, and the per-request framing cap derived from the
  statement cap plus its envelope allowance. Per-call caps are
  JSON-RPC error responses with pinned wording; the per-request and
  stream caps are fatal (the host kills and reaps the child, v1's
  refusal policy).
- **The response direction is bounded too** — the child reads every host
  response under the wire kit's record cap, so a tool result that would
  not fit one line is refused as a per-call error instead of being
  written for the child to reject as malformed. The caps bind both
  pipes, not only the child's payloads.
- **Requests are answered; notifications are not** — the host answers
  messages that carry an `id`. A message without one is a JSON-RPC
  notification, even when its method is `tools/call`: it is dropped
  before dispatch, so nothing runs and nothing is written back. A child
  that waits for such a reply waits for an answer that cannot come.
- **Exit classification inside the frame** — the child's exit is
  classified at the request stream's end, *inside* the framed import: a
  child that streams cleanly and then exits non-zero fails the run
  before a `whole-import` commit, so a failed run never leaves a
  committed store behind.
- **Summary and checkpoint** — `report_output` renders through the same
  `print_summary` (sanitized text, starters indented two spaces), and the
  run ends with the same interactive handoff and `final_checkpoint`.

Refusals are answers, never silences: a malformed line is a `-32700`
parse error, an unknown method `-32601`, and an unknown tool, a bad
payload, a cap, a read or DDL guard, or a failed statement `-32602` with
the engine's own text where the engine spoke. A session whose first
request is not `initialize`, or which ends without one, is a fatal host
error (exit 1). A message without an id is a notification; only the
`notifications/*` family may omit the id — a `tools/call` without one is a
fatal host error (exit 1, `McpError::RequestWithoutId`: `the MCP request
'tools/call' carries no id; only notifications may omit it`), because a
child waiting for an answer that will never be written would hang the
session instead of failing it.

### 4. The child-side kit

`cypherlite_wire::mcp::McpChild` is the one home both sides share: the
MCP revision, the five tool-name constants with their pinned rejection
texts, the `tools/list` result, the strict JSON-RPC 2.0 record types, and
the client kit (`start` reads the envelope, performs the handshake —
`initialize` with the revision, then `notifications/initialized` — and
fails on a foreign revision; `call` sends one `tools/call`;
`report_output` is the summary convenience). A first-party MCP child is a
`main` that links the kit; a third-party child may print the same JSON-RPC
lines in any language.

## Alternatives considered

- **Keep the bespoke v1 wire only.** Rejected for the target state: it
  cannot read the store it writes, and it forces every agent/framework
  extension through a bespoke adapter. It stays supported as the
  migration wire; the first-party importers do not change.
- **Depend on an MCP SDK crate.** Rejected (AGENTS.md R6 and the brief):
  the five-record JSON-RPC surface is small, `serde_json` is already in
  tree, and an SDK would add a heavy transitive dependency to both the
  host and every child. Hand-rolling keeps the wire printable and the
  child dependency-free.
- **Child as MCP server, host as client.** Rejected: the host must own
 the `Database` ([ADR-0430](./0430-cli-extension-wire-protocol.md)'s core decision). A child server would either
  own the store or proxy every call, and the host could not enforce caps,
  commit policy, the checkout, or the fresh-store check around calls it
  does not drive.
- **External transaction control (begin/end tools).** Rejected: the
  commit policy is a manifest property applied host-side, exactly as v1
  frames batches. A child-chosen transaction boundary would let a package
  override the safety policy the manifest declared, and would leak
  transaction state into a tool vocabulary the host must then police.
- **First-line detection without a manifest field.** Rejected as
  unimplementable with unchanged v1 children: v1 reads stdin to EOF
  before its first record, MCP needs stdin open for responses, and the
  host cannot both close and keep it. The wire is declared and the first
  line is validated against the declaration.

## Consequences

- Extensions become agent-native: an MCP client (an agent, a framework,
  a script) can mount as `cypherlite run <name>` with the five tools, and
  a child can read the graph it is building. The handshake is MCP's
  handshake (revision, capabilities, `notifications/initialized`), so a
  stock client's protocol layer can complete it; the one extension-specific
  step remains the request envelope's single line on stdin before the
  handshake, which an adapter or the kit writes.
- The mount gains a real read capability (`read_cypher`) and schema
  introspection across a call (`update_schema`'s `created`), which wire v1
  never had.
- An importer can size its work against the store's budget: the handshake
  publishes the capacity triple and every writing result repeats the
  remaining bytes, so a child counts its own usage down without an extra
  round-trip. The feedback is per call under `per-batch` and per session
  under `whole-import`, whose transaction the store does not hold until
  commit.
- `sh` fixtures and every pre-existing package keep the v1 path
  unchanged; a shell child that wants MCP needs the JSON-RPC loop (the
  Rust fixture is the first-party proof; a shim can wrap one).
- P2 callables (LABS-468) can ride the same server: a `tools/call`
  dispatcher is already the shape a callable surface needs.
- The manifest grows one optional field (`wire`), defaulting to `v1`;
  the strict decode refuses any other value.
- No new dependency: std pipes + `serde_json`. The caps and the protocol
  number are the existing wire-kit constants.
- The spike's e2e covers both commit policies, the persisted store, the
  rendered summary, and the silent-child refusal; the unit suite pins
  every refusal's exact wording over scripted pipes.
