---
type: adr
area: engine
claim: 3fc856367fb815c53e85661fe9d67616502b82f5
date: 2026-11-21
deciders: Relate maintainers
name: 0640-executor-expression-memo-tiers
status: proposed
tags:
  - executor
  - performance
title: Executor expression memo tiers, break-even rule, and the xxhash-rust dependency
REFERENCES:
  - "[[adr/0463-expression-allocation-budget]]"
---

# 0640: Executor expression memo tiers, break-even rule, and the xxhash-rust dependency

## Context

On a branching graph the arrival set of a stage is a small fraction of its
candidate stream, so a pure check is paid once per edge for one answer per
distinct input. A memo removes that repetition, but a probe costs time and a
table costs memory: a memo can lose to the expression it wraps. The executor
kernels already carry a memo module and a value-hash module, and the
workspace pins `xxhash-rust` for the latter, with no decision recorded for
either.

## Decision

1. **Purity gate.** Only expressions that are a function of their input key
   and of state fixed for the stage (one snapshot, the statement parameters,
   row-invariant deterministic bounds) are memoised. Anything reading the row,
   a nondeterministic call, or a write clause's own-write view is never
   wrapped.
2. **Four key tiers, cheapest first.** No key (a value constant for the
   stage, computed once); a direct-indexed table over id keys with no
   hashing; pointer identity of a stage-pinned borrowed slice; and a 128-bit
   structural hash of a value, used only for expressions whose cost dwarfs
   the hash.
3. **Break-even rule.** A memo is worth installing when the expected
   repetition covers its probe: about 2x for id keys (a direct index is
   nearly free), about 4-5x for hashed keys (the hash is a pass over the
   value). A class whose expression costs about a probe is never wrapped.
4. **Adaptive disable.** Each memo runs its first inputs unmemoised so small
   stages never allocate a table. At one review, after a window long enough
   for a large key domain to fill, a hit rate below the policy floor drops
   the table and bypasses the memo for the rest of the stage.
5. **Memory budget.** Table bytes are charged to the retained-container
   meter (ADR-0463), each memo has a byte ceiling, and all memos of a query
   share one live budget of 16 MiB. Past a ceiling or the budget the stage
   keeps answering from the function and hits are still served. A refusal or
   bypass never fails a query.
6. **Errors are never cached.** An erroring input propagates, so the first
   erroring input surfaces exactly as it would without a memo.
7. **Hashed keys.** Value keys use the full-content XXH3-128 hash from
   `xxhash-rust` (feature `xxh3` only, no default features, no transitive
   dependencies), seeded per process. A hit is served without an equality
   check, justified by a 2^-128 per-pair collision rate; a sampled hash is
   not offered because its collisions are structured. Entities, temporals,
   points and vectors and over-deep values are not hashed and bypass the
   memo. The hash is not cryptographic and is not designed against a
   determined adversary.

## Alternatives considered

- **Hash and verify by equality on every hit.** Rejected: pays a second walk
  of the value on every hit, removing most of the saving.
- **A 64-bit hash.** Rejected: collisions are plausible at query scale, and one is a wrong result.
- **Memo every pure expression.** Rejected: a probe for a cheap
  expression costs as much as the work.

## Consequences

- High-fan-in stages pay each distinct check once; low-repetition stages
  pay a counter increment, then bypass.
- Memo memory is bounded per memo and per query and metered.
- The dependency set gains `xxhash-rust` (small, dependency-free); later
  consumers of the value-keyed tier build on this decision rather than
  re-deciding the key scheme.
