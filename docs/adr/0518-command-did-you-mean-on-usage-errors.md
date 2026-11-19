---
type: adr
area: cli
claim: 6f77dc329a909082c162abd012dc35e28a288a92
name: 0518-command-did-you-mean-on-usage-errors
status: proposed
tags:
  - did-you-mean
title: Command did-you-mean on query-run usage errors
REFERENCES:
  - "[[adr/0069-cli-lifecycle-commands]]"
  - "[[adr/0104-cargo-feature-bundles]]"
  - "[[adr/0192-cli-unknown-flag-suggestions]]"
  - "[[adr/0200-query-command-and-database-flag]]"
  - "[[adr/0230-command-did-you-mean-on-parse-errors]]"
---

# ADR-0518: Command did-you-mean on query-run usage errors

## Context

[ADR-0230](./0230-command-did-you-mean-on-parse-errors.md) added the command did-you-mean as a **fallback after a failed
parse**: a first positional that is not a command word is a Cypher
statement, and when the engine rejects it with a `Syntax` error, stderr
gains `did you mean "serve"?  -- <synopsis>`.

The fallback never runs when the failure is a **usage error** instead of
a parse error. The query-run argument walk in `flags.rs` exits(2) on an
unknown flag or an unexpected extra positional *before* the statement
ever reaches the engine, so the same mistyped command word loses its
suggestion the moment parameters follow it:

```
$ cypherlite service
syntax error: parse error: unexpected token "service"
  service
  ^

did you mean "serve"?  -- host the HTTP Query API and MCP surfaces on a database

$ cypherlite service --bolt 7687
Error: unknown flag: --bolt
Usage: cypherlite [OPTIONS] [--database PATH] [QUERY]
Run 'cypherlite --help' for more information.
```

The flag suggestion ([ADR-0192](./0192-cli-unknown-flag-suggestions.md)) that this exit does carry cannot help
here: its candidate list is the exit's own usage scope — the query run's
flags — while `--bolt` is a perfectly valid `serve` flag. The user's
mistake is the command word, and no suggestion names it.

## Decision

The two query-run usage-error exits that fire while a statement is
pending — the unknown-flag exit and the unexpected-argument exit — carry
the [ADR-0230](./0230-command-did-you-mean-on-parse-errors.md) command suggestion for the pending statement:

1. **Applicability.** When the exit fires and a statement is already
   pending (the first positional, or `--command`'s value), the
   suggestion runs `suggest::suggest_command` on that statement — the
   same matcher, the same candidate table (`help::COMMANDS` plus the
   `update` alias), the same rendering (blank line, then
   `did you mean "<word>"?  -- <synopsis>` through `print_error`). No
   statement pending (`--bolt 7687 service`, flag first), or a first
   word that is not a plausible command typo (`RETURN 1 --bolt`) — no
   suggestion, byte-for-byte today's output.
2. **Parsing stays authoritative.** Nothing new is rejected or rerouted;
 the suggestion is printed by the exit that already fired. [ADR-0230](./0230-command-did-you-mean-on-parse-errors.md)'s
   rejection of a pre-check stands: a statement that would parse is
   never decorated, because no exit fires for it.
3. **One suggestion slot.** The exit carries at most one did-you-mean
   line. When the pending statement matches a command, the command line
   replaces the flag suggestion — the flag candidate list belongs to the
   exit's usage scope, which is the wrong scope for the command the user
   meant, so a flag suggestion beside a command suggestion can only add
   noise (`servcie --databse x` suggests `serve`; the retry then gets
   `--database` from the flag matcher on its own exit). No command match
 — the flag suggestion behaves exactly as before ([ADR-0192](./0192-cli-unknown-flag-suggestions.md)).
4. **Scope.** Exit codes are unchanged (still 2 for usage errors), the
   JSON envelope gains no field, and the engine-error fallback
 ([ADR-0230](./0230-command-did-you-mean-on-parse-errors.md) §1) is untouched. Lifecycle verbs' own parsers are
   untouched: their exits have no pending statement, so there is nothing
   to suggest.

## Alternatives considered

- **Pre-check the first positional against the command words before the
  walk, refusing early with a suggestion.** Rejected for the same reason
 [ADR-0230](./0230-command-did-you-mean-on-parse-errors.md) rejected it: it changes what parses. A statement within edit
  distance of a command word would be refused even when the engine
  would accept it; the fallback-only shape keeps valid Cypher untouched
  by construction.
- **Print both the flag and the command suggestion when both match.**
  Rejected: the two lines answer different tokens and the flag one is
  computed from the wrong candidate list whenever the command word is
  the actual mistake. One slot, command first.
- **Also carry the suggestion on `take_value`'s "requires a value" exits
  and the other in-walk value errors.** Rejected for this change: those
  errors already name the exact flag and its fix (supply the value), and
  threading the pending statement through every `take_value` call site
  balloons the diff for a strictly weaker hint. The exit shape added
  here extends to them unchanged if that is ever wanted.
- **Check whether the unknown flag is valid for the suggested command
  and say so.** Rejected: it couples the query-run walk to every
  command's flag table for a second line of marginal value; the
  suggestion already names the retry that will accept the flag.

## Consequences

- `cypherlite service --bolt 7687` and `cypherlite service extra` keep
  their usage error as the headline and gain the `serve` suggestion;
 `cypherlite service` (no parameters) is unchanged from [ADR-0230](./0230-command-did-you-mean-on-parse-errors.md).
- `usage_error`'s suggestion parameter becomes one `UsageSuggestion`
  slot (flag text or command line) instead of an `Option<&'static str>`;
  every other call site passes `None` or a flag suggestion and is
  mechanically unchanged.
- `suggest.rs` keeps one renderer for the suggestion line, shared by the
  engine-error fallback and the usage-error exits, so the two cannot
  drift.
- Related: [ADR-0230](./0230-command-did-you-mean-on-parse-errors.md) (command did-you-mean, engine fallback — amended
 here), [ADR-0192](./0192-cli-unknown-flag-suggestions.md) (flag did-you-mean), [ADR-0069](./0069-cli-lifecycle-commands.md)/[ADR-0200](./0200-query-command-and-database-flag.md) (positional
 verbs, the `query` verb), [ADR-0104](./0104-cargo-feature-bundles.md) (feature bundles: compiled-out
  commands are not candidates).
