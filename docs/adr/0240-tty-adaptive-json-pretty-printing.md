---
type: adr
area: cli
name: 0240-tty-adaptive-json-pretty-printing
status: accepted
tags:
  - output
title: TTY-adaptive JSON pretty-printing and `--pretty`
REFERENCES:
  - "[[adr/0107-jsonl-default-for-piped-output]]"
  - "[[adr/0194-cli-error-envelope-and-exit-codes]]"
  - "[[adr/0227-ontology-document-format]]"
---

# ADR-0240: TTY-adaptive JSON pretty-printing and `--pretty`

## Context

(Numbered [ADR-0227](./0227-ontology-document-format.md) when written; renumbered to avoid a collision
with the ontology ADR [ADR-0227](./0227-ontology-document-format.md) that landed on main first.)

LABS-233 (agent-CLI audit, `docs/agent-cli-audit.md` finding 6). The `json`
output mode always pretty-printed: a one-scalar result cost four lines. The
consumers of piped JSON are programs — `jq`, agent harnesses, model contexts
— for whom whitespace is pure token cost (the audit's compactness principle:
compact at the source, pretty on demand). Meanwhile a human running
`--json` at a terminal benefits from the indentation. [ADR-0107](./0107-jsonl-default-for-piped-output.md) already made
the *mode* default TTY-adaptive (`table` on a terminal, `jsonl` otherwise);
the *rendering* of an explicitly chosen `json` mode was not.

## Decision

1. **`json` mode renders compact when stdout is not a terminal**, indented
 when it is. The CLI resolves this once, next to the [ADR-0107](./0107-jsonl-default-for-piped-output.md) mode
   default: `Formatter.pretty = stdout.is_terminal() || --pretty`.
2. **`--pretty` forces indented JSON** regardless of the terminal
   detection (a pipe into a file a human will read). It is a query-run flag,
   double-dash only, meaningful only to `json` mode; `jsonl` and `envelope`
   are always compact and ignore it.
3. **`Formatter.pretty` defaults to `true`** in `cypherlite-formatter`, so
   existing library callers keep the historical pretty output; the CLI is
   the surface that adapts to the terminal.
4. Docs, help text, and tests land in the same change.

## Alternatives considered

- **Always compact (`--json` changes meaning for everyone)** — rejected:
  a human asking for `--json` at a terminal gets an unreadable wall of
 text; the TTY split keeps both audiences served, matching [ADR-0107](./0107-jsonl-default-for-piped-output.md).
- **A `--compact` flag instead, pretty by default** — rejected: the default
  should already be right for the common consumer (a pipe), not require
  every harness to opt out of whitespace.
- **Compact `jsonl` only, drop pretty `json` entirely** — rejected: the
  single-document JSON form is useful for tooling that wants one value
  (`jq -s` gymnastics aside); this ADR changes only its whitespace.

## Consequences

- Piped `--json` output is one line; byte-identical information, less
  whitespace. Golden tests that pinned multi-line pretty JSON under a pipe
  updated to the compact form (or pass `--pretty`).
- `Formatter` gains a public `pretty` field — an additive change to the
  formatter crate's API.
- The [ADR-0194](./0194-cli-error-envelope-and-exit-codes.md) error envelope on stdout (`{"error":…}`) was already
  compact and is unchanged.
