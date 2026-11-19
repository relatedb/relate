---
type: adr
area: cypher
claim: 3129bc81b73641df5c37079fd0d9fa4ba9815577
name: 0619-source-level-unicode-escapes
status: proposed
tags:
  - dialect
  - parser
title: Source-level unicode escapes in Cypher text
REFERENCES:
  - "[[adr/0505-parse-pipeline-token-and-string-representation]]"
---

# 0619. Source-level unicode escapes in Cypher text

## Context

The engine accepts the GQL/Java Unicode whitespace set between tokens
(NBSP, em quad, line separator, NEL, vertical tab), verified against the
reference server. That server also decodes `\uXXXX` escapes in Cypher
*source text* before classifying a token: an escape spelling whitespace
is whitespace, an escape inside an identifier is that character, and an
escape can spell a keyword letter-for-letter.

The engine decodes escapes only inside string literals. So the same
query written with the escape spelling — the only way some characters
can be written at all in a text-first corpus, whose embedded-text blocks
split on those very separators — is rejected with
`unexpected character: \`. The whitespace rows for the line separators
are ungreenable by any other means: the characters cannot appear in the
corpus format, and the escape spelling is what the language's own
documentation uses for them.

## Decision

The lexer decodes a well-formed `\uXXXX` (backslash, `u`, four hex
digits, a scalar value) in the two positions the widening needs:

1. **Whitespace.** Between tokens, an escape whose character is
   whitespace is consumed as that whitespace.
2. **Identifiers.** An escape whose character starts or continues an
   identifier is part of that identifier's text — synthesized as owned
   text from the first escape on, the escape-free prefix staying
   borrowed. Keywords resolve through the same lookup, so an escaped
   keyword letter spells the keyword.

Everything else is unchanged, and that is the recorded boundary: string
literals keep their own escape rules (including surrogate-pair
decoding), backtick names stay verbatim, comments stay literal,
parameters and numbers reject an escape, and an escape whose character
would open any other token (a quote, a digit, punctuation) is rejected
as the historical `unexpected character:` error — a typed rejection,
never a silent half-decode. Malformed escapes and surrogate values are
not escapes and keep the same rejection.

## Alternatives considered

- **Decode the whole input before lexing**, as the reference server
  does. Rejected: it re-decodes string literals that already carry
  their own escape semantics (surrogate pairs would become lone
  surrogates), and it forces an owned copy of every query to outlive
  the borrowed tokens ([ADR-0505](./0505-parse-pipeline-token-and-string-representation.md)) for a spelling almost no query uses.
- **Gate behind `RELATE_PREVIEW`.** Rejected: this completes the
  already-ungated Unicode-whitespace acceptance rather than opening a
  new syntax family — the escape spelling is that same work's own
  notation for its separators.
- **Correct the corpus instead.** Rejected: the line separators at
  issue are terminators for the corpus format's embedded-text blocks,
  so the escape spelling is the only expressible form; the rows are
  correct as written and the engine is the side that moved.

## Consequences

Queries that spell whitespace or identifier characters as `\uXXXX`
become accepted, matching the reference server. Identifiers carrying an
escape allocate their text; every escape-free path stays borrowed and
byte-identical. Positions stay source positions: an escape occupies its
six bytes on the line it is spelled on, so the line and column a token
reports still name a place in the text the caller wrote — the invariant
the token-to-byte-offset helper depends on. The rejection boundary is
stricter than the reference server's general substitution (an escaped
quote or digit is rejected rather than substituted), recorded here and
asserted by test.
