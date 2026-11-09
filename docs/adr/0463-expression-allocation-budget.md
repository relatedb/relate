# ADR-0463: A per-allocation expression build cap

Status: Proposed
Claim: 0bcfafbf52360ba687499fa22de021d8c6cb3ab2
Later references: ADR-0499 keeps the vals container's per-row charge exact for every fold (a transient fold's vals hold the payloads until the fold completes; a fixed-width estimate was tried and rejected — constructed payloads nested one level escape it) and removes the measured per-row overhead by inlining the walk chain instead. The collect/comprehension/UNWIND/projection totals above are unchanged. ADR-0555 makes the retaining-container charge the price of a declared breaker only: an operator output that grows with intermediate rows is not a container to charge but a shape to stream.

## Context

Expression evaluation has no bound on the size of the values it builds.
A few hundred bytes of query text can ask for an arbitrarily large value:

```cypher
RETURN range(1, 1000000000)                       // one billion Ints
RETURN reduce(acc = [0], x IN range(1, 60) | acc + acc)
RETURN [x IN range(1, 1000000) | x]                // comprehension output
RETURN string.join(range(1, 1000000), ',')          // joined string
```

Each of these allocates a container or string whose size is a function of
inputs the client supplies — `range`'s element count is exact before the
first push, a list/string concatenation's result is the operands' combined
length, a comprehension's output is bounded by its source, and a
`reduce`/`allReduce` accumulator can double every iteration. The ADR-0257
input limits bound static shape (4 MiB of text, parse depth 500), not a
dynamic result size: a tiny `range(1, 1000000000)` passes every static
check. ADR-0395 bounds *nested statement passes*, not the bytes one pass
allocates. ADR-0042's capacity engages only for stored-graph writes at
commit, and ADR-0352 deliberately has no per-query byte meter.

The result is an available denial of service on every surface: an embedded
Rust/C-ABI host, Bolt, the HTTP Query API, the CLI in `--read-only`, and
MCP all evaluate expressions before any result is produced. A query that
allocates gigabytes dies on the allocator, not as a controlled refusal.

## Decision

A **per-allocation** cap on the values expressions build. The engine refuses
the allocation when the one value about to be built is larger than the cap;
it does not keep a statement-wide running total.

### 1. Per-allocation enforcement (stateless)

`ExecGraph` carries the store's configured cap (`expression_limit`, inherited
from the store-level `StoreServices` bundle so every namespace/session sees
one value). Every bounded site computes the size of the allocation it is
about to make and calls `check_expression_allocation(bytes)`:

```rust
if limit != 0 && bytes > limit {
    return Err(Error::capacity_expression(bytes, limit));
}
```

The size is the actual allocation: `range` knows its exact element count
before the first push, a list/string concatenation knows its operands'
combined length, a comprehension has `with_capacity(source.len())`, a
`collect`/`UNWIND` buffer is charged as one buffer, a reducer's new
accumulator value is measured after it is built, and a growing string is
checked against its new length before `push_str`. For incrementally grown
containers the same check applies at each `with_capacity`/growth point
against the new capacity's byte size; where a growth point is not
observable (the regex crate's replacement), the site charges a conservative
upper bound before the call (see §3). Nothing is refunded and nothing
accumulates: a value that is dropped is not subtracted from anything,
because nothing was added to a total.

This is deliberately **not** a cumulative per-statement meter. A cumulative
total is inaccurate — it counts values the statement already dropped — and
it false-positives long streaming statements that build many small values.
The measured heartbeat fixture (30 × `UNWIND range(1,100000)` stages) built
about 120 MiB cumulatively while never holding more than a few MiB at once;
the cumulative form refused it, the per-allocation form runs it unchanged.
That fixture and the CLI `--timeout` runaway are the regression pins.

**Retaining containers are the one place a running total is exact.** A
`collect()` vals vector, a list/map/pattern-comprehension output,
`UNWIND`'s materialized `next` rows, and a projection's output rows hold
every element until the container drops — nothing in them is dropped earlier, so their running retained size
can never over-count, and it is the quantity that actually bounds memory.
Those sites call `charge_retained(&mut total, added)`: `total` starts at the
container's buffer size, each pushed element adds its owned payload
(`workspace::owned_heap`, saturating; shared/borrowed payloads add nothing),
and the check is against the container's **total** as it grows. The total
resets when that construction completes and the value flows on; this is a
per-construction scope, not a statement-wide meter. Transient,
non-retaining charges stay per-allocation only. A comprehension of one
million ~200-byte strings therefore fails as a typed refusal (~256 MiB
retained) even though every individual string and the slot buffer are far
under the cap. The seam owns the whole protocol: `ExecGraph::retain_slots` /
`retain_bytes` return a `RetainedTotal` guard whose `add_owned`, `add_bytes`
and `add_allocation` grow and check one container's total, and
`workspace::value_buffer_bytes` / `retained_rows_bytes` are the one formulas
the guard and the combined-charge sites read.

### 2. The cap, the knob, and the measured default

The default cap is **256 MiB** (`EXPRESSION_BUDGET_BYTES = 256 << 20`).
The knob is the same ladder as the other memory limits:

- `--expression-memory-limit SIZE` (query session and `serve`),
- `CYPHERLITE_EXPRESSION_MEMORY_LIMIT`,
- `OpenOptions::expression_memory_limit` for embedded hosts.

`SIZE` uses the shared memory-limit grammar (`bytes`/`mb`/`mib`/`gb`/`gib`),
plus the spelled opt-out **`none`** (and `0`), which disables the cap for
known bulk pipelines. Unset keeps the generous default — never silently
off; an explicit value may raise or lower it. A malformed value fails the
open before a store is leased, like the other memory knobs.

The default is measured, not a placeholder. The canonical bulk shapes
(release build; telemetry read from `PROFILE`'s construction-work column,
which counts every allocation built, and the single-allocation size from the
same buffer formula) are:

| Shape | Largest single allocation | Cumulative built (telemetry) |
|---|---|---|
| `UNWIND` 10 000 rows + per-row expression | 786 456 B (0.75 MiB) | 1 572 912 B (1.5 MiB) |
| `UNWIND` 100 000 rows + per-row expression | 6 291 480 B (6.0 MiB) | 12 582 960 B (12.0 MiB) |
| `UNWIND` 1 000 000 rows + per-row expression | 50 331 672 B (48.0 MiB) | 100 663 344 B (96.0 MiB) |

The 1M-row shape is the binding watermark: its single 48 MiB buffer is the
largest allocation a legitimate bulk load produced, and the retained total
of its materialized rows (one value slot + row header + `next` slot per row)
is ~107 MiB. 256 MiB leaves 2.4× headroom over that retained total and
still admits a ~4.2M-row `UNWIND` buffer; 128 MiB would sit uncomfortably
close to the 1M-row retained total, and 512 MiB buys no shape the measured
workloads need. The graph-examples pull path is a batched LOAD/import path whose
per-row constructions are far smaller than this watermark; it was not
executed here (the differential harness needs a Neo4j reference container),
so the 1M `UNWIND` shape is the conservative binding measurement.

The knob is the operator escape hatch and the message names it: the default
is a protective guard rail, not a native database limit, and a refusal is
one flag away from being raised.

### 3. Charge sites

| Site | Where | Method |
|---|---|---|
| `range()` | `eval/function/list.rs` | exact `size × size_of::<Value>()` buffer before `Vec::with_capacity` |
| `+` / `\|\|` list+list, list+scalar, scalar+list, map+map, string+string | `eval/expr.rs` (`add`/`concat`, charged in the allocating arm) | exact result buffer/string before building |
| `coll.zip` | `eval/collection.rs` | outer buffer + `len × arity` inner buffers |
| `coll.flatten` | `eval/collection.rs` | one walker counts, then the output buffer |
| `string.join` | `eval/function/string.rs` | pre-scan part lengths + delimiters |
| `split` | `eval/function/string.rs` | input bytes + one value slot per byte + 1 |
| `replace` | `eval/function/string.rs` | occurrence pre-scan, then input + occurrences × replacement |
| `string.interp` | `eval/function/string.rs` | the rendered part and the growing result length before the push |
| `string.regexReplace` | `eval/function/string.rs` | conservative `input.len() × (replacement.len() + 1)` pre-bound |
| `normalize()` / `toUpper()` / `toLower()` / `IS NORMALIZED` | `eval/function/string/`, `eval/string.rs` | conservative `len × 12` pre-bound (Unicode compatibility decomposition and SpecialCasing expansions can exceed the input several-fold; 12 is the documented upper bound) |
| list/map/pattern comprehension | `eval/mod.rs` | the output buffer before `with_capacity` (map keys/entries incremental) |
| `reduce` / `allReduce` | `eval/quantifier.rs` | the grown accumulator value's size per iteration |
| `collect()` | `exec/aggregate.rs` | the output buffer before `with_capacity` |
| `UNWIND` | `exec/clause_arms.rs` | the row buffer per input row |

**`regexReplace` imprecision.** The replacement allocation happens inside
the `regex` crate and has no interception point. The site charges
`input.len() × (replacement.len() + 1)` before the call — a sound upper
bound when the pattern can match at most once per byte, and deliberately
conservative when it matches less (including not at all). That can refuse a
result that would have fit; it is the documented trade for not intercepting
the allocator.

### 4. The refusal explains itself

`Error::Capacity` gains a `kind` (`CapacityKind::Graph` |
`CapacityKind::Expression`) so the variant and every surface's status
mapping stay unchanged while the expression refusal renders a user-language
line:

> This statement tried to build a single value of 24 GB. This is a safety
> limit protecting against memory exhaustion (not a database limit) — build
> the value in smaller pieces, or raise it with --expression-memory-limit.

What happened (the value, in MB/GB), that it is a protection rather than a
native limit, and the one action — the three facts every memory refusal in
the docs table now carries. The graph-cap kind keeps the historical
`capacity exceeded: used=… limit=…` contract byte-for-byte (ADR-0042,
ADR-0307). Status mapping is untouched: HTTP 507, Bolt
`TransactionOutOfMemoryError`, CLI `ErrorFamily::Capacity`, Neo4j-wire
`MemoryLimitExceeded`.

### 5. PROFILE reports construction work, not memory

The per-statement counter remains as **telemetry only**: every charged
allocation adds to it, and the PROFILE column reports the cumulative bytes
the operator built, never a live-memory measurement (values are dropped as
the statement runs). The metric and column are named
**`Expression Built (Bytes)`** (`ExpressionBuiltBytes` in the wire args,
`expression_built_bytes` in the agent document) so no reader mistakes it
for the row-materialization `Memory (Bytes)` beside it. It is not consulted
for enforcement.

## Alternatives considered

- **A cumulative per-statement total.** Rejected — the user-driven change
  behind this revision. A running total counts values the statement already
  dropped, so it is not a memory measurement; it false-positives long
  streaming statements (the heartbeat fixture built ~120 MiB cumulatively
  while holding a few MiB), and it cannot be made accurate without refunds
  at every drop, which no seam observes.
- **Fixed constant, no knob.** Rejected after the design review: the
  operator must be able to raise the cap for a known bulk pipeline without
  a rebuild, and the refusal message must name that action. Unset keeps the
  generous default; `none` is the explicit opt-out.
- **An `OpenOptions`-only knob.** Rejected: the CLI, `serve`, and embedded
  hosts must all reach the same open path, so the knob rides the existing
  memory-limit ladder (flag → env → `OpenOptions` → default).
- **Reuse ADR-0042's graph cap.** Rejected: it engages at WAL append for
  stored writes and never sees a read-only `range(1, 1_000_000_000)`.
- **Reuse ADR-0395's nested-pass budget.** Rejected: a flat statement
  allocates its result in one pass; passes do not measure bytes. The two
  budgets are orthogonal.
- **Measure allocated heap with an allocator hook.** Rejected: RSS and
  allocator accounting are platform-dependent and not attributable to one
  clause deterministically; the expression layer knows its projected sizes.
- **A new error variant.** Rejected: `Error::Capacity` already maps to
  every surface's controlled memory refusal; `CapacityKind` carries the
  message difference without a second mapping.

## Consequences

- A statement that tries to build one value over the cap fails with the
  plain-language `capacity exceeded` protection message instead of growing
  without bound. Long streaming statements with many small builds are not
  charged against any total and run unchanged; a **retaining** container's
  running total is enforced (it is exact because nothing in it is dropped),
  so a `collect`/comprehension that retains over the cap is refused.
- The default is 256 MiB; the operator can raise it, or pass `none` for
  known bulk pipelines. The refusal itself names the knob, so an agent that
  hits it needs no docs lookup.
- PROFILE gains one column and the plan wire one additive metric key
  (`ExpressionBuiltBytes`); the column is construction-work telemetry, not
  live memory. EXPLAIN trees carry neither.
- The charge is conservative in two places — shared payloads are counted at
  their logical size, and `regexReplace` uses an upper bound — so a value
  near the cap can be refused although its retained heap would be smaller.
- The cap is per allocation, not per query or per transaction: a statement
  may build many under-cap values in sequence.
