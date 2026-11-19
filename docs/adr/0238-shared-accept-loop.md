---
type: adr
area: surface
name: 0238-shared-accept-loop
status: proposed
tags:
  - serve
title: One shared accept loop in `cypherlite-serve`
REFERENCES:
  - "[[adr/0110-bolt-connection-query-governance]]"
  - "[[adr/0224-result-column-types]]"
  - "[[adr/0226-ontology-mappings]]"
  - "[[adr/0233-bolt-thread-per-connection]]"
---

# ADR-0238: One shared accept loop in `cypherlite-serve`

## Context

Three network surfaces — Bolt (`cypherlite-bolt`), the HTTP Query API
(`cypherlite-http`), and MCP over HTTP (`cypherlite-mcp`) — all run the
same accept policy ([ADR-0110](./0110-bolt-connection-query-governance.md), on sync primitives): thread-per-connection
under a connection cap, accept-then-close over cap, accept errors logged
and retried, never a shutdown condition. After the [ADR-0226](./0226-ontology-mappings.md) sync rewrite,
that policy existed **twice**: `cypherlite-http`'s `http.rs` owned
`ConnectionCap` / `accept_resilient` / `serve_capped` (consumed by HTTP
and MCP), while `cypherlite-bolt`'s `server.rs` re-implemented the trio in
miniature. The copies had already diverged: Bolt armed a 64 MiB stack per
connection thread and logged thread-spawn failures, while the HTTP copy
used a bare `std::thread::spawn` and silently dropped spawn errors — so
HTTP and MCP lacked a hardening Bolt had, and any future policy fix would
have had to land in two places again.

## Decision

The accept-loop trio — `ConnectionCap`/`ConnectionPermit`,
`accept_resilient`, and `serve_capped` — lives in exactly one place: a new
small surface crate, **`surfaces/cypherlite-serve`** (package
`cypherlite-serve`), std-only and protocol-free. Its public API is
parameterized by the four things that genuinely differ per surface:

- the log label (`"bolt"`, `"http"`, `"mcp"`),
- the per-connection thread stack size (Bolt passes its [ADR-0226](./0226-ontology-mappings.md) 64 MiB;
  HTTP/MCP pass `DEFAULT_CONNECTION_STACK_BYTES` = 8 MiB, because query
  execution already rides the engine's own 64 MiB query-worker thread and
  the connection thread only parses requests and encodes responses),
- the `max_connections` cap,
- the per-connection handler closure.

Thread-spawn failures are logged (`{surface}: failed to spawn the
connection thread: …`) and the connection closed — propagated to the logs,
never silently dropped, in every surface.

All three surfaces consume it: `cypherlite-bolt` keeps
`serve_conn`/`Transport`/`ConnConfig` and only hands the accept
scaffolding to the helper; `cypherlite-http` re-exports the trio from its
`http` module (so `cypherlite-mcp`'s import path is unchanged) and both
HTTP surfaces gain the explicit stack size and spawn-error logging.

The crate also hosts the one shutdown-signal wait,
`signal::wait_shutdown_signal` ([ADR-0233](./0233-bolt-thread-per-connection.md)'s shape: signal-hook's blocking
iterator on unix, a registered flag plus a timed park loop elsewhere,
because signal-hook's `iterator` module is `not(windows)`-gated). Bolt's
`signal::next_shutdown_signal` and the CLI's `cypherlite serve` both
delegate to it — the alternative, each surface carrying its own copy of
the signal code, is the same two-copies drift this ADR exists to prevent.
signal-hook is the crate's only dependency beyond std; the accept loop
itself stays std-only.

## Alternatives considered

- **Put the trio in `cypherlite-auth` (the existing [ADR-0224](./0224-result-column-types.md) shared
  crate).** Rejected: that crate's charter is generated-token basic auth,
  and an accept loop is not auth; widening an auth crate's scope to
  "miscellaneous server plumbing" invites exactly the kitchen-sink drift
  the crate layout rules exist to prevent, and it would mislead every
  future reader about where serving policy lives.
- **Keep the trio in `cypherlite-http` and let Bolt depend on it.**
  Rejected: Bolt deliberately stays self-contained from the HTTP surface
 ([ADR-0226](./0226-ontology-mappings.md)) — depending on `cypherlite-http` would pull `httparse` and
  the whole request/response layer into the Bolt binary for 70 lines of
  accept scaffolding, coupling two wire protocols that share nothing but
  this loop.
- **Leave the two copies and backport Bolt's hardening to the HTTP
  copy.** Rejected: the audit that found the divergence is the evidence —
  two copies of a policy drift; the third copy is where bugs breed. The
  fix is one home, not better copying discipline.

## Consequences

- New workspace crate `surfaces/cypherlite-serve` (R3-clean: surfaces may
  depend on surfaces; the engine never sees it). It joins the crate
  layout list in `AGENTS.md`, the workspace `default-members`, and the
  workspace dependency table.
- `cypherlite-bolt` and `cypherlite-http` gain one dependency edge each
  (`cypherlite-serve`, std-only); `cypherlite-mcp` reaches it through
 `cypherlite-http`'s re-export. The [ADR-0226](./0226-ontology-mappings.md) dependency-tree count moves
  from 149 to 150 normal-edge unique crates for `cypherlite-bolt`.
- HTTP and MCP connection threads now run on an explicit 8 MiB stack with
  spawn failures logged — Bolt's former hardening is now the shared floor.
- Any future accept-policy change (backoff, cap semantics, spawn policy)
  lands in one crate and is tested there; the surfaces keep only their
  label, stack size, cap, and handler.
