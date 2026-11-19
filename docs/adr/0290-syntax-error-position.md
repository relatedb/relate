---
type: adr
area: cli
date: 2026-10-09
deciders: CypherLite maintainers
name: 0290-syntax-error-position
status: proposed
tags:
  - errors
  - output
title: "Syntax-error position: caret on stderr, `column` and `source` in the envelope"
REFERENCES:
  - "[[adr/0103-engine-surfaces-testing-layout]]"
  - "[[adr/0194-cli-error-envelope-and-exit-codes]]"
---

# 0290. Syntax-error position: caret on stderr, `column` and `source` in the envelope

- **Renumbered**: from 0198 (shared-number cleanup)

## Context

The agent-CLI audit (`docs/agent-cli-audit.md`, finding 12) found that a syntax
error loses its caret. The parser renders

```text
parse error: unexpected token "RETRN"
  RETRN 1
  ^
```

but `print_error` flattens every newline to a space (it exists so engine text
cannot inject terminal control sequences, and so a multi-line message stays on
one line), leaving `unexpected token "RETRN"   RETRN 1   ^`, where the caret no
longer lines up under the offending token. The [ADR-0194](./0194-cli-error-envelope-and-exit-codes.md) error envelope carries
the same flattened message and no position.

The position does not reach the CLI as data. The engine's public
`Error::Syntax(String)` is built from `ParseError`'s `Display` text in the front
end (`Error::syntax(e.to_string())`), and changing that public type is a
cross-binding API change out of proportion to this finding. What the CLI has is
the rendered text, whose two shapes are fixed by the parser:

1. `parse error: <message>\n  <excerpt>\n  <caret>` when the parser knows the
   source line, optionally preceded by a TCK detail tag
   (`syntax error: <Detail>: parse error: …`);
2. `parse error: <message> at line L, col C` when it does not.

The excerpt is the statement's own line, so a position derived from it is
relative to the statement, not to the script the CLI split it from. The first
shape carries no line number.

## Decision

1. **stderr shows the caret.** For a syntax error whose message has the first
   shape, stderr prints the first line exactly as before (up to and including the
   message, so existing greps keep matching), then the excerpt and the caret on
   their own lines, indented two spaces, as the parser rendered them:

   ```text
   syntax error: parse error: unexpected token "RETRN"
     RETRN 1
     ^
   ```

   Every character of the excerpt passes through the same sanitizer as any other
   engine text: control characters are removed and the excerpt stays one line, so
   the safety property of `print_error` is unchanged. The caret is rebuilt from
   the column rather than trusted verbatim, so it always lines up under the
   sanitized excerpt.
2. **The envelope gains optional fields.** For a syntax error, `error.column`
   (1-based, in characters of the echoed line) and `error.source` (the echoed
   line, sanitized) are added when the message has the first shape; `error.line`
   and `error.column` are added from `at line L, col C` when it has the second
   shape and `L` is at least 1. A field is absent, never `null` or `0`, when the
   position is unknown. `message` keeps its current flattened text. The fields
   are additive: existing ones are never renamed.
3. **`line` is statement-relative.** The parser numbers lines within the
   statement it was given, and the first shape does not carry one at all, so the
   envelope does not invent a script line number. A caller that needs the
   script position of a statement tracks statement boundaries itself (per-
   statement progress, LABS-240, is the place that can report it).
4. **Recovery is by shape, and falls back.** Recognition is a single function
   over the engine text. Anything that does not match either shape is reported
   exactly as today: flattened message, no position, no extra stderr lines.
5. **Scope.** Syntax errors from the query run only, as in [ADR-0194](./0194-cli-error-envelope-and-exit-codes.md). Other
   error kinds, the lifecycle commands and the interactive shell are unchanged.

## Alternatives considered

- **Carry `ParseError` through the public `cypherlite::Error`.** Rejected: it
  changes the engine's public error type and every binding that matches it, for
  a presentational fix. It remains the right long-term home for the position and
  would let this recovery be deleted.
- **Depend on `cypherlite-parser` and re-parse the failing statement.**
 Permitted for a surface ([ADR-0103](./0103-engine-surfaces-testing-layout.md), R3), and it would yield a typed
  `ParseError`. Rejected: the CLI would parse every failing statement a second
  time and must reproduce exactly what the front end did to it first (a
  `CYPHER n` prefix, administration commands, detail tags), so a mismatch would
  report a different position than the engine's; and it still needs the engine
  text to decide which `Error::Syntax` values are parse errors at all, since
  validation errors (an undefined variable) are `Syntax` too.
- **Stop flattening newlines in `print_error`.** Rejected: that sanitizer is the
  single route for every interpolated diagnostic (argv, engine text, stdin
  fragments), and a blanket exemption would reopen the terminal-injection hole it
  closes.
- **Report a script line number.** Rejected: the excerpt form has no line, and
  the line in the second shape is statement-relative; a made-up number would be
  worse than none.
- **Put the excerpt and caret into `message`.** Rejected: it would change the
  envelope's `message`, which is contract; separate fields are additive.

## Consequences

- A syntax error's caret lines up again on stderr, and an agent can read the
  failing column and the line of source without parsing English.
- The envelope is assembled from a JSON literal plus optional keys. A typed,
  serializable body (one struct whose conditional fields skip when absent) would
  make that schema compiler-checked; it is a follow-up, held back here because
  the per-statement progress work (LABS-240) changes the same function.
- The CLI depends on the parser's rendered shape. A test pins both shapes against
  the real parser's output, so a change to the rendering fails the build instead of
  silently dropping the position.
- `docs/api/cli/spec.md` documents the two new fields and the stderr form.
