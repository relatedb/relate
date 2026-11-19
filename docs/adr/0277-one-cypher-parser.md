---
type: adr
area: cypher
date: 2026-09-30
deciders: CypherLite maintainers
name: 0277-one-cypher-parser
status: proposed
tags:
  - parser
title: One Cypher parser
REFERENCES:
  - "[[adr/0046-cypherlite-parser-memorydb-runtime]]"
---

# 0277. One Cypher parser

- **Renumbered**: from 0152 (shared-number cleanup)

## Context

[ADR-0046](./0046-cypherlite-parser-memorydb-runtime.md) made `cypherlite-parser` the product front-end and kept a second
parser in `cypherlite-ir` behind `CYPHERLITE_PARSER=planner-ir` (later
`=ir`) for diagnostics. The planner and executor still take
`cypherlite_ir::Query`. The product path already lowers the parser AST into
that IR in `cypherlite-front`. The second grammar only remained so tests
and the hatch could build a `Query` without going through that lowerer.

Two grammars drift. The hatch also skipped the parser's own limits except
for the size check bolted on in front of it.

## Decision

1. `parse_and_lower_statement` only calls `cypherlite_parser::parse` and
   `lower_statement`. `CYPHERLITE_PARSER` is not read.
2. `cypherlite-ir` no longer parses. `lex.rs`, `parse.rs`, and `parse_query`
   are removed. The IR AST, clause walks, and aggregate-name set stay.
3. Tests that used to call `parse_query` lower through
   `cypherlite_front::parse_and_lower`. There is no test-only parser.
4. The lowerer is split by clause family (clauses, patterns, expressions).
   It is still the boundary into planner IR. The planner is not rewritten
   onto the parser AST.

## Consequences

- [ADR-0046](./0046-cypherlite-parser-memorydb-runtime.md) Decision item 1's diagnostic parser is withdrawn. The later note
  on that ADR records this.
- A query the product parser rejects is rejected everywhere, including
  planner and executor tests.
