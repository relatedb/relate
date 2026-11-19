---
type: adr
area: cli
date: 2026-10-14
deciders: CypherLite maintainers
name: 0231-bare-invocation-opens-shell-on-tty
status: accepted
tags:
  - repl
title: bare `cypherlite` opens the interactive shell on a TTY
REFERENCES:
  - "[[adr/0069-cli-lifecycle-commands]]"
  - "[[adr/0200-query-command-and-database-flag]]"
SUPERSEDES:
  - "[[adr/0200-query-command-and-database-flag]]"
---

# ADR-0231: bare `cypherlite` opens the interactive shell on a TTY

## Context

[ADR-0069](./0069-cli-lifecycle-commands.md) and [ADR-0200](./0200-query-command-and-database-flag.md) reserved the bare invocation form for usage: today
`cypherlite` with no statement and no command word prints the top-level
help and exits 0, and only the explicit `query` verb reaches the session
trichotomy (oneshot / stdin script / interactive shell). The peer embedded
databases — sqlite3, duckdb, ch-local — are shells first: typing the bare
binary name at a terminal drops you into the prompt. Users reaching for
`cypherlite` the same way get a help page instead of a prompt, and every
interactive session costs a redundant `query` word.

The constraint that motivated the reservation is still real: scripts and
agent harnesses run `cypherlite` with stdin piped or redirected, and a bare
invocation there must never block reading a prompt. Any change must keep
the non-TTY behavior byte-for-byte.

## Decision

1. **Bare `cypherlite` on an interactive TTY opens the statement-session
   shell.** When no statement (positional or `-c`/`--command`) and no
   command word is given, and stdin is a terminal, the binary runs exactly
   the session `cypherlite query` with no `STATEMENT` runs on a terminal:
   same session code, same prompt, same `:memory:` default store.
   `--database PATH` and the other session flags still apply, so
   `cypherlite --database ./mydata` opens the shell on that store.
2. **Bare `cypherlite` on a non-TTY keeps today's behavior.** Piped,
   redirected, or closed stdin with no statement and no command word
   prints the top-level help to stdout and exits 0. Scripts never hang on
   stdin.
3. **No flag or exit-code changes.** The hand-written `--help` text stays
   the source of truth; its trailing usage notes are updated to describe
   the TTY shell. A clean shell exit (`.quit`, EOF) exits 0, as `query`
   already does. Telemetry classifies the run by the session it becomes
   (`repl` source), not by how it was spelled.

## Alternatives considered

- **Keep the reserved-for-usage bare form ([ADR-0200](./0200-query-command-and-database-flag.md)).** Rejected: it is
  the only invocation peers disagree on, and the `query` word is dead
  weight in the most common interactive case.
- **Open the shell whenever no statement is given, TTY or not.** Rejected:
  a piped or harness-driven bare invocation would block on stdin forever;
  the non-TTY help print is the scriptable contract.
- **A new `shell` verb instead of overloading the bare form.** Rejected:
  it adds a second spelling for what `query` already does and still leaves
  the bare form printing help on a terminal.

## Consequences

- [ADR-0200](./0200-query-command-and-database-flag.md) Decision 2 ("On the path-first form, no `QUERY` prints help and
  exits; it does not open the shell or read stdin") is superseded on an
  interactive TTY; the non-TTY half stands.
- The shell and its store selection are shared with `query` by
  construction: the bare form only relaxes the gate in `main` that used to
  print help before the session classification ran.
- `docs/api/cli/spec.md` and `docs/api/cli/description.md` describe the
  TTY/non-TTY split; the PTY e2e tests pin the shell opening with no
  arguments, and the existing piped e2e pins the non-TTY help print.

## Later references

None yet.
