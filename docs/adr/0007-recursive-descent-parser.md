---
type: adr
area: cypher
date: 2026-08-28
deciders: MemoryDB maintainers
name: 0007-recursive-descent-parser
status: accepted
tags:
  - parser
title: Cypher is parsed with a hand-written recursive-descent parser
REFERENCES:
  - "[[adr/0056-opencypher-tck-language-contract]]"
---

# 0007. Cypher is parsed with a hand-written recursive-descent parser

## Context

[ADR-0056](./0056-opencypher-tck-language-contract.md) requires OpenCypher. We need a parser that can grow with TCK
scenarios, emit useful errors, and stay a small dependency. Official
openCypher publishes ANTLR, EBNF, and ISO BNF grammars.

## Decision

`memorydb-parser` is a hand-written lexer plus recursive-descent parser
whose grammar follows the openCypher structure (clauses, patterns,
expressions with Cypher precedence). The AST is the input to
`memorydb-planner`. We do not vendor ANTLR or generate from the official
grammar in v1.

## Alternatives considered

- **pest** — fast to start, ugly for Cypher’s operator precedence, keywords
  vs identifiers, and `|` in relationship types. Rejected for v1 control
  over errors and TCK syntax failures.
- **lalrpop** — closer to a spec grammar, heavier build, harder error
  recovery. Rejected until the language surface is stable.
- **ANTLR runtime / generated parser** — Java-centric official grammar,
  extra runtime. Rejected for a lightweight Rust engine.
- **An existing Cypher crate as a black box** — unknown TCK alignment and
 license/maintenance risk. Rejected; we own the AST ([ADR-0056](./0056-opencypher-tck-language-contract.md)).

## Consequences

- Parser bugs are fixed in-tree; grammar extensions are code, not a
  generator step.
- A later move to lalrpop/pest is a new ADR.
- SyntaxError TCK scenarios can map to parser error codes as we fill them in.
