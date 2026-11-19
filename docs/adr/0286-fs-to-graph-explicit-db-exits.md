---
type: adr
area: cli
date: 2026-10-02
deciders: CypherLite maintainers
name: 0286-fs-to-graph-explicit-db-exits
status: accepted
tags:
  - commands
  - load
title: "`fs-to-graph` exits when `--db` is set"
REFERENCES:
  - "[[adr/0157-fs-to-graph-import]]"
  - "[[adr/0383-lab-database-flag]]"
SUPERSEDES:
  - "[[adr/0157-fs-to-graph-import]]"
---

# 0286. `fs-to-graph` exits when `--db` is set

- **Renumbered**: from 0166 (shared-number cleanup)

## Context

[ADR-0157](./0157-fs-to-graph-import.md)'s UX rule opens the interactive shell after a successful
`cypherlite lab fs-to-graph` whenever stdin is a TTY and the `repl` feature
is compiled in. That rule does not distinguish an omitted `--db` from an
explicit one.

The two cases are different. With no `--db` the graph lives in `:memory:`
and disappears when the process exits, so the shell is the only way to
query it. With `--db PATH` the graph is written to that path and
checkpointed; the caller opens it afterwards with `cypherlite PATH`. In
CypherLite 0.11.0, `cypherlite lab fs-to-graph ./cypherlite --db
./target/fs-graph` finished the import and then dropped into the `cypher>`
prompt on a TTY (LABS-166). The database was created; the process did not
exit.

## Decision

An **explicit `--db`** never opens the interactive shell. After a successful
import the command prints the summary and the paste-ready `SEARCH` query,
checkpoints a persistent target, and exits 0. This includes `--db :memory:`:
passing the flag is the request to finish, even when the target is not
durable.

**Omitting `--db`** is unchanged from [ADR-0157](./0157-fs-to-graph-import.md). The import uses `:memory:`,
and on a TTY with the `repl` feature the shell opens on that graph.
Without a TTY, or without `repl`, the command prints the summary and exits.

## Alternatives considered

- **Keep opening the shell on every TTY, including an explicit `--db`.**
  Rejected: that is the LABS-166 behavior. A persisted graph is queried by
  opening the path; leaving the prompt up blocks a terminal that expected
  the import to finish.
- **Open the shell for every `:memory:` target, including `--db :memory:`.**
  Rejected: the flag is the caller's signal that this invocation should
  exit. Treating the path string as the signal would make `--db :memory:`
  and an omitted `--db` the same command.
- **Add a `--no-shell` flag and keep the TTY default for `--db`.**
  Rejected: the surprising case is the one with an explicit database. A
  second flag would leave the default wrong and give scripts another switch
  to remember.

## Consequences

- `lab_cmd` records whether `--db` was present and opens the shell only
  when it was not, stdin is a TTY, and `repl` is compiled in.
- `docs/api/cli/spec.md`, the hand-written `--help` text, and
  `docs/reference/cli-flags.md` state that an explicit `--db` exits without
  the shell.
- [ADR-0157](./0157-fs-to-graph-import.md)'s unconditional TTY-shell sentence is superseded for the
  explicit-`--db` case; a **Later references** note there points here. The
  omitted-`--db` shell is unchanged.

## Later references

[ADR-0383](./0383-lab-database-flag.md) renames the flag this rule keys on: an explicit `-d` / `--database`,
not `--db`, is the signal that the import exits without the shell. The
supersession is the flag spelling only; the explicit-versus-omitted rule
above stands.
