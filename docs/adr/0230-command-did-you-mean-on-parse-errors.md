---
type: adr
area: cli
name: 0230-command-did-you-mean-on-parse-errors
status: accepted
tags:
  - did-you-mean
title: Did-you-mean command suggestions on query parse errors
REFERENCES:
  - "[[adr/0069-cli-lifecycle-commands]]"
  - "[[adr/0192-cli-unknown-flag-suggestions]]"
  - "[[adr/0194-cli-error-envelope-and-exit-codes]]"
  - "[[adr/0199-statement-progress-on-failure]]"
  - "[[adr/0200-query-command-and-database-flag]]"
  - "[[adr/0229-remove-system-ontologies-cypher-surface]]"
  - "[[adr/0290-syntax-error-position]]"
  - "[[adr/0518-command-did-you-mean-on-usage-errors]]"
---

# ADR-0230: Did-you-mean command suggestions on query parse errors

## Context

LABS-303. The `cypherlite` CLI recognizes its lifecycle verbs (`clone`,
`info`, `pull`, `skill`, `upgrade`/`update`, `telemetry`, `lab`) and the
statement-session verb `query` only as the first positional argument
([ADR-0069](./0069-cli-lifecycle-commands.md), [ADR-0200](./0200-query-command-and-database-flag.md)). Any other first positional is a Cypher statement,
so a mistyped command word reaches the engine and dies as an opaque parse
error:

```
$ cypherlite qu
syntax error: parse error: unexpected token "qu"
```

The CLI already has did-you-mean for **flags** ([ADR-0192](./0192-cli-unknown-flag-suggestions.md)): an alias-folded
edit-distance matcher in `suggest.rs`, driven from the candidate lists in
`help.rs`, rendered by the usage-error path in `flags.rs`. Nothing
equivalent exists for command words, even though the mistyped-command
case is the most common first-run stumble: the user meant a verb the
binary knows, and the error names neither the verb nor the fix.

The command set is already single-sourced: `help::COMMANDS` is the one
enumeration every help page, usage error, and feature gate reads
([ADR-0229](./0229-remove-system-ontologies-cypher-surface.md) pattern — render every consumer from one descriptor table so
the list cannot drift). The `update` → `upgrade` alias lives in
`flags/lifecycle.rs`'s `COMMAND_WORDS`.

## Decision

A fuzzy command suggestion as a **fallback after a failed parse**, never
a pre-check:

1. **Fallback only.** The suggestion runs exclusively when a statement
   fails with a `Syntax` engine error. Parsing stays authoritative: a
   statement that parses — however command-like its first token — runs
   untouched, and no new text is rejected or rerouted.
2. **Matcher.** `suggest.rs` gains `suggest_command`, reusing the same
   fold (dashes/underscores ignored, case folded) and Levenshtein helper
   as `suggest_flag`. The candidate list is derived from
   `help::COMMANDS` — every compiled-in command word plus the `update`
   alias of `upgrade` — so the suggestion set drifts neither from
   `--help` nor from the feature gates. The first word-like token of the
   failed statement
   (`[A-Za-z][A-Za-z0-9_-]*` after leading whitespace) is the needle; a
   statement with no word-like first token gets no suggestion. A
   candidate matches when the edit distance is within the flag matcher's
   limit (`max(len/3, 1)`) **or** the needle of length ≥ 2 is a prefix of
   the candidate (so `qu` → `query` and `agent-contex` → `agent-context`
   resolve while `qux` does not). The nearest candidate wins; an alias
   match reports the canonical word.
3. **Rendering.** The parse error stays the headline, byte-for-byte
 unchanged ([ADR-0290](./0290-syntax-error-position.md) caret layout included). After the human error
 lines — and after the multi-statement progress line ([ADR-0199](./0199-statement-progress-on-failure.md)) when
   there is one — stderr gains one line, preceded by a blank line:

   ```
   did you mean "query"?  -- run a statement session (oneshot, stdin, or shell)
   ```

   The blurb is the command's one-liner from its `CommandSpec`, first
   letter lowered, so the suggestion text cannot drift from the
   `Commands:` section of `--help`. The line is sanitized through
   `print_error` like every other stderr line.
4. **Scope.** Only the human stderr rendering gains the line. The JSON
 error envelope ([ADR-0194](./0194-cli-error-envelope-and-exit-codes.md)) is unchanged: it is a machine contract, and
   the suggestion text is derivable from the statement a client already
   has. Exit codes are unchanged (still 6 for a syntax error). The
   suggestion applies wherever `report_query_error` reports a statement
   failure — oneshot, stdin script, and the shell — because all three
   route through it.

## Alternatives considered

- **Pre-check the first positional against the command words before
  parsing.** Rejected: it changes what parses. A statement whose first
  token sits within edit distance of a command word (`info` is a fine
  variable name in a longer mistyped statement) would be rerouted or
  decorated even when the engine would have given the more precise error.
  Fallback-only keeps valid Cypher untouched by construction.
- **A separate hand-maintained alias table, like the flag matcher uses
  per command.** Rejected: it would be a second enumeration of the
  command set, free to drift from `help::COMMANDS`. The descriptor table
  already carries word, synopsis, and blurb; deriving candidates from it
 is the [ADR-0229](./0229-remove-system-ontologies-cypher-surface.md) pattern.
- **Suggest inside the JSON envelope too.** Rejected: the envelope is a
 stable machine contract ([ADR-0194](./0194-cli-error-envelope-and-exit-codes.md)); adding a field every consumer must
  tolerate is a protocol change with no consumer asking for it. The
  human line serves the human who mistyped.
- **Match against every keyword of the command grammar (subcommands like
  `fs-to-graph`, actions like `install`).** Rejected: the failure mode
  the ticket addresses is a mistyped first positional; second-position
  typos already fail inside the command's own parser, which has its own
 usage-error path ([ADR-0192](./0192-cli-unknown-flag-suggestions.md)).

## Consequences

- `cypherlite qu`, `cypherlite telemetr status`, and similar near-miss
  first positionals keep their parse error and gain one suggestion line
  naming the canonical command and its one-line synopsis.
- Adding, renaming, or feature-gating a command updates the suggestion
  set automatically through `help::COMMANDS`; the alias list is the one
  small addition site (`update` → `upgrade`).
- `suggest.rs` stays a pure string helper: no new dependencies, no new
  CLI surface, no `--help` or flag-table change.
- Related: [ADR-0069](./0069-cli-lifecycle-commands.md) (positional lifecycle verbs), [ADR-0192](./0192-cli-unknown-flag-suggestions.md) (flag
 did-you-mean), [ADR-0194](./0194-cli-error-envelope-and-exit-codes.md) (error envelope), [ADR-0290](./0290-syntax-error-position.md) (syntax-error
 positions), [ADR-0199](./0199-statement-progress-on-failure.md) (multi-statement progress), [ADR-0200](./0200-query-command-and-database-flag.md) (`query`
 verb), [ADR-0229](./0229-remove-system-ontologies-cypher-surface.md) (descriptor-table rendering pattern).

## Later references

[ADR-0518](./0518-command-did-you-mean-on-usage-errors.md) extends §1's applicability: the query-run usage-error exits
(unknown flag, unexpected argument) also carry the suggestion for the
pending statement, because their exit(2) fires before the engine — and
therefore before this ADR's fallback — ever sees the statement. Same
matcher, same candidates, same rendering; parsing stays authoritative
and the engine fallback itself is unchanged.
