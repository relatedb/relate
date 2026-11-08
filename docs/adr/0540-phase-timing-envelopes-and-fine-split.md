# ADR-0540: Fine phase split, plan-cache outcome, and machine-envelope timing keys

Status: Proposed

Claim: ad0c53bce0086ab4bb78fec9b50f5f33d09eb195

## Context

ADR-0491 shipped the always-on PARSE / PLAN / EXECUTE split on the Rust
API, the canonical wire envelope, Bolt and the CLI's human `Time:` line —
and deliberately left the CLI's machine modes and the HTTP Query API v2
envelope without any timing keys (the v2 envelope mirrors the reference
server's shape exactly for Browser compatibility). Three gaps followed:
an API or agent consumer — the envelope modes' primary
audience — cannot see any phase timing; the coarse split conflates stages
the current performance questions ask about (lexing, the rewrite pass,
validation, result generation); and the plan cache (ADR-0503) has no
hit/miss signal on any surface, so "is the plan cache active for this
workload?" requires reading the source.

## Decision

1. **Fine split, extending the existing vocabulary** (not a second one):
   `PhaseTimings` gains `lex_parse` (lexing + AST build), `rewrite` (the
   ADR-0382 `DEFINE`-block resolver pass), `lower` (validation + lowering) — sub-windows of
   PARSE — and `result_generation` — a sub-window of EXECUTE. Two requested
   boundaries stay fused, each for an architectural reason: the lexer and
   the parser share the parser's one tokenizing constructor (timing them
   apart means pre-tokenizing, which runs the tokenizer twice — the lexer
   is ~31–36% of a 1.5–1.9 µs parse, so that tax is real); the validation
   battery runs inside the lowering walk, so a per-pass split restructures
   the front's lowering.
2. **Always-on, decided by measurement**: one clock stamp costs ≤ 33 ns on
   arm64; the four extra stamps are ~130 ns theoretical, and the measured
   point-read p50 (16 µs wall, 12 µs engine, pokec-shaped fixture behind a
   range index, plan-cache path) is unchanged before vs after — the delta
   sits below the p50's resolution. No PROFILE gating, no flag, no
   thread-local arming.
3. **Plan-cache outcome, one enum the admission path already knows**:
   `hit-template` / `hit-raw-fastpath` / `replan-generation-mismatch` /
   `miss` / `bypassed`, beside the timings on every result header. A hit
   rejected for literal-shape safety reports `miss` (no cached plan
   served); its own rejected arm is unobservable — such a statement fails
   its own original compile, so no result carries it.
4. **Envelope keys, one rendering policy per surface family** (the fact is
   one: the `TimingStampMs` projection): the canonical envelope extends
   its flat `_ms` family (MCP, WASM, the C-ABI wire form); the CLI machine
   modes and the HTTP v2 envelope — which had no timing keys — carry one
   nested `timings` object plus the `planCache` outcome beside it, so a
   later phase joins the object and never the top level; Bolt extends its
   Float-seconds family (`lex_parse_time`, …, `result_generation_time`)
   plus `plan_cache`.
5. **HTTP v2 carries them** (reversing ADR-0491's exclusion): the keys sit
   after the Neo4j members, whose reference order is untouched; drivers
   ignore unknown members. The values are per-request, so byte-identity
   comparisons across requests must exclude the timing block.
6. **CLI machine-mode shape change, accepted**: `--mode json` wraps every
   plain run's rows as `{"data": […], "timings": …, "planCache": …}`;
   `--mode jsonl` closes every statement's stream with a keyed timing
   line (a zero-row statement emits exactly that line); `--mode toon`
   renders terminal `timings:` / `planCache:` entries. The strict text
   formats and the human table mode are unchanged — the `.timer` line
   stays the human rendering.

## Alternatives considered

- **PROFILE-gated or `--timings`-flagged fine split** (a phase-clock
  arming pattern, non-profiled statements paying one thread-local read) —
  rejected: the measured cost is below the
  p50 noise floor; gating adds a flag, a thread-local read and per-surface
  presence rules for a signal that is free.
- **Splitting lexer from parser by pre-tokenizing at the admission
  boundary** — rejected: runs the tokenizer twice per statement.
- **Splitting validation from lowering** — rejected: the battery is fused
  into the lowering walk; the split would restructure the front for a
  sub-µs window.
- **A `hit-rejected` outcome variant** — rejected: unreachable on any
  result (the rejected statement always fails its own compile); shipping
  an untestable public enum value is dead vocabulary.
- **Per-surface hand-spelled keys** — rejected: one projection
  (`timing_stamp_ms`) and one nested-object builder; a second spelling per
  surface is the parallel state the codebase bans.

## Consequences

Every machine-readable surface can answer "where did the time go, and did
the plan cache serve it" from the result alone. `--mode json` consumers
that assumed the bare array must read `data` (the PROFILE wrapper's
shape); jsonl consumers see one extra keyed line per statement. The FFI
per-field accessors and the Python / Swift / JS / Go client attributes
gain the fine split and the outcome in a follow-up change (the wire
envelope they can consume already carries it); the coarse trio they
expose today is unchanged. CI's per-PR query-latency lane (ADR-0245
bands) guards the real suite against any hidden cost the local fixture
could not resolve.
