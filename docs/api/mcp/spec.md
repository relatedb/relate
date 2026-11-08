# MCP surface spec — MCP on `relate serve`

The MCP (Model Context Protocol) server surface for Relate (ADR-0232
§4). Hand-rolled JSON-RPC 2.0 over `serde_json` — no external MCP SDK. The
public Rust API is the `relate-mcp` crate, and MCP is served by the
one product binary, `relate serve --mcp PORT|stdio` (ADR-0368): the
standalone `relate-mcp` binary is retired (LABS-381), and serve hosts
one store over streamable HTTP (`--mcp PORT`) or stdio (`--mcp stdio`).

## Rust API (`relate-mcp`)

| Item | Signature | Contract |
| --- | --- | --- |
| `McpServerConfig` | struct | Server configuration: `transport`, `auth`, `namespace`, `read_only`, `schema_sample_size: Option<u64>` (`None` = full scan; the wire/CLI `-1` converts via `sample_size_at_edge`), `max_connections`, `connection_cap: Option<Arc<relate_serve::ConnectionCap>>` (a process-wide cap shared with the other listeners; `None` builds one from `max_connections`), `read_timeout`, `header_deadline`, `body_deadline`. `Default` = stdio, no auth, no namespace, writes enabled, sample size 1000, a 128-connection cap, a 30 s read timeout, a 10 s absolute header deadline, and a 60 s absolute body deadline (ADR-0451; `0` / `Duration::ZERO` disables any limit). The HTTP listener enforces the cap accept-then-close and retries accept errors (ADR-0110's policy); a read stalled past `read_timeout` drops the connection, and a header or body unfinished at its deadline is dropped there — the per-read timeout is re-armed to the time remaining, so trickled bytes cannot hold the worker. `relate serve` shares one process-wide cap across its listeners and passes the resolved `--header-deadline-secs` / `--body-deadline-secs` values here. `McpServerConfig::validate()` (and the `serve*` entry points) applies the one-day deadline bound to a directly constructed config, returning a typed error rather than exiting the embedder's process. |
| `Transport` | enum | `Stdio` or `Http { host, port }`. |
| `McpServer` | `McpServer::new(db: Database, config: McpServerConfig)` | The dispatch core. `handle_message(&self, body: &str) -> Option<String>` answers one JSON-RPC message (`None` for notifications). |
| `serve` | `fn serve(db: Database, config: McpServerConfig) -> Result<(), ServeError>` | Blocking entry point a host (the CLI's `serve --mcp PORT`, `serve --mcp stdio`) calls. Runs until stdin EOF (stdio) or process exit (HTTP). |
| `serve_stdio` | `fn serve_stdio(server: &McpServer, reader: impl BufRead, writer: impl Write) -> Result<(), ServeError>` | The stdio framing loop, generic for pipes/tests. |
| `serve_http_on` | `fn serve_http_on(server: McpServer, listener: TcpListener) -> Result<(), ServeError>` | The HTTP listener (the shared `serve_capped` loop: one thread per connection, connection cap, accept errors retried). Validates the server config on entry and returns `ServeError::InvalidConfig` for an out-of-range deadline; on success it diverges — there is no shutdown condition, the process owns the listener's lifetime. `serve`/`serve_shared` bind for the standalone binary so a taken port is a loud startup error before the loop starts. |
| `sample_size_at_edge` | `fn sample_size_at_edge(value: i64) -> Result<Option<u64>, String>` | The wire/CLI edge's sample-size convention: `-1` = full scan (`None`), other negatives are an error, non-negative is `Some`. |
| `handle_http` | `fn handle_http(server: &McpServer, method: &str, authorization: Option<&str>, body: &str) -> HttpResponse` | The streamable-HTTP handler, socket-free for tests. This direct-caller entry always enforces the RFC 7617 credential and answers a failure with `401` plus the challenge. The accepted-connection path runs the admission gate first and calls the private post-auth answer core, so the credential is never checked twice. |
| `HttpResponse` | struct | `status: u16`, `headers: Vec<(String, String)>`, `body: String`. |
| `ServeError` | enum | `Io(std::io::Error)` or `InvalidConfig(String)` (a config value rejected at an entry point). |

## Protocol

JSON-RPC 2.0, one message per line on stdio; one message per `POST` body
on HTTP. Implemented methods:

- `initialize` → `{protocolVersion, capabilities: {tools: {}}, serverInfo:
  {name: "relate-mcp", version}}`. The client's `protocolVersion` is
  echoed when present; otherwise `2024-11-05`.
- `notifications/initialized` (and any `notifications/*`) → no response.
- `ping` → `{}`.
- `tools/list` → the tool array (below).
- `tools/call` → `{content: [{type: "text", text}], isError}`; tool
  failures are `isError: true` results, not protocol errors. Engine errors
  cross the ADR-0260 trust boundary: internal-class detail (the whole
  class — engine-internal, IO, storage and locked failures) is genericized
  to `Error: internal error`, the engine's one `INTERNAL_MESSAGE`
  (OPLANE_REQ-00340359); it must not reach host tool-call logs.
  Client-facing errors pass through verbatim.

Errors: unknown method → `-32601`; malformed `tools/call` params →
`-32602`. Request bodies decode through the shared strict decode
(`relate_wire::mcp::decode_request`, `deny_unknown_fields` plus the
`2.0` version check) — the one decoder both MCP surfaces run: a body that
is not JSON, omits a required member (`jsonrpc`, `method`), or carries an
unknown member is refused with `-32700` and `id: null`; a foreign
`jsonrpc` version with `-32600` and `id: null`. Both refusals carry the
extension mount's pinned wording (`malformed JSON-RPC message: <detail>`,
`unsupported JSON-RPC version '<version>'`).

### Tools

Names take an optional `<namespace>-` prefix when the server is configured
with a namespace.

The three tools and their input schemas are one table, shared with the
extension mount (`relate_wire::mcp::TOOL_TABLE`), and their bodies are
one implementation (`relate_mcp::tools`): this server adds only its
namespace prefix and its read-only gate (ADR-0447 §3). The names and the
`{statement, parameters}` inputs are the extension mount's vocabulary, so
one table serves both MCP surfaces.

Request `params` accept plain JSON values (ADR-0425): an envelope-shaped
object — a client echoing a response value back — stays a user map whose
entries are reachable as map lookups; there is no typed-form acceptance
in the MCP parameter path.


| Tool | Input | Output (text content, JSON-serialized) |
| --- | --- | --- |
| `get_schema` | `{sample_size?: integer}` (default 1000, `-1` = full scan) | Schema map keyed by label: `{"Person": {"type": "node", "count": 2, "properties": {"name": {"type": "String"}}, "relationships": {"KNOWS": {"direction": "out", "labels": ["Person"], "properties": {"since": {"type": "Integer"}}}}}}`. Counts are exact; property types are inferred from the sampled scan and mixed types keep the first type seen. |
| `read_cypher` | `{statement: string, parameters?: object}` | The canonical wire envelope (`{"columns": ["name"], "columnTypes": ["STRING"], "rows": [{"name": "Ada"}, …], "stats": {…}, "notifications": […], "plan": {…}|null, "elapsed_ms": <f64>, "parse_ms": <f64>, "plan_ms": <f64>, "execute_ms": <f64>, "lex_parse_ms": <f64>, "rewrite_ms": <f64>, "lower_ms": <f64>, "result_generation_ms": <f64>, "plan_cache": "<outcome>", "error": null}`): rows as objects under `rows`, values in the ADR-0223 wire JSON; `columnTypes` is the engine-stamped per-column type name of ADR-0224, `elapsed_ms` the engine-reported statement elapsed of ADR-0221 with its ADR-0491 phase split beside it — `parse_ms`/`plan_ms`/`execute_ms`, the PARSE / PLAN / EXECUTE partition of the same window, non-negative milliseconds whose sum never exceeds `elapsed_ms` — the LABS-1120 fine sub-split (`lex_parse_ms`/`rewrite_ms`/`lower_ms` partitioning `parse_ms`; `result_generation_ms` inside `execute_ms`; the PARSE fine sub-windows `0` on a plan-cache hit (a hit still builds its result, so `result_generation_ms` is non-zero)) and the `plan_cache` outcome string explaining it — and `notifications` the always-present ADR-0363 native list — the classification `EXPLAIN`'s plan-time notifications (see below), empty when it raises none. The same result-metadata contract as the CLI/FFI envelope (cross-surface alignment). Only a pure read (plan-derived query type `r`) passes — an `OPTIONAL MATCH` read included: the pattern's optionality never changes the class, so `MATCH (a:Room) OPTIONAL MATCH (a)-[:NORTH]->(b) RETURN a.name, b.name` is served, its unmatched arm returning null (LABS-888; the `Only MATCH queries` refusal text below is the reference server's pinned wording, not a MATCH-only rule). Writes (`w`/`rw`) AND schema DDL (`s` — `CREATE/DROP INDEX`, `CREATE/DROP CONSTRAINT`) are rejected with `Only MATCH queries are allowed for read-query`. Hosts auto-approve `readOnlyHint` tools, so nothing that mutates the graph or the catalog passes this guard. |
| `write_cypher` | `{statement: string, parameters?: object}` | The same wire envelope with empty `columns`/`columnTypes`/`rows`; the mutation counters in the snake_case wire vocabulary sit under `stats` (`{"nodes_created": 1, …}`), with the same always-present `notifications`, `plan` (the `relate.plan/v1` document, ADR-0381 — always present, because every MCP call classifies through EXPLAIN first) and `elapsed_ms` with its ADR-0491 phase split (`parse_ms`/`plan_ms`/`execute_ms`). A read-only statement is rejected with `Only write queries are allowed for write-query`. Absent entirely under `--read-only` (either freshness), on `relate serve` (ADR-0306 — the fence follows the opened handle, so a read-only or following open does not list the tool). |

**Byte arrays (ADR-0327).** The engine's statement-scoped byte array enters only through Bolt or a Rust `Value::Bytes` parameter, never through this surface's own parameters. A result value holding one renders as a list of integers 0–255 (the `value_to_cypher` crossing) and its column stamps `BINARY`; byte arrays cannot be stored as properties.

Write detection mirrors the reference server: `EXPLAIN <statement>`
classifies the plan (ADR-0162 `query_type`) before execution.

That classification `EXPLAIN` is also the notification source (ADR-0363
decision 10): both cypher tools put its `notifications` list into the
envelope, so a `read_cypher`/`write_cypher` response carries the plan-time
hints the engine already computed — every operator- and statement-scoped
kind — without a second `EXPLAIN` and never a `PROFILE` (so the
PROFILE-only `EstimationMiss` is absent). The executed statement's own list
is not consulted. Entries use the ADR-0363 native shape: `code`,
`gql_status`, `severity`, `category`, `title`, `description`, `remedy` and,
on operator-scoped kinds, `operator_id`; the array is always present and
empty when the statement raises nothing, e.g. a bare `RETURN 1 AS n`, which
names no label, relationship type, property key or variable-length pattern.
A write's own target names are not read-side evidence (decision 13): a
`CREATE`/`MERGE` target pattern or a `SET`/`REMOVE` property target raises
no `Unknown*Warning`, while a read clause in the same write still can.
A statement that does raise one reports it in that same array: a `MATCH
(a:Person), (b:City) RETURN a, b` over a store holding a `:Person` and a
`:City` node reports `Neo.ClientNotification.Statement.CartesianProduct`
(`03N90`, `INFORMATION`, `PERFORMANCE`).

Output-shape divergence from the reference server, deliberate: the
reference returns a bare row array / bare counters object; Relate wraps
both in the canonical wire envelope so the MCP surface carries the same
result metadata (column types, engine-reported elapsed and its ADR-0491
phase split) as every other surface. The reference's row objects and
counter vocabulary are unchanged inside the envelope.

### Tool-name mapping

Relate names its tools after itself rather than Neo4j; every other
part of each tool's contract — inputs, outputs, and the verbatim read/write
rejection messages — is the reference server's. The correspondence is:

| Relate name | Reference (`mcp-neo4j-cypher`) name |
| --- | --- |
| `get_schema` | `get_neo4j_schema` |
| `read_cypher` | `read_neo4j_cypher` |
| `write_cypher` | `write_neo4j_cypher` |

The names are the shared tool table's snake_case vocabulary (ADR-0447
§3); contract parity is otherwise exact.

## Auth (ADR-0232 §1)

- stdio: no challenge — the parent process spawned the server, so there is
  no trust boundary to cross.
- HTTP: every request needs the RFC 7617 `Authorization: Basic` header
  validated by the shared `relate-auth` crate. A missing or wrong
  credential gets `401` with `WWW-Authenticate: Basic
  realm="relate"` (the shared `relate_auth::REALM`, one realm for
  every HTTP surface). `--auth none` / `AuthConfig::None` disables the
  check. The check runs on the parsed headers **before the body is
  buffered**, and a failed check waits
  `relate_http::http::FAILED_AUTH_DELAY` (1 s, ADR-0139's failed-HELLO
  policy) before answering, bounding online guessing to one attempt per
  connection per second.

Browser-request guards (CSRF / DNS rebinding, MCP's Origin-validation
SHOULD) come from the shared pre-body pipeline
`relate_http::http::read_admitted_request` (its `browser_guard` gate),
identical to the Query API surface, and apply to every state-changing
method — this transport answers only `POST`, but the guard is the shared
policy, not a per-route check (ADR-0455). A `POST` without
`Content-Type: application/json` is `415` (the content-type rule is
POST-specific), and a state-changing request with a non-loopback `Origin`
(only `http(s)://localhost[:port]`, `127.0.0.1[:port]`, `[::1][:port]`
pass — ADR-0266's loopback set) is `403`. Both run before auth and before
the body read. (`GET`, `HEAD`, `OPTIONS` and `TRACE` are RFC 9110 safe
methods and carry no state-change guard; on the Query API surface `GET /`
additionally serves the public discovery document.)

Divergence from the Query API surface, deliberate: the HTTP surface is
browser-facing and adds a configurable Origin allowlist (`--origin`,
defaulting to loopback plus the hosted Neo4j Browser and Console origins)
plus CORS echo and
`OPTIONS` preflights; this transport is agent-facing and keeps the fixed
loopback-only guard with no CORS.

The HTTP wire layer is the shared minimal HTTP/1.1 stack of
`relate-http`'s `http` module (request-head parsing via `httparse`,
ADR-0234) — one implementation for both HTTP
surfaces, so the header/body caps and malformed-request handling are
identical. The same shared pre-body gate (`gated_head`) owns the **version
floor**: a request line that is not HTTP/1.1 (HTTP/1.0, say) is refused
with a parsed **400** rejection on both HTTP surfaces, before any gate or
body read, with no per-surface or per-call-site switch — so this transport
never answers a version it does not speak, and never offers
`Transfer-Encoding: chunked` (ADR-0386 §8).

## Streamable HTTP semantics

Stateless per request: no `Mcp-Session-Id` is issued, each `POST` carries
a complete JSON-RPC message, and the server holds no per-client state.
Requests answer `200 application/json`; notifications answer
`202 Accepted` with an empty body; non-POST methods answer `405`. Legacy
SSE is not implemented.

## Serving: `relate serve` (ADR-0368)

The standalone `relate-mcp` binary is retired (LABS-381, ADR-0368).
MCP is served by the one product binary:

```
relate serve --database PATH [--mcp PORT|stdio] [--host HOST] …
```

The MCP-relevant flags of `serve`'s grammar:

| Flag | Default | Meaning |
| --- | --- | --- |
| `-d`, `--database PATH` | `:memory:` | Store path or URI to serve |
| `--mcp PORT\|stdio` | off | MCP transport: `stdio` runs the stdio transport in the foreground (newline-delimited JSON-RPC on stdin/stdout) until the host closes stdin — no auth challenge, exit 0 at EOF; `PORT` runs the streamable-HTTP transport. The transport choice is once-only: combining the two is a usage error |
| `--host HOST` | `127.0.0.1` | HTTP bind host (plaintext HTTP only — keep the loopback default unless a TLS-terminating proxy fronts it) |
| `--auth VALUE` | generated token | HTTP transport only. `user:password` credential, or `none` to disable; the generated token is printed on stderr and persisted as a `<dir>.auth-token` sibling. The stdio transport skips auth entirely: no challenge, no token file, no auth banner — an explicit `--auth` on a stdio-only run is a loud usage error |
| `--read-only` | off | Open an existing store read-only (ADR-0306, ADR-0136): no writer lease, a missing store is not created, and the `write_cypher` tool is not listed |
| `--read-only=live` | off | Tail the WAL of a native directory store and serve the writer's commits (ADR-0137, the ADR-0480 spelling of the former `--follow`); refused for `:memory:` and `.rl` |

See the [CLI spec's serve flag table](../cli/spec.md) for the full serve
grammar (`--http PORT`, `--bolt PORT`, `--max-connections`, …). Flags of
the retired binary that serve does not carry — `--transport
stdio|http` (the transport is now the `--mcp` value), `--namespace NAME`
and `--sample-size N` (tool-name prefixing and the default `get_schema`
sample size) — have no serve spelling: they are library-level only
(`McpServerConfig`, `Transport`, `sample_size_at_edge` above).

Environment: `RELATE_AUTH` (`user:password`), honored when `--auth` is
absent (HTTP transport only).

Copy-paste (HTTP with a fixed credential):

```bash
cargo run -p relate-cli -- serve --database /tmp/graph.db --mcp 8000 --auth neo4j:s3cret
curl -u neo4j:s3cret -X POST localhost:8000 -d '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}'
```

Copy-paste (stdio, the shape an MCP host spawns — Claude Desktop / Claude
Code /Cursor style):

```bash
relate serve --mcp stdio --database /tmp/graph.db
```

Copy-paste (following a store another process is writing, ADR-0306 —
read-only beside the writer, `write_cypher` not listed):

```bash
relate serve --database ./mydata --mcp stdio --read-only=live
```

The startup banner (including a generated token) goes to **stderr** so
stdout stays a clean JSON-RPC channel on the stdio transport.

## Running in Docker

The shared server image (ADR-0128, unified runtime per ADR-0249) serves
MCP on its streamable-HTTP transport from the same `relate serve`
process as Bolt and the HTTP Query API: `make docker && make docker-run`
publishes MCP on **7475** (Bolt 7687, HTTP 7474). Auth is the one shared
credential of ADR-0232 §1 — `RELATE_AUTH`, `--auth`, or the generated
token printed in `docker logs`; the one token unlocks all three surfaces.
The container binds `--host 0.0.0.0` (plaintext; serve warns loudly per
ADR-0248 — front with a TLS-terminating proxy for anything beyond
dev/test). Details, seeding, and the fleet mode:
[Bolt spec — Docker](../bolt/spec.md#docker).

## Examples

- [examples/stdio.md](examples/stdio.md) — an MCP host session over stdio.
- [examples/http.md](examples/http.md) — `curl` against the HTTP transport.
- `surfaces/relate-mcp/examples/inspect_schema.rs` — the same session
  in-process against `McpServer::handle_message` (compiled in CI); the
  crate-level doctest runs it.
