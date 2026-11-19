---
type: adr
area: cli
date: 2026-09-21
deciders: CypherLite maintainers
name: 0073-interactive-cli-line-editing
status: accepted
tags:
  - repl
title: Interactive CLI line editing via rustyline
REFERENCES:
  - "[[adr/0058-interactive-shell-surface-reduction]]"
SUPERSEDES:
  - "[[adr/0058-interactive-shell-surface-reduction]]"
---

# 0073. Interactive CLI line editing via rustyline

## Context

[ADR-0058](./0058-interactive-shell-surface-reduction.md) reduced the interactive shell to a thin stdin loop
(`read_line` + `input_is_complete`) over `cypherlite::Database` and
explicitly withdrew rustyline: no line editing, tab completion, or
persistent history. That decision recorded that rustyline "can return
later via a new ADR if demanded."

Linear issue **LABS-59** ("Improve terminal editing in cypherlite
interactive CLI") is that demand. On a TTY, arrow keys are echoed as
escape sequences (for example `^[[D` for left), so users need an external
wrapper such as `rlwrap` to edit a line. One-shot `-c` / positional
`QUERY` and piped stdin are unaffected — they never go through the
prompt loop.

The workspace already pins `rustyline = "14"` under the unused `# REPL`
block in the root `Cargo.toml`. Reinstating it for the TTY prompt is a
dependency activation, not a new crate.

## Decision

The interactive TTY shell uses **rustyline** for GNU readline-style line
editing. Scope:

1. **TTY only.** rustyline is constructed only when stdin is a terminal.
   Piped stdin still `read_to_string`s the batch; one-shot `QUERY` /
   `--command` is unchanged.
2. **Editing and in-session history.** Emacs bindings (arrow keys,
   Home/End, Delete, Ctrl-A/E/K/U/W, and the rest of rustyline's default
   emacs map) and up/down history for the current process. History is
   **in-memory**: a completed statement (or dot-command) is one history
   entry. There is no history file and no tab completion.
3. **Prompt contract unchanged.** `cypher>` on an empty buffer, `   ...> `
   while `input_is_complete` is false. Statements still submit on a
   terminating top-level `;`; dot-commands still submit on their newline.
4. **Ctrl-C cancels.** During a prompt, Ctrl-C clears the pending
   multi-line buffer and returns to `cypher>`. It does not exit the
   process. Ctrl-D / EOF keeps today's behavior: execute any non-empty
   pending buffer, then exit 0. `.quit` / `.exit` still exit immediately.
5. **Fallback.** If the editor cannot be constructed, the shell prints a
   one-line warning to stderr and uses the previous `read_line` loop so a
   TTY session still works.
6. **No new flags.** Line editing is not optional and is not a lifecycle
   command.

Reinstating tab completion or a persistent history file needs a later ADR.

## Alternatives considered

- **Keep `read_line` and document `rlwrap`** — rejected: LABS-59 is the
  report that escape sequences leak and that an external wrapper is the
  workaround. The product CLI should edit lines itself.
- **Hand-roll an editor on `crossterm`** (already a workspace pin) —
  rejected: reimplements readline (encoding, redraw, history, signals)
  for no product difference.
- **reedline** — rejected: a second REPL crate while `rustyline = "14"`
 is already pinned and is the library [ADR-0058](./0058-interactive-shell-surface-reduction.md) named as the return path.
- **Full rustyline restoration** (tab completion, `~/.cypherlite_history`,
 advertised Ctrl-R search) — rejected: that is the surface [ADR-0058](./0058-interactive-shell-surface-reduction.md)
  declined; LABS-59 asks for cursor-key editing, not a second completion
  language. Ctrl-R remains whatever rustyline's default emacs map does
  against in-session history; it is not a documented product feature.

## Consequences

- [ADR-0058](./0058-interactive-shell-surface-reduction.md)'s "no line editing" clause is superseded for TTY sessions;
  its withdrawn **dot-command** list is unchanged.
- `cypherlite-cli` depends on the existing workspace `rustyline` pin.
- Docs that stated "no line editing" (getting-started, dot-commands,
  `docs/api/cli`, the skill-bundle CLI reference) describe editing and
  in-session history. Ctrl-C is documented as cancel, not quit.
- Piped and one-shot paths stay on the splitter / `exec_cypherlite_input`
  batch code, so existing e2e pipe tests remain the contract for
  non-TTY input.
- rustyline treats `TERM=dumb` (common in `cargo test` and some CI
  sandboxes) as an unsupported terminal and uses cooked stdin, same as
  the fallback path. A real terminal type (`xterm`, `xterm-256color`, …)
  is required for cursor-key editing.
