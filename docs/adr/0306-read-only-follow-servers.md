---
type: adr
area: surface
claim: 24e79bb65278a3d77ada43b582d9324265f528b2
name: 0306-read-only-follow-servers
status: proposed
tags:
  - serve
title: Read-only and follow opens for serve and cypherlite-mcp
REFERENCES:
  - "[[adr/0136-read-only-open]]"
  - "[[adr/0137-wal-follow]]"
  - "[[adr/0200-query-command-and-database-flag]]"
  - "[[adr/0232-network-surfaces-charter]]"
  - "[[adr/0249-docker-unified-serve-image]]"
  - "[[adr/0480-one-access-flag-read-only-freshness]]"
---

# ADR-0306: Read-only and follow opens for serve and cypherlite-mcp

## Context

`cypherlite serve` ([ADR-0232](./0232-network-surfaces-charter.md)) hosts the HTTP Query API v2, MCP, and Bolt
surfaces in one process on one open store — and it opens that store writable,
so it holds the exclusive `.cypherlite.writer` lease ([ADR-0136](./0136-read-only-open.md)) for the
process lifetime and a CLI write beside a running server fails with
`Error::Locked`. The standalone `cypherlite-mcp` binary is no better: its
`--read-only` documents itself as "disable the write-cypher tool" — it sets
`McpServerConfig.read_only` and hides that one tool, but the store is still
opened writable and the lease still taken (LABS-319).

The query CLI already has both opens a server needs. `--read-only`
([ADR-0136](./0136-read-only-open.md)) takes only the shared identity lock — no writer lease, no
create, no repair — and every mutation fails with the engine's read-only
error. `--follow` ([ADR-0137](./0137-wal-follow.md)) is that open plus a 50 ms WAL poll that
applies complete frames and reloads across a checkpoint gap, refused for
`:memory:` and `.cl`.

A server that serves beside a writer, and a server that follows that
writer's commits, are the same two opens on the served surfaces. The
decision here is only where the flags attach and what the banner says.

## Decision

1. **`serve` accepts `--read-only` and `--follow` after the verb** —
   post-verb flags like `--host`/`--auth`. `--follow` implies read-only;
   `--read-only --follow` together means follow. A repeated flag is a
   usage error (exit 2), the same once-only policy as every optional
   `serve` flag.
2. **The store opens with `OpenMode::ReadOnly` / `OpenMode::Follow`** —
 the same opens as `cypherlite query` ([ADR-0136](./0136-read-only-open.md), [ADR-0137](./0137-wal-follow.md)). Nothing new
   lands in the engine.
3. **Every served surface shares that one handle.** HTTP, MCP, and Bolt
   ride the one read-only/following `Arc<Database>`; a write fails with
   the engine's read-only error on every surface. The MCP surface sets
   `McpServerConfig.read_only` from the opened handle, so `write-cypher`
   is not listed — an always-failing tool would be a dead end for hosts
   that auto-approve `readOnlyHint` tools.
4. **No final checkpoint runs on shutdown** — the existing read-only skip
   in the shared final-checkpoint policy (`session::final_checkpoint`
   refuses when `db.is_read_only()`).
5. **The startup banner prints one line naming the access mode**
   (read-only or follow) on stderr, with the rest of the banner.
6. **`--read-only`/`--follow` given before the `serve` verb is a usage
   error (exit 2) naming the post-verb spelling.** It must never be
   silently ignored: a silently-writable server is the exact hazard the
   user meant to fence.
7. **The Docker entrypoint ([ADR-0249](./0249-docker-unified-serve-image.md)) treats both as valueless flags**
   when passing argv through — forwarded like any other `serve` flag, no
   value consumed and no mapping.
8. **The `cypherlite-mcp` binary's `--read-only` now opens
   `OpenMode::ReadOnly`** (still hiding `write-cypher`), and a new
   `--follow` opens `OpenMode::Follow`, implying read-only.

## Alternatives considered

- **Honor the pre-verb query-session flags (`cypherlite --read-only
  serve …`)** — rejected: the pre-verb position belongs to the
 path-first query form's grammar ([ADR-0200](./0200-query-command-and-database-flag.md)); wiring it into `serve` too
  splits one command across two grammars, and any gap in the forwarding
  leaves a silently-writable server — the loud post-verb-only error has
  one spelling and fails safe.
- **Read-only without follow (frozen serve only)** — rejected: a served
  snapshot that never sees the writer's commits invites "where did my
  data go" against a server that looks live; the follower already exists
 ([ADR-0137](./0137-wal-follow.md)) and serving it is the natural completion.
- **Keep `write-cypher` listed on the MCP surface under read-only** —
  rejected: a listed tool that always fails is a dead end for hosts that
  auto-approve `readOnlyHint` tools; hiding it is the honest
  advertisement of the handle's mode.
- **Keep `cypherlite-mcp --read-only` as a tool-only switch and add a
  separate flag for the read-only open** — rejected: two flags for one
  fence is a trap — the natural spelling would keep doing less than it
  promises; `--read-only` already says read-only and must open it.
- **Fall back to read-only when a writable open hits `Error::Locked`** —
  rejected: a server that silently changes its access mode from ambient
  state is a silent fallback; the mode is the operator's explicit choice
  and a lease conflict stays a loud failure.

## Consequences

- A native directory serves beside one writer process: read-only and
 follow opens take only the shared identity lock ([ADR-0136](./0136-read-only-open.md)), so
  `serve --read-only`/`--follow` runs next to a CLI writer (or any
  writable open) and the writer keeps its lease.
- A `.cl` bundle cannot be written while a read-only server holds it:
  the bundle reader's `LOCK_SH` conflicts with a writer's `LOCK_EX`
 ([ADR-0136](./0136-read-only-open.md)).
- `:memory:` read-only is an empty write fence: `serve --read-only
  --database :memory:` serves an empty store that rejects every write.
- `--follow` on `:memory:` or `.cl` is an open failure (exit 1,
 `Error::Semantic`, [ADR-0137](./0137-wal-follow.md)), and so is a missing store under either
 flag (`Error::Storage`, [ADR-0136](./0136-read-only-open.md) — read-only creates nothing).
- The Bolt ROUTE table still lists WRITE, same as
  `cypherlite-bolt --read-only`: the routing table advertises the
  transport's roles, not the handle's mode; a write fails at execution
  with the read-only error.
- A generated token is still persisted as the `<dir>.auth-token`
 sibling ([ADR-0232](./0232-network-surfaces-charter.md) §1): the access mode changes nothing about auth.
- `cypherlite-mcp --read-only` no longer creates a missing store — the
 read-only open creates nothing ([ADR-0136](./0136-read-only-open.md)).
- `cypherlite-mcp --follow` on its default `:memory:` is a startup error
  (exit 1).
- The `serve` hand-written `--help` and flag table
  (`docs/api/cli/spec.md`) and the `cypherlite-mcp` generated `--help`
  and spec (`docs/api/mcp/spec.md`) change in the same change as the
  flags, per the CLI rules.
- A following server's freshness is the last successful poll or reload
 ([ADR-0137](./0137-wal-follow.md)), not the writer's latest acknowledged commit.

## Later references

[ADR-0480](./0480-one-access-flag-read-only-freshness.md) supersedes the flag *spelling* in §1 and the pass-through set in
§7: `serve` takes one access flag, `--read-only[=snapshot|live]`, once-only
(the §1 fold and its precedence rule are deleted with the second flag);
`--follow` is removed with the generic unknown-flag error. The Docker
entrypoint passes `--read-only` through as a value-taking flag (the
attached `=live` splits; the spaced spelling forwards for the CLI to
refuse). The §6 position rules and everything else in this decision hold.
