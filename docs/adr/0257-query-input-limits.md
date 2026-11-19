---
type: adr
area: engine
date: 2026-09-22
deciders: CypherLite maintainers
name: 0257-query-input-limits
status: proposed
tags:
  - security
title: "Query input limits: max query-text size and max nesting depth"
REFERENCES:
  - "[[adr/0042-graph-memory-limit]]"
  - "[[adr/0046-cypherlite-parser-memorydb-runtime]]"
---

# 0257. Query input limits: max query-text size and max nesting depth

- **Renumbered**: from 0079 (shared-number cleanup)

## Context

The engine places no limit on the size or shape of an incoming Cypher
query text. Two failure modes were verified in a security review
(LABS-69/70/81):

- **Parse-time memory amplification.** There is no size cap before
  lexing. An 8 MiB input consisting only of `,` characters produces
  ≥520 MB of tokens — a ~65–90× memory amplification — before any
  parsing error surfaces.
- **Unbounded recursion → process abort.** The parser is
  recursive-descent and the front-end lowering/validation walkers are
  recursive. ~5000 nested parentheses abort the parser (SIGABRT), and
  `RETURN NOT NOT …×1000 true` aborts the production binary in
  front-end lowering. When the engine is embedded via the C ABI, a
  stack-overflow abort kills the host process; it cannot be surfaced
  as a recoverable error.

[ADR-0042](./0042-graph-memory-limit.md) caps stored-graph growth at commit time. It does not bound
query input: the amplification and recursion above happen before any
graph data is written, so [ADR-0042](./0042-graph-memory-limit.md) never engages.

All query paths — `cypherlite-front` ([ADR-0046](./0046-cypherlite-parser-memorydb-runtime.md)), the TCK harness, the
engine, and Bolt — funnel through a single choke point,
`cypherlite_parser::parse` (`crates/cypherlite-parser/src/parser.rs`).
A size cap there covers every entry point with one check.

## Decision

Two hard input limits, both producing **typed errors** (never aborts,
never unbounded allocation):

1. **Max query-text size: 4 MiB (4 194 304 bytes).** Enforced in
   `cypherlite_parser::parse`, *before lexing*. The single choke point
   covers the Rust API, the C ABI, the CLI, the TCK, and Bolt with one
   check. 4 MiB is far above any legitimate hand-written or generated
   analytical query we know of, and bounds worst-case token
   amplification at ~65–90× to ~270–380 MB of transient allocation —
   large but finite and survivable, versus unbounded today. (The lexer
   amplification constant can be reduced later as perf work; see
   Alternatives.)

2. **Max nesting depth: 500.** Enforced in two places: the parser
   (expression/primary nesting) and the front-end lowering boundary
   (statement/structural nesting). The value must sit comfortably
   *below* the empirically observed abort thresholds — ~1000 nested
   `NOT`s in front-end lowering and ~5000 nested parens in the parser —
   with margin, because debug builds use substantially larger stack
   frames and must not abort under test. 500 gives ~2× headroom against
   the tightest observed threshold (~1000) and ~10× against the parser
   threshold, on release frames; on debug frames the margin is smaller
   but still safe. A cap of 1000 would sit *at* an observed abort
   threshold and was rejected. Implementing nodes must add tests that
   exercise exactly the limit (accepted) and the limit + 1 (typed
   error), in both debug and release CI lanes.

Both values are documented in the public-facing spec for the affected
surfaces so that generators of Cypher know the contract.

## Alternatives considered

- **No limits; rely on the [ADR-0042](./0042-graph-memory-limit.md) memory limit.** Rejected. [ADR-0042](./0042-graph-memory-limit.md)
  caps stored-graph growth at commit; it never engages for parse-time
  token amplification or recursion, both of which happen before any
  commit.
- **Per-clause or per-construct limits.** Rejected. Inconsistent UX
  (different constructs fail at different depths), and a much larger
  surface to specify, document, and test than two global numbers.
- **Streaming lexer / token interning to reduce amplification instead
  of a hard cap.** Rejected. It reduces the amplification constant but
  bounds nothing — an adversarial input still scales memory without
  limit — and it is a much larger change. It can land later as
  performance work (LABS-58); the hard cap is required regardless.
- **Rewrite the recursive-descent parser (and walkers) to be iterative
  instead of capping depth.** Rejected. A large rewrite of the parser
  and front-end walkers; a depth cap achieves process safety now. If an
  iterative rewrite ever lands, the cap can be revisited, but real
  queries never legitimately approach depth 500.

## Consequences

- Oversized or over-nested queries get typed errors instead of process
  aborts or unbounded memory growth — critical when the engine is
  embedded via the C ABI.
- Generated/programmatic queries must stay under 4 MiB and 500 nesting
  depth; both values are part of the documented public contract.
- The Bolt message-chunk accumulation cap is a separate concern
  (LABS-76) and is not covered here.
- The implementing nodes (parser-input-limits,
  front-validation-hardening) add tests at exactly these limits,
  including limit + 1 rejection, in debug and release builds.
- Top-level first-error precedence changed: the scope-free validator
  battery (including the depth guards) now runs before the scope
  validators, so a query that is invalid under both may report a
  different first error than before.
