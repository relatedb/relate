# ADR-0468: Structure-of-arrays batch substrate for clause interiors

Status: Proposed

Claim: 67925094d026b75e63fe6cd64445de611154ec76

Later references: ADR-0555 bounds how many rows may be alive between operators: the row-at-a-time boundary this ADR takes as given becomes a stream of rows or bounded batches, and unbounded results are never accumulated.

## Context

The executor's clause interiors are row-at-a-time. `Row`
(`cypherlite-executor/src/eval/row.rs`) is one shared name schema plus a
`Vec<Value>` per row; operators walk expression trees per row, clone rows
per binding, and re-dispatch on `Value` variants element by element. The
speed-of-light experiment (2026-10-31 session; kernels in the uncommitted
`sol.rs` artifacts) measured what that machinery costs independent of plan
choice and store access: **q8 runs ~2.5 ms in the engine against 0.040 ms
for a hand-written loop with the same plan and the same 1.7k store reads —
a 61x gap that lives entirely in per-row machinery** (tree-walking `eval`
with per-row `date()` parse, per-walk `HashSet<RelId>`, `Row` clones).
q19 decomposes the same way (~29x plan choice, addressed by ADR-0351 /
ADR-0218 lineage and LABS-643; ~3.8x fold machinery), and q30's residual
~4x is per-hop bookkeeping. The substrate ticket is **LABS-359**; this
ADR is the decision that ticket implements, layer 1 of the execution
architecture (see also ADR-0351 cheapest-element start selection and
ADR-0218 stats-based match orientation — planner-side decisions that feed
a plan into whichever executor interior runs it).

Two proven per-row substrates already exist and stay: the aggregate fold
(`agg_fold`) and early-exit exists (`exists_probe`). The batch substrate
is the prerequisite for everything above it in the architecture —
scan/predicate specialization (the q8 61x family), contracted traversal
kernels, and SIMD via autovectorization over typed buffers.

Literature anchor: Kersten et al., VLDB 2018 ("Everything You Always
Wanted to Know About Compiled and Vectorized Queries but Were Afraid to
Ask") and the Umbra adaptive-execution line (ICDE 2018) — vectorized and
compiled execution reach near-parity in throughput, and at small data
scale the TUM systems themselves choose interpretation; HyPer's own
compile cost (0.7 ms codegen + 30 ms LLVM) exceeds our worst query
outright.

## Decision

The executor gains a **structure-of-arrays batch substrate** for clause
interiors, with these fixed properties:

- **Chunk type.** A batch ("chunk") of rows is stored column-major: one
  typed buffer per column — `u32` (interned IDs, enum-like keys),
  `i64`, `f64`, utf8 strings, `bool`, plus a **`Value`-fallback column**
  for heterogeneous types (lists, temporal, points, nodes/relationships
  as entities) — and a **null bitmap per column**. A column whose values
  do not fit one typed buffer degrades to the `Value` fallback, never to
  silent coercion. The shape follows `cypherlite-arrow`'s
  CypherValue ↔ Arrow correspondence, so the substrate and the Arrow
  surface agree on one columnar mapping of the value space.
- **Batch size.** Default in the **256–1024** row range, one constant
  owned by the substrate (not per-operator magic numbers). Below ~64 a
  batch collapses toward row-at-a-time overhead; above ~64K it thrashes
  cache (Kersten et al. VLDB 2018). The constant is the tuning knob, not
  a per-plan decision.
- **Execution discipline.** Operators **stream chunks pull-based inside a
  clause**; **materialization happens only at pipeline breakers** —
  clause boundaries, aggregations, sorts, hash builds. `Row` remains the
  boundary representation: **`Row` ↔ chunk conversion is lossless in both
  directions** (every `CypherValue`, including nulls via the bitmap,
  survives a round trip), so breakers and any operator not yet migrated
  interoperate with migrated interiors without behavior change. The
  **formatter boundary is unchanged** — result production materializes
  rows exactly as today; no output format, FFI signature, or client
  surface moves.
- **Scope.** **Clause interiors of scan/filter/project paths first** —
  the q8 family. Per-row traversal walks (expand) are explicitly out of
  scope here: they arrive later as **contracted traversal kernels over
  the substrate, specified by a separate ADR**. Planner behavior
  (ADR-0218, ADR-0351) is untouched — the substrate changes how a chosen
  plan executes, not which plan is chosen.

## Alternatives considered

- **JIT / runtime compilation of query plans** (HyPer/Umbra-style
  pipeline compilation, as the pgrust/Gavity analysis considered).
  Rejected: the WASM build forbids runtime codegen outright; MSRV 1.88
  constrains codegen tooling; and at sub-ms query scale the compile cost
  exceeds the whole query — HyPer's own numbers (0.7 ms codegen + 30 ms
  LLVM) dwarf even our worst (~180 ms) query, and Umbra's adaptive data
  shows the TUM systems themselves never choose compilation at our data
  scale (TPC-H SF 0.01–0.1 ≈ small LDBC): at this scale interpretation
  always wins.
- **Per-row callback-style batch API** — operators structured around
  calling a `dyn Fn` per row inside a nominally batched loop. Rejected:
  it recreates exactly the per-row dynamic-dispatch overhead being
  removed; the literature's vectorization win comes from tight loops over
  typed buffers with values in registers, which per-row callbacks
  preclude by construction.
- **Full columnar storage engine** — make the store itself columnar
  everywhere and read columns end to end. Rejected for this decision:
  storage-side segments are a separate, already-charted track
  (LABS-270/280/281/282/284 — sealed+delta adjacency, tiered arena,
  FSST/dict/FOR codecs, compressed-CSR projection). This ADR is
  **executor-internal batching over the existing store**; the two
  compose later, and neither blocks the other.

## Consequences

- The q8-family per-row overhead (per-row `eval` dispatch, `Row` clones,
  per-element `Value` matching) is removable from scan/filter/project
  interiors; tight loops over typed buffers are autovectorizable without
  hand-SIMD.
- `Row` survives as the boundary and fallback representation: breakers,
  the formatter, `agg_fold`, `exists_probe`, and unmigrated operators
  keep working unchanged during a staged migration, and the lossless
  round-trip property is the invariant every conversion site must
  preserve (and test).
- Every Cypher behavior the TCK pins must keep passing (3892 of 3892) —
  batched interiors are an execution representation change, not a
  semantics change; null handling through the bitmaps is the highest-risk
  surface and gets dedicated round-trip tests.
- Dual representation costs real code: column encoders/decoders, the
  fallback column, and conversion at every breaker. The substrate lives
  in `cypherlite-executor` (engine-internal, no new external dependency,
  no R6 review needed).
- The substrate is the prerequisite layer for contracted traversal
  kernels and batch-level zone maps; those arrive as their own ADRs and
  tickets and must not smuggle decisions in through this one.
