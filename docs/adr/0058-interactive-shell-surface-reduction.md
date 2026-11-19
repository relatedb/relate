---
type: adr
area: cli
date: 2026-09-17
deciders: CypherLite maintainers
name: 0058-interactive-shell-surface-reduction
status: proposed
tags:
  - repl
title: Interactive shell surface reduced to six dot-commands
REFERENCES:
  - "[[adr/0028-user-visible-transactions]]"
  - "[[adr/0052-graphstore-withdrawn]]"
  - "[[adr/0073-interactive-cli-line-editing]]"
---

# 0058. Interactive shell surface reduced to six dot-commands

## Context

The interactive shell predates the product unification. It grew a
SQLite-style dot-command surface (~36 commands) against machinery that no
longer exists: GraphStore backup/restore and the JSONL `--server`
([ADR-0052](./0052-graphstore-withdrawn.md)), Lance versioning, multi-database attach/use, import/export
formats, schema catalogs, output redirection, and a rustyline line editor
(tab completion, persistent history, Ctrl+R search).

After the merge onto `cypherlite::Database`, the shell is a thin stdin loop
(`read_line` + `input_is_complete`, no rustyline) over the public
`Database`/`Connection` API. The code implements exactly six dot-commands —
`.help`, `.quit` / `.exit`, `.begin`, `.commit`, `.rollback`, `.checkpoint`
(`crates/cypherlite-cli/src/main.rs`) — and every other dot-command fails
with `Error: unknown command <name>`.

The docs did not follow: `docs/docs/reference/dot-commands.md` presented the
full legacy surface as live, and the tutorials told new users to run
commands that only error. Docs are the public contract and examples must be
runnable (AGENTS.md §2), so the gap is a defect in the contract, not just
stale prose.

## Decision

The interactive shell's dot-command surface is exactly the six implemented
commands:

- `.help` — print the command summary.
- `.quit` / `.exit` (bare `exit` / `EXIT` also work) — exit immediately.
- `.begin` / `.commit` / `.rollback` — explicit transactions
 ([ADR-0028](./0028-user-visible-transactions.md) semantics on `Connection`).
- `.checkpoint` — force an immediate checkpoint (snapshot + WAL recycle).

Every other dot-command is withdrawn and errors `unknown command`. There is
no line editing, tab completion, or persistent history; statements submit on
a terminating top-level `;`, dot-commands on their newline. Introspection
formerly behind `.labels` / `.reltypes` / `.schema` / `.count` is done with
ordinary Cypher (`labels(n)`, `type(r)`, `count(...)`).

Documentation is brought in line in the same change: `dot-commands.md`
documents only the six commands and lists the withdrawn ones with a pointer
here; the tutorials and CLI examples no longer present removed features as
current.

## Alternatives considered

- **Reimplement the full legacy command set on `cypherlite::Database`** —
  rejected: most commands either duplicate what Cypher already expresses
  (introspection, counts, dump via `CREATE` statements) or target withdrawn
  machinery (GraphStore backup, Lance versions, attach/use), and each one
  adds a shell-only public API to test and document forever.
- **Restore rustyline (line editing, tab completion, history)** — rejected:
  an extra dependency and maintenance surface for marginal UX on a shell
  whose primary uses are one-shot `-c` queries and piped scripts; can return
  later via a new ADR if demanded.
- **Keep the aspirational docs and grow the code to match** — rejected:
  docs are the contract and examples must run (AGENTS.md §2); documenting
  unimplemented commands as live ships a broken tutorial to every new user.
- **Remove the interactive shell entirely (one-shot + pipe only)** —
  rejected: the six-command loop is small, useful for exploratory sessions,
  and already tested; removing it would break `docs/api/cli` workflows for
  no maintenance win.

## Consequences

- Any new dot-command is a public-API addition: it needs implementation,
  tests, `dot-commands.md`, and `docs/api/cli/spec.md` in the same change.
- Introspection and data movement go through Cypher itself or the
  one-shot/pipe flags (`--csv`, `--json`, `--mode`), keeping one way to do
  each thing across the REPL, scripts, and bindings.
- Withdrawn commands are documented as withdrawn (with this ADR as the
 reference), mirroring the treatment of the JSONL protocol ([ADR-0052](./0052-graphstore-withdrawn.md)).
- Reinstating any withdrawn command or line editing requires a new ADR
  referencing this one.

## Later references

- [ADR-0073](./0073-interactive-cli-line-editing.md) restores rustyline line
  editing and in-session history on TTY prompts. The withdrawn dot-command
  list in this ADR is unchanged. Tab completion and persistent history stay
  withdrawn.
