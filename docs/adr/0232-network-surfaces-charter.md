---
type: adr
area: surface
name: 0232-network-surfaces-charter
status: accepted
tags:
  - auth
  - serve
title: Network surfaces charter — shared auth, Query API v2 subset, MCP transport
REFERENCES:
  - "[[adr/0028-user-visible-transactions]]"
  - "[[adr/0057-embedded-c-abi-and-bolt]]"
  - "[[adr/0080-cli-pull-command]]"
  - "[[adr/0109-bolt-tls]]"
  - "[[adr/0116-fixed-namespaces-community-edition-semantics]]"
  - "[[adr/0126-bolt-basic-auth]]"
  - "[[adr/0136-read-only-open]]"
  - "[[adr/0137-wal-follow]]"
  - "[[adr/0223-wire-entity-kind-tags]]"
  - "[[adr/0234-httparse-request-parsing]]"
  - "[[adr/0235-db-ping-procedure]]"
  - "[[adr/0236-query-api-v2-value-encoding]]"
  - "[[adr/0237-query-api-tx-routes]]"
  - "[[adr/0238-shared-accept-loop]]"
  - "[[adr/0260-trust-boundary-error-sanitization]]"
  - "[[adr/0266-bolt-websocket-transport]]"
  - "[[adr/0306-read-only-follow-servers]]"
  - "[[adr/0480-one-access-flag-read-only-freshness]]"
---

# ADR-0232: Network surfaces charter — shared auth, Query API v2 subset, MCP transport

## Context

CypherLite ships one network surface today: the Bolt server
(`cypherlite-bolt`, [ADR-0057](./0057-embedded-c-abi-and-bolt.md)), with basic auth plus a generated-token
default ([ADR-0126](./0126-bolt-basic-auth.md)), a TLS/plaintext policy ([ADR-0109](./0109-bolt-tls.md)), and a WebSocket
transport on the same bind ([ADR-0266](./0266-bolt-websocket-transport.md)). Two new network surfaces are
planned on top of `cypherlite::Database` (LABS-297/298/299):

- an **HTTP Query API v2 endpoint** (`cypherlite serve --http PORT`),
  speaking the Neo4j Query API v2 statement-envelope so existing
  HTTP-first tooling (including `cypherlite-load`'s ingest direction)
  works against CypherLite without a Bolt driver;
- an **MCP server** (`cypherlite serve --mcp PORT`), so LLM agents can
  run Cypher through the Model Context Protocol the way the reference
  `mcp-neo4j-cypher` server does against Neo4j.

Before either is built, four cross-cutting decisions must be fixed so
the two surfaces (and the CLI `serve` node that hosts them) cannot
drift: one auth story, one home for the shared auth code, the exact
Query API v2 subset (the user has explicitly ruled out routing,
transaction persistence, and extras), and the MCP transport plus SDK
choice.

The Neo4j Query API v2 reference shape is confirmed against the Neo4j
codebase (`community/server/.../queryapi/`): route
`POST /{databaseName}/query/v2`, request record
`{statement, parameters, includeCounters}`, response envelope keys
`data` (`fields` + `values`), `counters`, `bookmarks`, `errors`
(`Fieldnames.java`). The reference MCP server
(`mcp-neo4j/servers/mcp-neo4j-cypher`) supports three transports:
`stdio`, `sse`, and streamable `http`, and exposes exactly three tools
(`read_neo4j_cypher`, `write_neo4j_cypher`, `get_neo4j_schema`).

## Decision

### 1. One shared auth story

All HTTP-bearing network surfaces (Query API v2 endpoint, MCP over
HTTP) use **generated-token basic auth, identical in kind to the Bolt
surface ([ADR-0126](./0126-bolt-basic-auth.md))**:

- resolution order: `--auth user:password` flag wins, then
  `CYPHERLITE_AUTH=user:password`, else a generated token printed at
  startup and persisted as a `<dir>.auth-token` sibling (mode 0600,
  fresh per start for `:memory:`);
- `none` disables authentication (the zero-config story);
- a present-but-malformed value is a loud startup error, never a silent
  fallback;
- credential comparison is constant-time, as in Bolt's `auth.rs`.

On the wire, HTTP surfaces carry the credential as an RFC 7617
`Authorization: Basic` header; MCP over stdio skips the challenge
entirely (the parent process spawned the server — there is no trust
boundary to cross). The *credential itself* — one user, one password or
generated token — is the same across surfaces, so a single printed
token unlocks Bolt, HTTP, and MCP-HTTP alike.

### 2. Where the shared auth code lives

The Bolt `auth.rs` mechanism is **extracted into a new small surface
crate, `surfaces/cypherlite-auth`** (package name `cypherlite-auth`),
exporting `AuthConfig` (resolution, persistence, constant-time check)
with no HTTP- or Bolt-specific types. `cypherlite-bolt`, the HTTP Query
API surface, and the MCP surface each depend on it and adapt its
`check` to their wire shape (HELLO map vs `Authorization` header). The
crate is a pure adapter of engine-neutral logic: it stays in
`surfaces/` per R3, and the engine never sees it.

### 3. Query API v2 statement-endpoint subset

Exactly one route, the auto-commit statement endpoint:

- **Route:** `POST /db/{databaseName}/query/v2` (the `/db` prefix keeps
  the path distinct from any future root-level endpoint; the
 `{databaseName}` is validated against the [ADR-0116](./0116-fixed-namespaces-community-edition-semantics.md) fixed namespaces
  and an unknown name fails).
- **Request fields:** `statement` (string, required), `parameters`
  (object, optional, `$name` bindings), `includeCounters` (boolean,
  default false). Nothing else — no `accessMode`, no `impersonatedUser`,
  no `maxExecutionTime`, no `txConfig`.
- **Response envelope (success):** `data.fields` (column names),
 `data.values` (rows of wire-JSON values per [ADR-0223](./0223-wire-entity-kind-tags.md)), `bookmarks`
  (empty array — no bookmark manager), and `counters` (stats counters
  in the snake_case vocabulary, present when `includeCounters` is true
  or the statement updated the graph).
- **Error envelope:** `{"errors":[{"code","message"}]}` with the
  GQLSTATUS-style codes the engine already produces (e.g. `42N00` for
  an unknown database), mirroring the Bolt trust-boundary sanitization
 of [ADR-0260](./0260-trust-boundary-error-sanitization.md).

Explicitly **out** per the user's instruction: no routing headers or
server-discovery endpoint, no transaction persistence (`POST /tx`,
`tx/{id}` begin/commit/rollback are not implemented and return 404),
no multi-statement batching, no profiled-query format negotiation.

### 4. MCP transport and SDK

**Both stdio and streamable HTTP.** Streamable HTTP is required because
the `cypherlite serve --mcp PORT` flag implies a network listener, and
it matches the reference server's default for Docker deployments.
stdio is required because the dominant MCP hosts (Claude Desktop,
Claude Code) spawn servers as child processes over stdio, and the
reference server supports it; an HTTP-only server would be unusable for
the local-agent case that motivates LABS-299. Legacy SSE transport is
not implemented (superseded by streamable HTTP in the MCP spec).

**SDK: hand-rolled JSON-RPC 2.0, no `rmcp`/external MCP SDK.** The MCP
server protocol surface needed for three tools (`initialize`,
`tools/list`, `tools/call`, plus `ping`/`notifications/initialized`) is
a few hundred lines over `serde_json`; the official Rust SDK (`rmcp`)
pulls in a heavy async-runtime-and-macros dependency tree for
abstractions we do not need, and per R6 a heavy dependency deserves its
own justification — here it has none. **Dependency note:** the MCP
surface adds only `serde`/`serde_json` (already workspace-wide) plus
whatever HTTP stack the Query API surface already introduces; no new
heavy transitive tree.

## Alternatives considered

### Auth story

- **Per-surface auth mechanisms (bearer tokens for HTTP, basic for
  Bolt).** Rejected: two credential stories means two startup banners,
  two persistence files, and users re-learning each surface; the
 [ADR-0126](./0126-bolt-basic-auth.md) generated-token pattern is already proven and documented.
- **mTLS / client certificates.** Rejected: orders of magnitude more
  setup than the embedded, localhost-first use case justifies; TLS
 itself remains governed by [ADR-0109](./0109-bolt-tls.md)'s policy.
- **No auth on HTTP surfaces (localhost is safe enough).** Rejected:
  the Bolt surface deliberately defaults to a generated token because
  "localhost" is not a trust boundary in shared/dev-container
  environments; weakening that for HTTP would be an inconsistent
  security posture.

### Home for shared auth code

- **Duplicate the small `auth.rs` module into each surface.** Rejected:
  three copies of token generation, persistence, and constant-time
 comparison is exactly the drift pattern [ADR-0223](./0223-wire-entity-kind-tags.md) calls out for the
  wire format; a security-sensitive module is the worst thing to fork.
- **Move auth into `surfaces/cypherlite-ffi` (the existing shared
  surface crate).** Rejected: the FFI crate's job is the C ABI;
  linking a C-facing artifact into pure-Rust servers couples them to
  the ABI's build and symbol surface for no reason.
- **Move auth into an engine crate (e.g. `cypherlite-core`).**
  Rejected: auth is a server concern, not engine logic; R1 would then
  permit every engine crate to see it, and the engine would gain
  file-persistence policy it should not own (the engine never depends
  on surface concerns).

### Query API v2 subset

- **Full Neo4j Query API v2, including managed transactions.** Rejected:
  the user explicitly scoped this to the statement endpoint; transaction
  persistence across HTTP requests is a stateful-session problem
  CypherLite's embedded model does not need to solve for the target
  tooling.
- **A CypherLite-native JSON envelope instead of the v2 shape.**
  Rejected: the point of the endpoint is compatibility with existing
 Query API clients (and the reverse of `cypherlite-load`, [ADR-0080](./0080-cli-pull-command.md)); a
  bespoke envelope buys nothing and costs every client an adapter.

### MCP transport and SDK

- **HTTP-only.** Rejected: excludes stdio-spawning MCP hosts, the
  primary consumption mode for local agent tooling.
- **stdio-only.** Rejected: contradicts the agreed `serve --mcp PORT`
  flag shape and remote/container deployments.
- **Adopt the official Rust MCP SDK (`rmcp`).** Rejected: a heavy
  async/macros dependency tree for a protocol subset this small fails
  R6's cost-benefit bar; hand-rolled JSON-RPC keeps the dependency
  footprint at `serde_json` and is fully testable against the MCP
  inspector.
- **Support legacy SSE transport too.** Rejected: superseded in the MCP
  spec by streamable HTTP; three transports is surface area without
  consumers.

## Consequences

- `surfaces/cypherlite-auth` is created by the first surface node that
  needs it (the HTTP Query API node); Bolt's `auth.rs` is re-homed
  there in the same change, with Bolt's behavior unchanged (its tests
  move with the module).
- The `cypherlite serve` CLI node (LABS-297) gains `--http PORT`,
  `--mcp PORT`, and reuses `--auth`/`CYPHERLITE_AUTH`; the flag table
  in `docs/api/cli/spec.md` and the hand-written `--help` update in the
  same change per the CLI rules.
- The Query API v2 endpoint documents its subset under
  `docs/api/http/` (spec, description, examples) in the node that
 implements it; responses use the [ADR-0223](./0223-wire-entity-kind-tags.md) `kind`-tagged wire JSON, so
  row values match every other surface with no new encoder.
- The MCP server documents its three tools and transports under
  `docs/api/mcp/`; no new workspace dependency is added for MCP beyond
  what the HTTP surface already brings.
- Later additions (managed transactions, routing, extra MCP tools or
  resources) each require their own ADR amending this charter.

## Later references

The HTTP Query API v2 statement endpoint of §3 landed as
`surfaces/cypherlite-http` (package `cypherlite-http`): a hand-rolled
minimal HTTP/1.1 server (no hyper/axum) exporting `serve` / `serve_handler`
for the later `cypherlite serve --http PORT` CLI node, with auth adapted
from `cypherlite-auth`'s `AuthConfig::check_basic` and row values encoded
through `cypherlite-model`'s [ADR-0223](./0223-wire-entity-kind-tags.md) kind-tagged wire JSON. The route,
envelope, status codes, and GQLSTATUS-style error-code table are documented
under [`docs/api/http/`](../api/http/spec.md).

The `cypherlite serve` CLI node of Consequences landed as
`surfaces/cypherlite-cli/src/serve_cmd.rs`: one open store, `--http PORT` /
`--mcp PORT` (Cargo features `http` / `mcp` on the CLI), one
`cypherlite_auth::resolve` auth story printed once at startup, loopback-only
binds with no default ports, and a SIGINT/SIGTERM shutdown that takes the
shared final checkpoint. The MCP crate gained `serve_shared` /
`serve_http_on` entry points so the CLI can bind both sockets before the
banner and share one `Arc<Database>` across surfaces.

[ADR-0234](./0234-httparse-request-parsing.md): the request-head byte parsing of the HTTP layer this charter
kept hand-rolled is delegated to `httparse`; every cap, gate, and the
accept-loop policy named above stays owned by `cypherlite-http`.

[ADR-0235](./0235-db-ping-procedure.md): `db.ping` (client connectivity probe). [ADR-0236](./0236-query-api-v2-value-encoding.md): the Query API
v2 value encoding follows the Neo4j plain/typed JSON contract rather than
[ADR-0223](./0223-wire-entity-kind-tags.md)'s kind tags on this surface. [ADR-0237](./0237-query-api-tx-routes.md): the §3 exclusion of
managed-transaction routes is superseded — `…/tx`, `…/tx/{id}`,
`…/tx/{id}/commit` and `DELETE …/tx/{id}` now map onto the engine's
explicit transactions ([ADR-0028](./0028-user-visible-transactions.md)).

[ADR-0238](./0238-shared-accept-loop.md): the accept-loop trio (`ConnectionCap`, `accept_resilient`,
`serve_capped`) is re-homed from `cypherlite-http` into the new std-only
`surfaces/cypherlite-serve` so Bolt, HTTP, and MCP share one
implementation; the auth crate was deliberately not widened to hold it.

[ADR-0306](./0306-read-only-follow-servers.md): `serve` takes post-verb `--read-only` and `--follow` (follow
implying read-only), opening the served store with `OpenMode::ReadOnly` /
`OpenMode::Follow` — the [ADR-0136](./0136-read-only-open.md)/[ADR-0137](./0137-wal-follow.md) opens — so a server serves
beside a writer process and a following server shows its commits; the
standalone `cypherlite-mcp` binary's `--read-only` becomes that same
read-only open and it gains `--follow`. [ADR-0480](./0480-one-access-flag-read-only-freshness.md) later folds the CLI
spelling into one flag, `--read-only[=snapshot|live]`; the opens and the
`cypherlite-mcp` behavior are unchanged.
