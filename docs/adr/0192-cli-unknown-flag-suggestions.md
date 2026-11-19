---
type: adr
area: cli
date: 2026-10-09
deciders: CypherLite maintainers
name: 0192-cli-unknown-flag-suggestions
status: proposed
tags:
  - did-you-mean
title: CLI "did you mean" suggestion for unknown flags
REFERENCES:
  - "[[adr/0069-cli-lifecycle-commands]]"
  - "[[adr/0190-scoped-cli-help]]"
---

# 0192. CLI "did you mean" suggestion for unknown flags

## Context

[ADR-0190](./0190-scoped-cli-help.md) scoped usage errors to the relevant command's synopsis plus a help
hint. An unknown flag still gives no hint about what was meant: a user typing
`--readonly` (the flag is `--read-only`) gets `unknown flag: --readonly` and
must scan the help to find the spelling. The CLI parses flags by hand
([ADR-0069](./0069-cli-lifecycle-commands.md)), so there is no parser library providing suggestions.

## Decision

1. An unknown flag error, for the query run and for every command's parser,
   prints one extra line between the `Error:` line and the `Usage:` line when
   a flag of the same scope is a close match: `Did you mean '--read-only'?`.
2. Matching ignores leading dashes, `-`/`_` separators, and case, and accepts
   a Levenshtein distance of at most `max(1, len / 3)` on the folded name.
   The closest candidate wins; no candidate means no line.
3. Candidates are the static `TOP_LEVEL_FLAGS` list for the query run and the
   `flags` field of each command's `CommandSpec` in `help.rs` (long spellings
   only; legacy single-dash aliases are not suggested). Tests pin each list
   against its help page, and `e2e_help.rs` feeds every documented flag to the
   real parser. `--fetch-params` is a candidate only in builds that document
   it. Only a dash-prefixed rejected argument is matched;
   a rejected positional or subcommand word (`skill instal`) gets none.
4. The suggestion comes from that static list and is printed outside
   `print_error`'s sanitizer; user input is never echoed in it.
5. The command parsers return a typed `parse_error::ParseError`:
   `Message(String)` for every failure with no rejected argument, and
   `Rejected { command, arg }` for an argument a command does not accept (an
   unknown flag, an extra positional, a stray word). `Display` keeps the
   established wording byte for byte
   (`unexpected argument to <command>: <arg>`), and `From<String>`/`From<&str>`
   keep `?` and `.into()` at the existing error sites working. The rejected
   argument travels as data, so no message text is ever re-parsed, and only a
   `Rejected` error can draw a suggestion: a missing-argument error, or a
   flag-shaped value that another flag swallowed (a `Message`), never does.
   `suggest_flag` itself declines anything that does not start with `-`, so a
   rejected positional or subcommand word gets none. The suggestion is typed
   `&'static str`, so the compiler enforces that the unsanitized line can only
   hold candidate-list text. `ParseError` and the shared `flag_value`
   primitive live in `parse_error.rs`; `suggest.rs` is the pure matcher.
6. No new dependency: the edit distance is a ~12-line local function.

## Alternatives considered

- **Adopt clap for the CLI.** Gives suggestions for free, but replaces the
 hand-rolled parser and its byte-for-byte pinned help ([ADR-0069](./0069-cli-lifecycle-commands.md), [ADR-0190](./0190-scoped-cli-help.md)).
  Rejected: far larger than the problem.
- **Add the `strsim` crate.** Rejected: a dependency for a dozen lines of code.
- **Fold the suggestion into the `Error:` line.** Rejected: `print_error`
  sanitizes newlines out of messages, so it would be one long line.
- **Keep `Result<_, String>` and re-parse the message.** Tried first, with the
  wording owned by one constructor and its inverse. Rejected after review: the
  rejected token travelled as formatted text, so the suggestion layer depended
  on message wording that a typed value states directly.
- **Mine the first dash-prefixed token of the arguments.** Rejected: value-taking
  flags swallow the next token, so a flag-shaped value was mistaken for the
  typo and drew a suggestion on an unrelated error.
- **Suggest on every unknown flag with any distance.** Rejected: wrong
  suggestions are worse than none; the threshold keeps `--bogus` silent.

## Consequences

- `--readonly`, `--read_only`, `--jsn`, `clone --wal-dri` and similar slips name the right flag.
- A new flag must be added to `TOP_LEVEL_FLAGS` or its command's `flags`; the
  help-page tests fail if a list names a flag the help does not show. A flag
  missing from the list only loses its suggestion.
- `docs/api/cli/spec.md` documents the extra line.
