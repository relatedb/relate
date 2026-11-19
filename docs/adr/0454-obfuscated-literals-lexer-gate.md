---
type: adr
area: cypher
claim: 5495436b8757cc3f6b8ed26a5021244869471ee7
name: 0454-obfuscated-literals-lexer-gate
status: proposed
tags:
  - parser
  - security
title: The obfuscated literal gates at the lexer
REFERENCES:
  - "[[adr/0371-preview-feature-gating]]"
  - "[[adr/0378-ontology-procedures-preview-gate]]"
---

# ADR-0454: The obfuscated literal gates at the lexer

## Context

The Neo4j acceptance corpus (ObfuscatedLiteralAcceptance) carries the
`******` obfuscated literal: with Neo4j's corresponding setting
enabled it parses to a
dummy `false` expression that only has to pass semantic analysis; without
the setting the query fails with an ordinary syntax error
(`Invalid input '******', expected: an expression`). Both behaviors are
pinned by the corpus — two scenarios assert the enabled parse, two assert
the disabled syntax error.

[ADR-0371](./0371-preview-feature-gating.md)'s preview-gate shape cannot express that pair: it always parses
the gated construct and fails closed in the front's detector loop with the
authored `preview feature '<name>' is not enabled` error — a
`SemanticError`, not the `SyntaxError` the disabled scenarios assert. The
one prior gate-location exception ([ADR-0378](./0378-ontology-procedures-preview-gate.md), `ontology`) moved the gate to
procedure dispatch; this one needs it before the token stream exists,
because the enabled and disabled forms differ in what the text lexes to.

## Decision

Register `obfuscated-literals` in the [ADR-0371](./0371-preview-feature-gating.md) preview registry
(`CYPHERLITE_PREVIEW=obfuscated-literals`), but gate it at the lexer: with
the feature enabled, a run of exactly six `*` lexes as one
`ObfuscatedLiteral` token that parses to a dummy `false` literal; with it
disabled, the run lexes as ordinary `Star` tokens and the parser fails
with its own syntax error — Neo4j's disabled-form behavior, not the
authored preview error. The front's detector loop carries the feature's
arm as `false` (nothing to detect: a statement that parsed never used the
disabled gate), and the front derives the lexer options from the store's
resolved preview set, so no second configuration surface exists.

## Alternatives considered

- Always parse and fail closed with the authored preview error
 ([ADR-0371](./0371-preview-feature-gating.md)'s uniform shape) — rejected: the error class is
  `SemanticError`, so the corpus's disabled-form scenarios (42I06,
  `SyntaxError`) fail; the gate must reproduce Neo4j's own syntax error.
- A dedicated engine setting (a new
  `CYPHERLITE_ENABLE_OBFUSCATED_LITERALS`-style env var) — rejected: a
  second configuration surface for one feature, where the existing
  `CYPHERLITE_PREVIEW` opt-in already resolves per-store at open and is
  documented.
- Parsing `******` unconditionally — rejected: the disabled form is part
  of the contract; an always-on parse makes the two disabled-form
  scenarios fail.

## Consequences

- The registry gains one feature whose gate lives in the lexer, a second
 recorded exception to [ADR-0371](./0371-preview-feature-gating.md)'s detector-loop location (after
 [ADR-0378](./0378-ontology-procedures-preview-gate.md)'s dispatch-gate exception); the registry row's doc comment and
  AGENTS.md name it.
- `cypherlite-parser` gains `ParseOptions` / `parse_with_options` — the
  threading path for lexer-affecting gates — with `parse` as the
  all-gates-closed default every other caller keeps.
- The disabled form's error text is the parser's ordinary
  `unexpected token "*"` message, not the authored preview text; that is
  the pinned Neo4j behavior, accepted deliberately.
