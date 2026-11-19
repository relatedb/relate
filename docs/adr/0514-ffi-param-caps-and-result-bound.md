---
type: adr
area: surface
claim: 473040cc041f5735550baec5c9402730b7f55efc
name: 0514-ffi-param-caps-and-result-bound
status: proposed
tags:
  - ffi
  - memory
title: Fixed caps for the C ABI v2 params builder; the engine memory cap is the result-side bound
REFERENCES:
  - "[[adr/0257-query-input-limits]]"
  - "[[adr/0395-nested-evaluation-budget]]"
  - "[[adr/0440-follower-writer-toggle]]"
  - "[[adr/0462-value-nesting-depth]]"
  - "[[adr/0502-typed-c-abi-v2]]"
---

# ADR-0514: Fixed caps for the C ABI v2 params builder; the engine memory cap is the result-side bound

## Context

OPLANE's security review of the typed C ABI v2 (PR #722, [ADR-0502](./0502-typed-c-abi-v2.md);
requirements 00340363 and 00340357, CWE-400/CWE-20) asked for resource
limits on parameter construction and asked for a decision on the result
side.

The params builder ([ADR-0502](./0502-typed-c-abi-v2.md) §4) caps nesting at the engine's
`MAX_VALUE_DEPTH` ([ADR-0462](./0462-value-nesting-depth.md)) but had no other bound: a runaway loop over
a putter accumulates entries without limit, and a single `put_string`
accepts any length. Bolt has a natural bound — its 16 MiB chunked
message cap (`framing::MAX_MESSAGE_BYTES`) bounds every parameter set
the wire can carry; HTTP has the request-body limit. The C ABI has no
framing, so nothing bounds construction before the engine sees it.

The result side (the value walker, [ADR-0502](./0502-typed-c-abi-v2.md) §1) mints child handles
lazily one level at a time, but the result itself is already in memory:
its size is the query's doing, not the caller's, and the engine already
enforces its own bound there — the retryable processing memory cap (the
logical-workspace door, [ADR-0440](./0440-follower-writer-toggle.md)), whose refusal is a typed
`ResourceLimit` error.

Threat model: the ABI caller is an in-process embedder that can already
exhaust its own memory, so these caps are defense-in-depth, not a
security boundary — a runaway loop should fail with a typed refusal
instead of an OOM.

## Decision

1. **The params builder gains three fixed caps** (LABS-937,
   OPLANE_REQ-00340363/57), each enforced where it is checkable and each
   a defined refusal — the internal-stage error shape in `*error_json`,
   the builder unchanged — one past its limit:
   - `MAX_PARAM_ENTRIES = 100 000` — entries per one list/map container
     (checked in `insert`, the one choke point both container kinds
     share);
   - `MAX_PARAM_NAME_BYTES = 4096` — per parameter name (names are
     identifiers; 4 KiB is far above any legitimate one), refused by a
     bounded scan before the name's bytes are copied;
   - `MAX_PARAM_TOTAL_BYTES = 16 MiB` — a builder-wide byte budget, the
     one wire-message bound (`cypherlite_core::MAX_WIRE_MESSAGE_BYTES`,
     which Bolt's chunked framing also derives from): every entry
     charges its name, its value's bytes (strings their length, vectors
     their coordinates) and a fixed 16-byte overhead, across every
     nested container, so the aggregate is bounded, not just each
     container; `put_string` refuses an over-budget length before
     copying its bytes.
 Fixed constants, no knob — the house rule of [ADR-0257](./0257-query-input-limits.md)'s 4 MiB/500,
 [ADR-0395](./0395-nested-evaluation-budget.md)'s fixed budget and [ADR-0462](./0462-value-nesting-depth.md)'s depth cap.
   Documented in `cypherlite.h` and `docs/api/c/spec.md`, alongside the
   trusted-length contract: a `len`/`dim` argument and its buffer are
   trusted (Rust cannot detect a short or unterminated buffer); name
   buffers must be NUL-terminated.

2. **No ABI-level result cap.** The result side's bound is the engine's
   processing memory cap together with the walker's lazy one-level child
   minting: a query cannot materialize more than the workspace cap
   admits, and the walker never copies the result — it hands out handles
   into it. A separate ABI-level cap would be a second bound on the same
   fact with no failure mode of its own to catch (the engine's refusal is
   already a typed, retryable `ResourceLimit` riding the result's error
   shape).

## Alternatives considered

- **Per-container/per-value caps only, no builder-wide budget** —
  rejected (the first review round's shape): 100 000 entries x 16 MiB
  strings is ~1.6 TB in one container, and nesting multiplies it — the
  aggregate a CWE-400 runaway actually reaches, while every per-container
  gate stays green. The budget is one `Rc<Cell<u64>>` shared from the
  root to every child at minting, charged at `insert`'s single choke
  point.
- **An ABI-level result cap** (e.g. rows or cells per result) —
  rejected: see Decision 2; it restates the engine cap and would have to
  pick a number the engine's own cap already makes moot.
- **Configurable caps (a knob per cap)** — rejected: the fixed-constant
 house rule ([ADR-0257](./0257-query-input-limits.md), [ADR-0462](./0462-value-nesting-depth.md)); a knob on a defense-in-depth bound
  buys nothing the caller cannot already do by not looping.

## Consequences

- A putter loop that exceeds a cap fails deterministically with a typed
  refusal instead of an OOM; boundary tests accept each limit and refuse
  one past it.
- Programs that legitimately pass parameter sets above the caps (more
  than 100 000 entries in one container, names over 4 KiB, strings over
  16 MiB) are refused — none exist in the repo's clients, examples or
  tests, and Bolt could not carry a single string past 16 MiB anyway.
- The fuzz harness (LABS-937 item 2) drives the putters with arbitrary
  sequences; the caps guarantee it terminates in a refusal rather than
  unbounded growth.
- The 16 MiB string cap is not an independent number: it derives from
  `cypherlite_core::MAX_WIRE_MESSAGE_BYTES`, the one wire-message bound,
  which Bolt's chunked framing also derives from — the surfaces cannot
  disagree.
- This ADR is Proposed: the numbers are the reviewer's recommendation;
  Michael rules before they bind.
